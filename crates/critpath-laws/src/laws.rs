//! The rules, each gated on what it needs before it is allowed to speak.
//!
//! Every rule is a [`Law`], so the only way to reach its result is through `ask`, which runs the
//! gate first. A rule can therefore never answer about a trace it was not entitled to read.

use core::marker::PhantomData;
use std::collections::HashMap;

use critpath_core::{Graph, Micros};
use fitkit_core::{Answer, Confidence, Evidence, Refusal};
use fitkit_ledger::{Citation, Law};

use crate::{window, Finding, Observation, CRITICAL_PATH, FORMAT};

/// Category, name, and what the source said the work was done to.
type Subject<'a> = (&'a str, &'a str, &'a str);

/// Whether the machine was busy at any point in a window, answered without rescanning the trace.
///
/// The question "was anything at all running between these two moments" is asked once per wait on
/// the chain, and answering it by looking at every activity makes the cost of a report the product
/// of the chain's length and the trace's size. That is invisible on a trace whose chain is a
/// handful of steps and fatal on one whose chain is most of the recording -- and a busy single
/// thread produces exactly the second kind, where it turns a one-second report into a three-minute
/// one.
///
/// Spans sorted by start, carrying the highest end seen so far. Anything that started before the
/// window closed is a candidate, and the greatest end among those candidates decides the answer,
/// so one binary search replaces the scan. Same predicate, same answer, in logarithmic time.
struct Busy {
    starts: Vec<Micros>,
    highest_end: Vec<Micros>,
}

impl Busy {
    fn of(graph: &Graph) -> Self {
        // Inferred extents are excluded here exactly as they were when this was a scan: a
        // correlation this reader invented is not the machine doing anything.
        let mut spans: Vec<(Micros, Micros)> = graph
            .activities
            .iter()
            .filter(|activity| !activity.inferred && activity.end > activity.start)
            .map(|activity| (activity.start, activity.end))
            .collect();
        spans.sort_unstable();
        let mut starts = Vec::with_capacity(spans.len());
        let mut highest_end = Vec::with_capacity(spans.len());
        let mut highest = Micros::MIN;
        for (start, end) in spans {
            highest = highest.max(end);
            starts.push(start);
            highest_end.push(highest);
        }
        Self { starts, highest_end }
    }

    /// Whether anything was running at any point between `from` and `to`.
    fn during(&self, from: Micros, to: Micros) -> bool {
        let candidates = self.starts.partition_point(|&start| start < to);
        candidates > 0 && self.highest_end[candidates - 1] > from
    }
}

/// Refuse a rule whose claim the holes in this trace could overturn.
///
/// Not one gate but three, because the holes are not alike. An unknown event threatens everything.
/// A missing edge can move work onto the chain, so it threatens only claims about membership. Work
/// still running when the window closed threatens nothing, because it is held open to the end of
/// the window rather than dropped. Refusing every rule on any hole refuses every real trace, and
/// trusting them all reports a chain that may not be the chain; itemising is how both are avoided.
fn intervals_complete(observation: &Observation<'_>) -> Answer<()> {
    if observation.graph.coverage.unread > 0 {
        return Err(Refusal::unreported("events in the trace could not be read at all"));
    }
    if observation.graph.coverage.intervals_are_complete() {
        Ok(())
    } else {
        Err(Refusal::unreported("work in the trace has no known interval"))
    }
}

/// Refuse while a missing or contradicted edge could change what is on the chain.
fn edges_complete(observation: &Observation<'_>) -> Answer<()> {
    if observation.graph.coverage.unread > 0 {
        return Err(Refusal::unreported("events in the trace could not be read at all"));
    }
    if observation.graph.coverage.edges_are_complete() {
        Ok(())
    } else {
        Err(Refusal::unreported("dependencies in the trace could not all be attached"))
    }
}

/// Refuse a chain with nothing on it.
fn has_a_chain(observation: &Observation<'_>) -> Answer<()> {
    if observation.path.steps.is_empty() {
        return Err(Refusal::uninformative("the chain has no steps"));
    }
    Ok(())
}

/// The same work, done twice, on the path that decides the finish.
///
/// Threshold free by construction. It fires on work repeated *on the same subject*, and that is a
/// fact about the trace rather than a judgement about how long something ought to take.
///
/// Judged on the subject and not the name, because a name repeats for two very different reasons.
/// A loop that fetches seventy resources runs the same code seventy times and wastes nothing; a
/// program that fetches one resource twice wasted the second. Only the subject the source recorded
/// tells them apart, so where the source recorded none this rule has nothing to prove and says
/// nothing, which is the difference between this and a profile that ranks names by total time.
#[derive(Debug, Default)]
pub struct RepeatedWork<'a>(PhantomData<&'a ()>);

impl<'a> Law for RepeatedWork<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        FORMAT
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        // Two activities sharing a key on one real chain is a fact about that chain. A missing
        // edge elsewhere cannot unmake it, so this rule survives an incomplete edge set.
        has_a_chain(observation)?;
        if observation.graph.coverage.unread == 0 {
            Ok(())
        } else {
            Err(Refusal::unreported("events in the trace could not be read at all"))
        }
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        let graph = observation.graph;
        // Charged in self time, so a repeated task loop is not mistaken for repeated work. A
        // frame that only encloses other work has no self time, and a claim worth zero is no
        // claim, so such names drop out without the rule ever having to know one.
        let self_time = graph.self_times();
        let on_chain = graph.with_nested(&observation.path.activities().collect::<Vec<_>>());
        let mut groups: Vec<(Subject<'_>, Vec<usize>)> = Vec::new();
        // Kept in first-appearance order so the report is stable, but found by key rather than by
        // scanning what has been found so far. A chain carrying many distinct subjects is the case
        // where the scan costs the square of the chain.
        let mut seen: HashMap<Subject<'_>, usize> = HashMap::new();
        for id in on_chain {
            if self_time[id] == 0 {
                continue;
            }
            let Some(key) = graph.activities[id].identity() else {
                continue;
            };
            if let Some(&at) = seen.get(&key) {
                groups[at].1.push(id);
            } else {
                seen.insert(key, groups.len());
                groups.push((key, vec![id]));
            }
        }
        Ok(groups
            .into_iter()
            .filter(|(_, occurrences)| occurrences.len() > 1)
            .map(|(key, occurrences)| Finding::RepeatedWork {
                key: (key.0.to_owned(), key.1.to_owned(), key.2.to_owned()),
                // Everything after the first occurrence is time the chain spent recomputing what
                // it already had, so the first is the work and the rest are the cost. The span
                // cites the stretch the repeats ran across, and the trust is the weakest of the
                // intervals the cost was summed from, because a total is only as good as the
                // shakiest measurement inside it.
                cost: Evidence::new(
                    window(
                        graph.activities[occurrences[1]].start,
                        occurrences[1..]
                            .iter()
                            .map(|&id| graph.activities[id].end)
                            .max()
                            .unwrap_or_default(),
                    ),
                    occurrences[1..]
                        .iter()
                        .map(|&id| graph.activities[id].confidence)
                        .fold(Confidence::FULL, Confidence::min),
                    occurrences[1..].iter().map(|&id| self_time[id]).sum::<Micros>(),
                ),
                occurrences,
            })
            .collect())
    }
}

/// Time on the chain during which the whole machine was idle.
///
/// Also threshold free: the rule needs no view on how long a wait may be, only on whether anything
/// was running during it.
#[derive(Debug, Default)]
pub struct DeadWait<'a>(PhantomData<&'a ()>);

impl<'a> Law for DeadWait<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        CRITICAL_PATH
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        // Claiming the machine was idle requires knowing what every interval was. Work whose end
        // was never recorded is held open to the window edge, so it is answered for; work whose
        // start was never recorded is not, and neither is an event nobody could read.
        intervals_complete(observation)?;
        has_a_chain(observation)
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        let graph = observation.graph;
        let mut findings = Vec::new();
        let busy = Busy::of(graph);
        for step in &observation.path.steps {
            if step.wait_before == 0 {
                continue;
            }
            let starts = graph.activities[step.activity].start;
            let opened = starts - step.wait_before;
            // Contention is a different problem with a different fix, so the rule only fires when
            // nothing at all was running: an idle machine is always a dependency issued too late.
            // Censored and concurrent work counts, because a network request in flight explains a
            // gap that a late dependency would not. Inferred work does not: its extent was
            // correlated by this reader rather than observed, and more importantly a transfer in
            // flight is not the machine doing anything. Letting it count would let an inference
            // silence the one rule that catches an idle machine.
            if !busy.during(opened, starts) {
                findings.push(Finding::DeadWait {
                    before: step.activity,
                    waited_on: step.waited_on,
                    stated: step.stated,
                    // The gap is the measurement, so the span is the gap exactly rather than a
                    // region enclosing it. Trust comes from the interval that fixes where the gap
                    // ended, since that is the only recorded thing the claim rests on.
                    cost: Evidence::new(
                        window(opened, starts),
                        graph.activities[step.activity].confidence,
                        step.wait_before,
                    ),
                });
            }
        }
        Ok(findings)
    }
}

/// The largest thing in the trace, when it turns out not to matter.
///
/// The finding a ranked profile inverts. Reported so the report can say plainly that the work at
/// the top of the flame graph can be deleted without the finish moving.
#[derive(Debug, Default)]
pub struct OffPath<'a>(PhantomData<&'a ()>);

impl<'a> Law for OffPath<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        CRITICAL_PATH
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        // The whole claim is about membership of the chain, which a missing edge can change.
        intervals_complete(observation)?;
        edges_complete(observation)
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        // Weighed in self time for the same reason the repeat rule is: the widest bar in a trace
        // is usually a frame that contains the chain rather than a rival to it, and calling that
        // off-path would be false. The largest unit of work that is genuinely its own is the one
        // a ranked profile would put at the top and tell you to go and fix.
        let self_time = observation.graph.self_times();
        let Some((longest, &duration)) = self_time
            .iter()
            .enumerate()
            .filter(|&(id, _)| {
                // Only an interval the source measured may be called the largest thing here. An
                // inferred extent is a gap between two marks, and letting one compete for the
                // title would put a correlation this reader invented at the top of the report.
                observation.graph.activities[id].decides()
            })
            .max_by_key(|&(_, cost)| cost)
        else {
            return Ok(Vec::new());
        };
        let on_chain =
            observation.graph.with_nested(&observation.path.activities().collect::<Vec<_>>());
        if duration == 0 || on_chain.contains(&longest) {
            return Ok(Vec::new());
        }
        // How much room it has is the other half of the claim. Saying only that it is off the
        // chain invites the reader to ignore it forever; the backward pass says how much it may
        // grow first, which is a bound rather than a dismissal.
        let room = observation.path.slack_of(longest).map_or(0, |slack| slack.total);
        Ok(vec![Finding::OffPath { activity: longest, duration, room }])
    }
}

/// What was in flight while the chain waited.
///
/// The rule that answers "waiting for what". A chain made mostly of waiting defeats every rule
/// about work, because there is no work to convict: the machine was busy elsewhere, so the idle
/// rule stays silent, and nothing was repeated, so the repetition rule stays silent. The report
/// then states a large number and names nothing, which is the failure mode of every trace tool
/// that only reads what ran.
///
/// Threshold free, and deliberately modest. It measures how much of the chain's waiting each piece
/// of concurrent work overlapped, which is arithmetic on intervals. It does not say the chain was
/// waiting for that work, because a dependency between them was never stated -- and where the
/// trace does state one, the wait already has a subject and this rule is not needed.
///
/// Only concurrent work is weighed. Work that holds a track is ordered by that track, so its
/// relationship to the chain is already decided by the graph; work correlated by identity is the
/// only kind that can be in flight independently of what any thread is doing.
#[derive(Debug, Default)]
pub struct WaitedWhileInFlight<'a>(PhantomData<&'a ()>);

impl<'a> Law for WaitedWhileInFlight<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        CRITICAL_PATH
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        // The claim is arithmetic on two intervals that were both read. A dependency missing
        // elsewhere cannot make an overlap stop having happened, so an incomplete edge set is
        // survivable; an event nobody could read is not, since it might be the work in flight.
        has_a_chain(observation)?;
        if observation.graph.coverage.unread == 0 {
            Ok(())
        } else {
            Err(Refusal::unreported("events in the trace could not be read at all"))
        }
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        let graph = observation.graph;
        let in_flight: Vec<usize> = (0..graph.activities.len())
            .filter(|&id| graph.activities[id].concurrent && graph.activities[id].is_informative())
            .collect();
        let mut covered: Vec<(usize, Micros, usize)> = Vec::new();
        let mut at: HashMap<usize, usize> = HashMap::new();
        for step in &observation.path.steps {
            if step.wait_before == 0 {
                continue;
            }
            let starts = graph.activities[step.activity].start;
            let opened = starts - step.wait_before;
            // One wait has one best explanation, not a list of everything that happened to be
            // open. Weighed by trust rather than by length alone, because the widest candidate is
            // usually the least meaningful one: a correlation that spans the whole recording
            // overlaps every wait completely and would otherwise win all of them. Scaling the
            // overlap by the confidence the interval earned lets a short, well identified transfer
            // outrank a long, badly identified one without this rule holding a cutoff.
            let best = in_flight
                .iter()
                .filter_map(|&id| {
                    let activity = &graph.activities[id];
                    let overlap = activity.end.min(starts) - activity.start.max(opened);
                    (overlap > 0).then(|| (id, overlap, activity.confidence.apply(overlap as f64)))
                })
                .max_by(|a, b| a.2.total_cmp(&b.2).then_with(|| b.0.cmp(&a.0)));
            let Some((id, overlap, _)) = best else {
                continue;
            };
            if let Some(&entry) = at.get(&id) {
                covered[entry].1 += overlap;
                covered[entry].2 += 1;
            } else {
                at.insert(id, covered.len());
                covered.push((id, overlap, 1));
            }
        }
        covered.sort_by_key(|&(id, overlap, _)| (core::cmp::Reverse(overlap), id));
        Ok(covered
            .into_iter()
            .map(|(during, overlap, waits)| Finding::WaitedWhileInFlight { during, overlap, waits })
            .collect())
    }
}

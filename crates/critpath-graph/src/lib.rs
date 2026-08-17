//! The critical path, and the slack behind it.
//!
//! Total time is the longest chain of dependent work, not the largest bar in a sorted list. Work
//! off that chain can be deleted outright without the whole finishing any sooner, which is the one
//! thing a ranked profile cannot express and the reason this crate exists.

use critpath_core::{Activity, ActivityId, EdgeKind, Graph, Micros};
use fitkit_core::{Answer, Margin, Refusal};

mod chain;
mod order;

pub use chain::{Reckoning, Slack};
pub use order::{contradictions, topological};

/// One activity on the path, and the dead time immediately before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The activity that ran.
    pub activity: ActivityId,
    /// Time between the previous step ending and this one starting. Nothing on the chain ran for
    /// it, so it is waiting rather than working.
    pub wait_before: Micros,
    /// The work this step was waiting for, when there was a previous step.
    ///
    /// A wait with no subject is not a finding, it is a number. What ends a wait is the thing the
    /// chain came from, and the graph already holds it, so naming it costs no new evidence.
    pub waited_on: Option<ActivityId>,
    /// Whether the source stated that dependency, rather than it following from track order.
    ///
    /// The difference between two kinds of wait, and the rule needs no threshold to tell them
    /// apart. A stated dependency names what was awaited, so the wait can be attributed. Bare
    /// track order says only that the track was idle, and nothing in the trace says why, so the
    /// honest report is that the wait is unattributed.
    pub stated: bool,
}

/// The longest chain of dependent work, with its cost split into work and waiting.
#[derive(Clone, Debug, PartialEq)]
pub struct CriticalPath {
    /// The chain, in order.
    pub steps: Vec<Step>,
    /// Time spent inside activities on the chain.
    pub work: Micros,
    /// Time on the chain during which nothing on the chain was running.
    pub wait: Micros,
    /// How much shorter the chain must get before a different chain becomes the constraint.
    ///
    /// The point at which optimising this path stops paying. No margin means something else
    /// already finishes just as late, so the next microsecond saved here buys nothing.
    pub margin: Margin,
    /// Room behind every activity in the graph, indexed by [`ActivityId`].
    ///
    /// The half a forward pass cannot supply. An activity off the chain is not simply irrelevant;
    /// it has a stated amount of room before it becomes the constraint, and that turns "ignore
    /// this" into "ignore this until it grows by this much".
    pub slack: Vec<Option<Slack>>,
    /// When the last thing to finish finished, which is what the room is measured against.
    pub finish: Micros,
}

impl CriticalPath {
    /// Elapsed time the chain accounts for.
    pub fn total(&self) -> Micros {
        self.work + self.wait
    }

    /// Whether `activity` lies on the chain.
    pub fn holds(&self, activity: ActivityId) -> bool {
        self.steps.iter().any(|step| step.activity == activity)
    }

    /// The activities on the chain, in order.
    pub fn activities(&self) -> impl Iterator<Item = ActivityId> + '_ {
        self.steps.iter().map(|step| step.activity)
    }

    /// Room behind one activity, if its interval could decide anything.
    pub fn slack_of(&self, activity: ActivityId) -> Option<Slack> {
        self.slack.get(activity).copied().flatten()
    }
}

/// Recover the critical path.
///
/// # Errors
///
/// Refuses rather than guessing when the evidence cannot support a chain: no informative activity,
/// several tracks with no stated flow between them, or dependencies that form a cycle. The middle
/// one carries the weight. Without a flow event, work on separate threads has no recorded order,
/// and ranking those activities by duration would invent a causality the trace never stated.
pub fn critical_path(graph: &Graph) -> Answer<CriticalPath> {
    if !graph.activities.iter().any(Activity::decides) {
        return Err(Refusal::uninformative("no activity in the trace has a usable interval"));
    }
    if graph.tracks().len() > 1 && graph.stated_edges() == 0 {
        return Err(Refusal::unreported(
            "several tracks and no flow events, so no order between them was ever recorded",
        ));
    }
    let order = topological(graph)
        .ok_or_else(|| Refusal::incoherent("dependencies form a cycle, so no chain is longest"))?;
    let reckoning = chain::reckon(graph, &order)
        .ok_or_else(|| Refusal::uninformative("no activity carries a positive interval"))?;
    let walked = &reckoning.chain;

    // Which pairs the source stated itself, so a wait can say whether it has a subject at all.
    // Sorted once and searched, rather than scanned per step, since a real trace states edges in
    // the thousands and the chain is walked in full.
    let mut stated: Vec<(ActivityId, ActivityId)> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Flow)
        .map(|edge| (edge.from, edge.to))
        .collect();
    stated.sort_unstable();

    let mut steps = Vec::with_capacity(walked.len());
    let mut work = 0;
    let mut wait = 0;
    let mut previous: Option<(ActivityId, Micros)> = None;
    for id in walked {
        let activity = &graph.activities[*id];
        let gap = previous.map_or(0, |(_, end)| (activity.start - end).max(0));
        work += activity.duration();
        wait += gap;
        steps.push(Step {
            activity: *id,
            wait_before: gap,
            waited_on: previous.map(|(from, _)| from),
            stated: previous.is_some_and(|(from, _)| stated.binary_search(&(from, *id)).is_ok()),
        });
        previous = Some((*id, activity.end));
    }

    let margin = chain::competitor(&reckoning, walked)
        .map_or(Margin::UNBOUNDED, |slack| Margin::new(slack as f64));
    Ok(CriticalPath { steps, work, wait, margin, slack: reckoning.slack, finish: reckoning.finish })
}

#[cfg(test)]
mod tests {
    use critpath_core::{Activity, Edge, EdgeKind, Graph, Track};
    use fitkit_core::{Confidence, RefusalKind};

    use super::{contradictions as critpath_graph_contradictions, critical_path};

    fn activity(tid: i64, start: i64, end: i64) -> Activity {
        Activity {
            name: format!("a{tid}_{start}"),
            category: String::new(),
            track: Track { pid: 1, tid },
            start,
            end,
            confidence: Confidence::FULL,
            concurrent: false,
            subject: None,
            inferred: false,
        }
    }

    fn graph(activities: Vec<Activity>, edges: &[(usize, usize, EdgeKind)]) -> Graph {
        Graph {
            activities,
            edges: edges.iter().map(|&(from, to, kind)| Edge { from, to, kind }).collect(),
            ..Graph::default()
        }
    }

    fn fat_parallel_trace() -> Graph {
        graph(
            vec![activity(1, 0, 10), activity(1, 10, 20), activity(1, 20, 30), activity(2, 0, 25)],
            &[(0, 1, EdgeKind::Serial), (1, 2, EdgeKind::Serial), (0, 3, EdgeKind::Flow)],
        )
    }

    #[test]
    fn the_chain_beats_the_biggest_bar() {
        // A fat parallel activity, and a thin chain that actually finishes last. A ranked profile
        // would name the fat one; only the chain explains when the work ended.
        let path = critical_path(&fat_parallel_trace()).unwrap();
        assert_eq!(path.steps.len(), 3, "the chain is the three thin activities");
        assert!(!path.holds(3), "the largest activity is not on the chain");
        assert_eq!(path.work, 30);
    }

    #[test]
    fn the_margin_is_the_gap_to_the_next_constraint() {
        let path = critical_path(&fat_parallel_trace()).unwrap();
        assert!(path.margin.survives(4.0), "saving 4 leaves this chain critical");
        assert!(!path.margin.survives(5.0), "saving 5 hands the constraint to the other chain");
    }

    #[test]
    fn several_tracks_with_no_stated_flow_are_refused_rather_than_ranked() {
        let subject = graph(vec![activity(1, 0, 10), activity(2, 0, 99)], &[]);
        assert_eq!(critical_path(&subject).unwrap_err().kind(), RefusalKind::Unreported);
    }

    #[test]
    fn waiting_is_counted_apart_from_working() {
        let subject =
            graph(vec![activity(1, 0, 10), activity(1, 60, 70)], &[(0, 1, EdgeKind::Serial)]);
        let path = critical_path(&subject).unwrap();
        assert_eq!((path.work, path.wait, path.total()), (20, 50, 70));
    }

    #[test]
    fn a_dependency_the_clock_denies_never_reaches_the_chain() {
        // Stated backwards, so the chain is built from the serial order alone and the finish is
        // still explained. The disagreement is reported through coverage, not through the path.
        let subject = graph(
            vec![activity(1, 0, 10), activity(1, 10, 20)],
            &[(0, 1, EdgeKind::Serial), (1, 0, EdgeKind::Flow)],
        );
        let path = critical_path(&subject).unwrap();
        assert_eq!(path.steps.len(), 2);
        assert_eq!(critpath_graph_contradictions(&subject), 1);
    }

    #[test]
    fn a_trace_with_nothing_off_the_chain_bounds_nothing() {
        let subject =
            graph(vec![activity(1, 0, 10), activity(1, 10, 20)], &[(0, 1, EdgeKind::Serial)]);
        assert!(critical_path(&subject).unwrap().margin.is_unbounded());
    }

    #[test]
    fn an_empty_trace_is_refused() {
        assert_eq!(
            critical_path(&Graph::default()).unwrap_err().kind(),
            RefusalKind::Uninformative
        );
    }
}

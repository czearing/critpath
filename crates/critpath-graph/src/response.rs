//! What the product did between a person acting on it and the screen changing.
//!
//! The finish is one question and responsiveness is another, but they are the same shape: a chain
//! of dependent work between two moments the producer stated. So this reuses the chain machinery
//! rather than introducing a second notion of cost, and reports the same working/waiting split.
//! That split is not a coincidence -- Interaction to Next Paint is defined as input delay plus
//! processing plus presentation delay, and delay is waiting while processing is working.
//!
//! The cost of answering for every interaction is the cost of answering for one. The naive design
//! re-walks the graph per interaction, which on a real capture means re-reading a hundred thousand
//! intervals a hundred times, and is exactly why a tool that drives the product to measure it
//! feels slow. Here one reverse sweep over the order the chain already computed gives every
//! interaction its chain at once.

use critpath_core::{ActivityId, Graph, Micros, Phases};

use crate::order::links;

/// One interaction, from the moment it arrived to the moment the screen answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// Which arrival this answers, indexing [`Graph::arrivals`].
    pub arrival: usize,
    /// The interval the producer recorded for handling the arrival.
    pub activity: ActivityId,
    /// When handling began.
    pub began: Micros,
    /// The first moment after that handling ended at which something reached the screen.
    pub presented: Micros,
    /// The chain of dependent work between the two, in order, the handler first.
    pub chain: Vec<ActivityId>,
    /// Time spent inside activities on that chain.
    pub working: Micros,
    /// Time on that chain during which nothing on it was running, the wait for the screen
    /// included.
    pub waiting: Micros,
    /// The split the producer stated for this interaction, when it stated one.
    ///
    /// Present means the whole latency was measured by the producer from the hardware timestamp,
    /// so [`Response::elapsed`] is exact rather than a lower bound.
    pub phases: Option<Phases>,
}

impl Response {
    /// How long the person waited, from the input arriving to the screen changing.
    ///
    /// Exact when the producer stated the interaction, because it then measured from the moment
    /// the input reached the machine. Otherwise measured from the handler starting, which cannot
    /// see the time the input spent queued behind other work, and is a lower bound on what the
    /// person experienced rather than the whole of it.
    pub fn elapsed(&self) -> Micros {
        match self.phases {
            Some(phases) => phases.latency,
            None => (self.presented - self.began).max(0),
        }
    }

    /// Whether the whole wait was measured, rather than only the part after the handler ran.
    pub fn exact(&self) -> bool {
        self.phases.is_some()
    }
}

/// The first stated presentation strictly after a moment, if the recording holds one.
fn after(presentations: &[Micros], moment: Micros) -> Option<Micros> {
    let found = presentations.partition_point(|&at| at <= moment);
    presentations.get(found).copied()
}

/// Where to start decoding an interaction whose producer stated its whole window.
///
/// A stated interaction is recorded as one interval spanning the entire wait, from the input
/// landing to the frame that answered it. That interval is an envelope, not work: it *contains*
/// the handler and everything after it. Chaining forward from an envelope is meaningless, and
/// because a parent and its own child are never a sequence, an envelope has no successors at all
/// -- which is why decoding from it produced a chain of one activity every time and explained
/// nothing.
///
/// So the chain is decoded from the work inside the window instead: the contained activity whose
/// own onward run is longest, which is the first step of the critical path through the
/// interaction. Confined to the window at both ends, so no step of the returned chain can be work
/// that ran after the frame it was supposed to explain.
fn seed(
    graph: &Graph,
    by_start: &[(Micros, ActivityId)],
    tail: &[Micros],
    envelope: ActivityId,
    window: (Micros, Micros),
) -> Option<ActivityId> {
    let (from, to) = window;
    let first = by_start.partition_point(|&(start, _)| start < from);
    let mut best: Option<(Micros, Micros, ActivityId)> = None;
    for &(start, id) in by_start[first..].iter().take_while(|&&(start, _)| start < to) {
        let activity = &graph.activities[id];
        if id == envelope
            || graph.envelopes.binary_search(&id).is_ok()
            || !activity.decides()
            || activity.end > to
            || tail[id] == Micros::MIN
        {
            continue;
        }
        // Longest onward run wins; ties go to whichever ran first, so the same recording always
        // reports the same chain.
        if best.map_or(true, |(seen, at, _)| tail[id] > seen || (tail[id] == seen && start < at)) {
            best = Some((tail[id], start, id));
        }
    }
    best.map(|(_, _, id)| id)
}

/// Decode every interaction's chain, in one sweep over the order the chain already needed.
///
/// The recurrence is the forward one run backwards: the longest run of dependent work *onward*
/// from each activity. What makes it answer for an interaction rather than for the whole recording
/// is that it is confined to a deadline -- an activity may only be extended through a successor
/// that faces the same presentation it does. That confinement is checked by equality against a
/// deadline computed once per activity, not proved by an argument about time, so a chain can never
/// run past the frame it was supposed to explain.
///
/// Empty when the recording states no arrivals or no presentations, which is not a judgement that
/// nothing was slow: it is the same absence [`critpath_core::Asked`] refuses on, kept absent here
/// so the two can never be confused by a caller that skipped the question.
pub fn responses(graph: &Graph, order: &[ActivityId]) -> Vec<Response> {
    // A producer that states an interaction's own end has already proved something reached the
    // screen, so requiring a separately spelled presentation as well would refuse a recording that
    // holds the better evidence.
    let stated = graph.arrivals.iter().any(|arrival| arrival.phases.is_some());
    if graph.arrivals.is_empty() || (graph.presentations.is_empty() && !stated) {
        return Vec::new();
    }
    let count = graph.activities.len();

    // The frame each activity is answerable to: the first one drawn after it finished. Computed
    // for every activity once, so confinement below is an integer comparison rather than a search.
    let deadline: Vec<Option<Micros>> =
        graph.activities.iter().map(|activity| after(&graph.presentations, activity.end)).collect();

    let mut successors: Vec<Vec<ActivityId>> = vec![Vec::new(); count];
    for (from, to) in links(graph) {
        successors[from].push(to);
    }

    // Backward over the dependency order, so every successor is settled before the activity that
    // depends on it. One pass, whatever the number of interactions.
    let mut tail = vec![Micros::MIN; count];
    let mut goes_to: Vec<Option<ActivityId>> = vec![None; count];
    for &id in order.iter().rev() {
        let activity = &graph.activities[id];
        if !activity.decides() || deadline[id].is_none() {
            continue;
        }
        let mut best = (activity.duration(), None);
        for &next in &successors[id] {
            if tail[next] == Micros::MIN || deadline[next] != deadline[id] {
                continue;
            }
            let gap = (graph.activities[next].start - activity.end).max(0);
            let through = activity.duration() + gap + tail[next];
            if through > best.0 {
                best = (through, Some(next));
            }
        }
        tail[id] = best.0;
        goes_to[id] = best.1;
    }

    let mut by_start: Vec<(Micros, ActivityId)> =
        graph.activities.iter().enumerate().map(|(id, a)| (a.start, id)).collect();
    by_start.sort_unstable();

    let mut found: Vec<Response> = graph
        .arrivals
        .iter()
        .enumerate()
        .filter_map(|(arrival, moment)| {
            let activity = moment.activity?;
            let began = graph.activities[activity].start;
            // A stated interaction already knows when it ended, and that moment is the frame that
            // answered this interaction rather than the next frame drawn for any reason. Falling
            // back to the next presentation when the producer has stated the end would report a
            // wait that belongs to something else.
            let presented = match moment.phases {
                Some(phases) => began + phases.latency,
                None => deadline[activity]?,
            };

            let start = moment
                .phases
                .and_then(|_| seed(graph, &by_start, &tail, activity, (began, presented)))
                .unwrap_or(activity);
            let mut chain = vec![start];
            let mut here = start;
            while let Some(next) = goes_to[here] {
                chain.push(next);
                here = next;
            }

            // Working and waiting are accumulated along the chain rather than subtracted from the
            // elapsed time, because dependent work on separate threads may overlap and a
            // subtraction would report negative waiting as if the screen had answered early.
            let mut working = 0;
            let mut waiting = 0;
            let mut reached = began;
            for &step in &chain {
                let step = &graph.activities[step];
                waiting += (step.start - reached).max(0);
                working += step.duration();
                reached = reached.max(step.end);
            }
            waiting += (presented - reached).max(0);

            Some(Response {
                arrival,
                activity,
                began,
                presented,
                chain,
                working,
                waiting,
                phases: moment.phases,
            })
        })
        .collect();

    // Worst first: the question is whether anything was slow, and the answer is the slowest thing.
    // Ties broken by when they happened, so the report is stable across runs of the same trace.
    found.sort_by_key(|response| (-response.elapsed(), response.began));
    found
}

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

use critpath_core::{ActivityId, Graph, Micros};

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
}

impl Response {
    /// How long the person waited, from the handler starting to the screen changing.
    ///
    /// This is the wait that was felt, and it is measured from the handler rather than from the
    /// hardware. A producer that also states when the input physically arrived would let the time
    /// before the handler ran be included; when it does not, that time is unmeasured rather than
    /// zero, and this figure is a lower bound on what the person experienced.
    pub fn elapsed(&self) -> Micros {
        (self.presented - self.began).max(0)
    }
}

/// The first stated presentation strictly after a moment, if the recording holds one.
fn after(presentations: &[Micros], moment: Micros) -> Option<Micros> {
    let found = presentations.partition_point(|&at| at <= moment);
    presentations.get(found).copied()
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
    if graph.arrivals.is_empty() || graph.presentations.is_empty() {
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

    let mut found: Vec<Response> = graph
        .arrivals
        .iter()
        .enumerate()
        .filter_map(|(arrival, moment)| {
            let activity = moment.activity?;
            let presented = deadline[activity]?;
            let began = graph.activities[activity].start;

            let mut chain = vec![activity];
            let mut here = activity;
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

            Some(Response { arrival, activity, began, presented, chain, working, waiting })
        })
        .collect();

    // Worst first: the question is whether anything was slow, and the answer is the slowest thing.
    // Ties broken by when they happened, so the report is stable across runs of the same trace.
    found.sort_by_key(|response| (-response.elapsed(), response.began));
    found
}

//! The chain itself, recovered by dynamic programming over the dependency order.

use critpath_core::{ActivityId, Graph, Micros};

use crate::order::links;

/// Dead time between one activity ending and the next beginning. Never negative.
fn wait(graph: &Graph, from: ActivityId, to: ActivityId) -> Micros {
    (graph.activities[to].start - graph.activities[from].end).max(0)
}

/// The chain of dependent work that ends with the last thing to finish.
///
/// Two parts, and they answer different questions. The chain *ends* where the trace says the work
/// ended, which is observed rather than modelled. The chain is *reconstructed* by dynamic
/// programming: the accumulated cost of reaching each activity is the best over its dependencies
/// of their cost plus the wait between them, so the walk back from the end follows evidence at
/// every step instead of following whichever predecessor happens to be largest.
pub fn longest(graph: &Graph, order: &[ActivityId]) -> Option<Vec<ActivityId>> {
    let count = graph.activities.len();
    let mut predecessors: Vec<Vec<ActivityId>> = vec![Vec::new(); count];
    for (from, to) in links(graph) {
        predecessors[to].push(from);
    }

    let mut cost = vec![Micros::MIN; count];
    let mut came_from: Vec<Option<ActivityId>> = vec![None; count];
    for &id in order {
        let activity = &graph.activities[id];
        if !activity.is_informative() {
            continue;
        }
        let mut best = (activity.duration(), None);
        for &previous in &predecessors[id] {
            if cost[previous] == Micros::MIN {
                continue;
            }
            let through = cost[previous] + wait(graph, previous, id) + activity.duration();
            if through > best.0 {
                best = (through, Some(previous));
            }
        }
        cost[id] = best.0;
        came_from[id] = best.1;
    }

    // The last thing to finish is what everything else was waiting for, so it terminates the
    // chain. Cost breaks a tie, because between two activities ending together the one that
    // accumulated more dependent work is the one that explains the elapsed time.
    let mut end = (0..count)
        .filter(|&id| graph.activities[id].is_informative())
        .max_by_key(|&id| (graph.activities[id].end, cost[id]))?;

    let mut chain = vec![end];
    while let Some(previous) = came_from[end] {
        chain.push(previous);
        end = previous;
    }
    chain.reverse();
    Some(chain)
}

/// How much the chain must shorten before something else becomes the constraint.
///
/// The gap between when the chain finishes and when the latest activity off it finishes. Save more
/// than this and the answer changes, which is the honest place to stop optimising. Nothing off the
/// chain at all leaves the margin unbounded, reported by the caller.
pub fn competitor(graph: &Graph, chain: &[ActivityId]) -> Option<Micros> {
    let ends = graph.activities[*chain.last()?].end;
    let rival = (0..graph.activities.len())
        .filter(|id| !chain.contains(id))
        .filter(|&id| graph.activities[id].is_informative())
        .map(|id| graph.activities[id].end)
        .max()?;
    Some((ends - rival).max(0))
}

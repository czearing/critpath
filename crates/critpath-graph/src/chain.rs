//! The chain itself, recovered by dynamic programming over the dependency order.

use critpath_core::{ActivityId, Graph, Micros};

use crate::order::links;

/// Dead time between one activity ending and the next beginning. Never negative.
fn wait(graph: &Graph, from: ActivityId, to: ActivityId) -> Micros {
    (graph.activities[to].start - graph.activities[from].end).max(0)
}

/// How much room one activity has before it starts costing the finish.
///
/// Two numbers, because they answer two different questions and only one of them is about the
/// finish. Both are bounds rather than measurements, and the direction of their error matters more
/// than their size: the recorded dependencies are always a subset of the real ones, and adding a
/// dependency can only take room away, so every figure here is the *most* room there can be.
/// Reporting it as an exact quantity would be the classic scheduling error of a network missing
/// its logic, which reads looser than the work really is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slack {
    /// At most this much later this work could finish before the whole finishes later.
    ///
    /// Zero means it is on a chain that decides the finish.
    pub total: Micros,
    /// At most this much later it could finish before the next dependent work is delayed.
    ///
    /// [`None`] when nothing was recorded as depending on it, in which case there is no next work
    /// to delay and the question has no answer rather than an unbounded one.
    pub free: Option<Micros>,
}

/// The chain, and the room behind every activity in the graph.
#[derive(Clone, Debug)]
pub struct Reckoning {
    /// The chain of dependent work that ends with the last thing to finish.
    pub chain: Vec<ActivityId>,
    /// Room per activity, indexed by [`ActivityId`]. [`None`] where the interval decides nothing.
    pub slack: Vec<Option<Slack>>,
    /// When the last thing to finish finished, which is what the room is measured against.
    pub finish: Micros,
}

/// Recover the chain and the room behind it, in one forward pass and one backward pass.
///
/// The forward pass is the chain: the accumulated cost of reaching each activity is the best over
/// its dependencies of their cost plus the wait between them, so the walk back from the end
/// follows evidence at every step instead of following whichever predecessor happens to be
/// largest. The chain *ends* where the trace says the work ended, which is observed rather than
/// modelled.
///
/// The backward pass is what the forward pass cannot say. Reaching an activity tells you nothing
/// about whether delaying it would matter; only the longest path *onward* from it does. Running
/// the same recurrence in reverse gives that, and the two together place every activity in the
/// graph relative to the finish, so work off the chain stops being a bare "does not matter" and
/// becomes "does not matter yet, and here is by how much".
pub fn reckon(graph: &Graph, order: &[ActivityId]) -> Option<Reckoning> {
    let count = graph.activities.len();
    let mut predecessors: Vec<Vec<ActivityId>> = vec![Vec::new(); count];
    let mut successors: Vec<Vec<ActivityId>> = vec![Vec::new(); count];
    for (from, to) in links(graph) {
        predecessors[to].push(from);
        successors[from].push(to);
    }

    // Forward: the longest run of dependent work ending with each activity, its own time included.
    let mut head = vec![Micros::MIN; count];
    let mut came_from: Vec<Option<ActivityId>> = vec![None; count];
    for &id in order {
        let activity = &graph.activities[id];
        if !activity.is_informative() {
            continue;
        }
        let mut best = (activity.duration(), None);
        for &previous in &predecessors[id] {
            if head[previous] == Micros::MIN {
                continue;
            }
            let through = head[previous] + wait(graph, previous, id) + activity.duration();
            if through > best.0 {
                best = (through, Some(previous));
            }
        }
        head[id] = best.0;
        came_from[id] = best.1;
    }

    // Backward: the latest each activity could have finished without the whole finishing later.
    // Worked in observed clock time rather than in accumulated duration from a floating origin,
    // because this is an account of what happened, not a schedule that may be rearranged. An
    // activity that in fact started late did not have the room its earliest possible start would
    // have given it, and claiming otherwise would report slack nobody can spend.
    let finish = (0..count)
        .filter(|&id| graph.activities[id].is_informative())
        .map(|id| graph.activities[id].end)
        .max()?;
    let mut latest = vec![finish; count];
    for &id in order.iter().rev() {
        if head[id] == Micros::MIN {
            continue;
        }
        let bound = successors[id]
            .iter()
            .filter(|&&next| head[next] != Micros::MIN)
            .map(|&next| latest[next] - graph.activities[next].duration())
            .min();
        if let Some(bound) = bound {
            latest[id] = bound;
        }
    }

    let slack: Vec<Option<Slack>> = (0..count)
        .map(|id| {
            if head[id] == Micros::MIN {
                return None;
            }
            let free = successors[id]
                .iter()
                .filter(|&&next| head[next] != Micros::MIN)
                .map(|&next| wait(graph, id, next))
                .min();
            Some(Slack { total: (latest[id] - graph.activities[id].end).max(0), free })
        })
        .collect();

    // The last thing to finish is what everything else was waiting for, so it terminates the
    // chain. Accumulated cost breaks a tie, because between two activities ending together the one
    // that accumulated more dependent work is the one that explains the elapsed time.
    let mut end = (0..count)
        .filter(|&id| graph.activities[id].is_informative())
        .max_by_key(|&id| (graph.activities[id].end, head[id]))?;

    let mut chain = vec![end];
    while let Some(previous) = came_from[end] {
        chain.push(previous);
        end = previous;
    }
    chain.reverse();
    Some(Reckoning { chain, slack, finish })
}

/// How much the chain must shorten before something else becomes the constraint.
///
/// The smallest room behind anything off the chain. Save more than this and the answer changes,
/// which is the honest place to stop optimising. Nothing off the chain at all leaves the margin
/// unbounded, reported by the caller.
///
/// Derived from the backward pass rather than from finish times, because something finishing
/// earlier does not mean it has room: it may be one step of a rival chain that has none. Only the
/// longest path onward from it settles that, and that is what the backward pass computes.
pub fn competitor(reckoning: &Reckoning, chain: &[ActivityId]) -> Option<Micros> {
    reckoning
        .slack
        .iter()
        .enumerate()
        .filter(|(id, _)| !chain.contains(id))
        .filter_map(|(_, slack)| slack.map(|slack| slack.total))
        .min()
}

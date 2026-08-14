//! Which dependencies count, and in what order they can be walked.

use critpath_core::{ActivityId, Graph};

/// Whether one activity wholly contains the other, so the pair is nesting rather than sequence.
///
/// A parent and its own child must never both add their time to one chain; the child's time is
/// already inside the parent's.
fn nested(graph: &Graph, first: ActivityId, second: ActivityId) -> bool {
    let (a, b) = (&graph.activities[first], &graph.activities[second]);
    (a.start <= b.start && b.end <= a.end) || (b.start <= a.start && a.end <= b.end)
}

/// The dependencies a chain may be built from.
///
/// Filtered to pairs that can carry accumulated time: both sides informative, neither nested in
/// the other, and the dependent side not starting before the thing it waits on. The last filter
/// drops evidence, so whatever it drops is counted by [`contradictions`] and reported as a hole
/// rather than quietly forgotten.
pub fn links(graph: &Graph) -> Vec<(ActivityId, ActivityId)> {
    graph
        .edges
        .iter()
        .filter(|edge| usable(graph, edge.from, edge.to))
        .filter(|edge| graph.activities[edge.from].start <= graph.activities[edge.to].start)
        .map(|edge| (edge.from, edge.to))
        .collect()
}

/// Whether a stated pair is one a chain could ever be built from, ignoring direction in time.
fn usable(graph: &Graph, from: ActivityId, to: ActivityId) -> bool {
    from != to
        && graph.activities.get(from).is_some_and(critpath_core::Activity::is_informative)
        && graph.activities.get(to).is_some_and(critpath_core::Activity::is_informative)
        && !nested(graph, from, to)
}

/// Dependencies the source stated that its own timestamps deny.
///
/// A dependent that began before the thing it waits on. The trace cannot be believed on both
/// counts, and choosing which half to disbelieve is not something evidence supports, so the
/// disagreement is counted and the findings are gated on it.
pub fn contradictions(graph: &Graph) -> usize {
    graph
        .edges
        .iter()
        .filter(|edge| usable(graph, edge.from, edge.to))
        .filter(|edge| graph.activities[edge.from].start > graph.activities[edge.to].start)
        .count()
}

/// An order in which every dependency precedes its dependent, or `None` when one cannot exist.
///
/// Kahn's algorithm. A cycle is left unordered deliberately: breaking it would require choosing
/// which stated dependency to disbelieve, which no evidence supports. No trace read today can
/// produce one, since [`links`] admits only pairs already ordered in time, and the guard is kept
/// so a future source of edges cannot quietly build a chain out of a cycle.
pub fn topological(graph: &Graph) -> Option<Vec<ActivityId>> {
    let count = graph.activities.len();
    let links = links(graph);
    let mut outgoing: Vec<Vec<ActivityId>> = vec![Vec::new(); count];
    let mut incoming = vec![0_usize; count];
    for &(from, to) in &links {
        outgoing[from].push(to);
        incoming[to] += 1;
    }

    let mut ready: Vec<ActivityId> = (0..count).filter(|&id| incoming[id] == 0).collect();
    let mut order = Vec::with_capacity(count);
    while let Some(id) = ready.pop() {
        order.push(id);
        for &next in &outgoing[id] {
            incoming[next] -= 1;
            if incoming[next] == 0 {
                ready.push(next);
            }
        }
    }
    (order.len() == count).then_some(order)
}

#[cfg(test)]
mod tests {
    use critpath_core::{Activity, Edge, EdgeKind, Graph, Track};
    use fitkit_core::Confidence;

    use super::{contradictions, links, topological};

    fn graph(spans: &[(i64, i64)], edges: &[(usize, usize)]) -> Graph {
        Graph {
            activities: spans
                .iter()
                .map(|&(start, end)| Activity {
                    name: "work".into(),
                    category: String::new(),
                    track: Track { pid: 1, tid: 1 },
                    start,
                    end,
                    confidence: Confidence::FULL,
                })
                .collect(),
            edges: edges
                .iter()
                .map(|&(from, to)| Edge { from, to, kind: EdgeKind::Flow })
                .collect(),
            ..Graph::default()
        }
    }

    #[test]
    fn a_parent_and_its_own_child_are_not_a_sequence() {
        assert!(links(&graph(&[(0, 100), (10, 20)], &[(0, 1)])).is_empty());
    }

    #[test]
    fn a_dependency_the_clock_denies_is_not_believed() {
        // The source says the earlier activity waited on the later one. The chain cannot use it,
        // and the disagreement is counted rather than silently dropped.
        let subject = graph(&[(0, 10), (10, 20)], &[(1, 0)]);
        assert!(links(&subject).is_empty());
        assert_eq!(contradictions(&subject), 1);
    }

    #[test]
    fn a_dependency_the_clock_agrees_with_is_no_contradiction() {
        assert_eq!(contradictions(&graph(&[(0, 10), (10, 20)], &[(0, 1)])), 0);
    }

    #[test]
    fn every_activity_appears_exactly_once_in_the_order() {
        let order = topological(&graph(&[(0, 10), (10, 20), (20, 30)], &[(0, 1), (1, 2)])).unwrap();
        assert_eq!(order.len(), 3);
        let position = |id| order.iter().position(|&each| each == id).unwrap();
        assert!(position(0) < position(1) && position(1) < position(2));
    }
}

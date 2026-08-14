//! Attaching dependencies to the activities they connect.

use critpath_core::{Activity, ActivityId, Edge, EdgeKind, Track};

use super::FlowPoint;

/// The innermost activity on `track` whose interval contains `at`.
///
/// Innermost, because a flow leaves from the work that issued it, not from the frame that happens
/// to enclose that work.
fn enclosing(activities: &[Activity], track: Track, at: i64) -> Option<ActivityId> {
    activities
        .iter()
        .enumerate()
        .filter(|(_, a)| a.track == track && a.start <= at && at <= a.end && a.is_informative())
        .max_by_key(|(_, a)| a.start)
        .map(|(id, _)| id)
}

/// Whether `outer` wholly contains `inner`, so `inner` is nested work rather than the next task.
fn contains(outer: &Activity, inner: &Activity) -> bool {
    outer.track == inner.track
        && outer.start <= inner.start
        && inner.end <= outer.end
        && (outer.start, outer.end) != (inner.start, inner.end)
}

/// Order edges between the top level activities of each track.
///
/// A track executes serially, so one top level activity finishing before the next begins is a real
/// dependency and needs no flow to state it. Nested activities are excluded: they are the parent's
/// own work, not the next thing waiting for it.
pub fn serial(activities: &[Activity]) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut tracks: Vec<Track> = activities.iter().map(|a| a.track).collect();
    tracks.sort_unstable();
    tracks.dedup();

    for track in tracks {
        let mut top: Vec<ActivityId> = activities
            .iter()
            .enumerate()
            .filter(|(_, a)| a.track == track && a.is_informative())
            .filter(|(_, inner)| !activities.iter().any(|outer| contains(outer, inner)))
            .map(|(id, _)| id)
            .collect();
        top.sort_by_key(|&id| (activities[id].start, activities[id].end));
        for pair in top.windows(2) {
            edges.push(Edge { from: pair[0], to: pair[1], kind: EdgeKind::Serial });
        }
    }
    edges
}

/// Flow edges, and the count of endpoints that bound to nothing.
///
/// Endpoints sharing an id are ordered by time and joined consecutively, so a start, any number of
/// steps and an end become a chain without the reader having to know which phase it saw.
pub fn flows(activities: &[Activity], points: &[FlowPoint]) -> (Vec<Edge>, usize) {
    let mut bound: Vec<(&str, i64, ActivityId)> = Vec::new();
    let mut unbound = 0;

    for point in points {
        match enclosing(activities, point.track, point.at) {
            Some(id) => bound.push((point.id.as_str(), point.at, id)),
            // A flow endpoint outside every activity states a dependency whose owner was never
            // recorded. Counting it keeps the hole visible instead of silently losing an edge.
            None => unbound += 1,
        }
    }
    bound.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));

    let mut edges = Vec::new();
    for pair in bound.windows(2) {
        let ((first_id, _, from), (second_id, _, to)) = (pair[0], pair[1]);
        if first_id == second_id && from != to {
            edges.push(Edge { from, to, kind: EdgeKind::Flow });
        }
    }
    (edges, unbound)
}

#[cfg(test)]
mod tests {
    use critpath_core::{Activity, EdgeKind, Track};
    use fitkit_core::Confidence;

    use super::{serial, Edge};

    fn activity(track: i64, start: i64, end: i64) -> Activity {
        Activity {
            name: "work".into(),
            category: String::new(),
            track: Track { pid: 1, tid: track },
            start,
            end,
            confidence: Confidence::FULL,
        }
    }

    #[test]
    fn nested_work_does_not_become_the_next_task() {
        let activities = vec![activity(1, 0, 100), activity(1, 10, 20), activity(1, 100, 150)];
        let edges: Vec<Edge> = serial(&activities);
        assert_eq!(edges.len(), 1, "only the two top level activities are ordered");
        assert_eq!((edges[0].from, edges[0].to, edges[0].kind), (0, 2, EdgeKind::Serial));
    }

    #[test]
    fn separate_tracks_are_never_ordered_against_each_other() {
        let activities = vec![activity(1, 0, 10), activity(2, 20, 30)];
        assert!(serial(&activities).is_empty(), "only a flow may cross tracks");
    }
}

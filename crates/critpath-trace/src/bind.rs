//! Attaching dependencies to the activities they connect.

use critpath_core::{Activity, ActivityId, Edge, EdgeKind, Graph, Track};

use super::{Binding, FlowPoint};

/// Activities on one track that could contain a moment, with the furthest end any earlier activity
/// reaches. The second half is what lets the search stop early.
struct Reach<'a> {
    activities: &'a [Activity],
    ids: &'a [ActivityId],
    furthest: Vec<i64>,
}

impl<'a> Reach<'a> {
    fn new(activities: &'a [Activity], ids: &'a [ActivityId]) -> Self {
        let mut furthest = Vec::with_capacity(ids.len());
        let mut high = i64::MIN;
        for &id in ids {
            high = high.max(activities[id].end);
            furthest.push(high);
        }
        Self { activities, ids, furthest }
    }

    /// The innermost activity whose interval contains `at`.
    ///
    /// Innermost, because a flow leaves from the work that issued it, not from the frame that
    /// happens to enclose that work. The ids are ordered by start, so walking back from the last
    /// one that began in time finds the latest starter first, and the running furthest end proves
    /// when nothing earlier could still reach `at`.
    ///
    /// An instantaneous interval counts. A source is free to record the work a flow leaves from as
    /// having taken no measurable time, and refusing to bind to it discards a dependency the trace
    /// did state, which then reads as a hole the trace does not have. Only work whose own extent
    /// was never observed is excluded, because its end was chosen by this reader rather than
    /// reported, and a dependency must not rest on an interval nobody measured.
    fn enclosing(&self, at: i64) -> Option<ActivityId> {
        let mut index = self.ids.partition_point(|&id| self.activities[id].start <= at);
        while index > 0 {
            index -= 1;
            if self.furthest[index] < at {
                return None;
            }
            let id = self.ids[index];
            let activity = &self.activities[id];
            if activity.start <= at && at <= activity.end && !activity.confidence.is_zero() {
                return Some(id);
            }
        }
        None
    }

    /// The first activity to begin at or after `at`.
    ///
    /// Where an arrival attaches. A flow end marks the moment work became runnable, so the thing
    /// it hands to is the next thing that starts, not whatever happened to be running -- which is
    /// usually nothing, since the point of the handoff is that the machine was free to take it.
    fn next_after(&self, at: i64) -> Option<ActivityId> {
        let start = self.ids.partition_point(|&id| self.activities[id].start < at);
        self.ids[start..].iter().copied().find(|&id| !self.activities[id].confidence.is_zero())
    }
}

/// Order edges between the top level activities of each track.
///
/// A track executes serially, so one top level activity finishing before the next begins is a real
/// dependency and needs no flow to state it. Nested activities are excluded: they are the parent's
/// own work, not the next thing waiting for it. Concurrent work is excluded too, because it is
/// correlated by identity rather than by a call stack, so its position on a track states nothing.
pub fn serial(graph: &Graph) -> Vec<Edge> {
    let activities = &graph.activities;
    let mut edges = Vec::new();
    for (_, ids) in graph.by_track() {
        let mut previous: Option<ActivityId> = None;
        let mut open_until = i64::MIN;
        for id in ids {
            let activity = &activities[id];
            if !activity.is_informative() || activity.concurrent {
                continue;
            }
            // Ordered by start with the longest first, so anything beginning before the current
            // top level activity ends is that activity's own nested work.
            if activity.start < open_until {
                continue;
            }
            open_until = activity.end;
            if let Some(from) = previous {
                edges.push(Edge { from, to: id, kind: EdgeKind::Serial });
            }
            previous = Some(id);
        }
    }
    edges
}

/// Flow edges, and the count of endpoints that bound to nothing.
///
/// Endpoints sharing an id are ordered by time and joined consecutively, so a start, any number of
/// steps and an end become a chain without the reader having to know which phase it saw.
pub fn flows(graph: &Graph, points: &[FlowPoint]) -> (Vec<Edge>, usize) {
    let activities = &graph.activities;
    let tracks = graph.by_track();
    let reach: Vec<(Track, Reach<'_>)> =
        tracks.iter().map(|(track, ids)| (*track, Reach::new(activities, ids))).collect();

    let mut bound: Vec<(&str, i64, ActivityId)> = Vec::new();
    let mut unbound = 0;
    for point in points {
        let found =
            reach.iter().find(|(track, _)| *track == point.track).and_then(
                |(_, reach)| match point.binds {
                    Binding::Enclosing => reach.enclosing(point.at),
                    Binding::Next => reach.next_after(point.at),
                },
            );
        match found {
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
    use critpath_core::{Activity, EdgeKind, Graph, Track};
    use fitkit_core::Confidence;

    use super::serial;

    fn activity(track: i64, start: i64, end: i64, concurrent: bool) -> Activity {
        Activity {
            name: "work".into(),
            category: String::new(),
            track: Track { pid: 1, tid: track },
            start,
            end,
            confidence: Confidence::FULL,
            concurrent,
            subject: None,
        }
    }

    fn graph(activities: Vec<Activity>) -> Graph {
        Graph { activities, ..Graph::default() }
    }

    #[test]
    fn nested_work_does_not_become_the_next_task() {
        let subject = graph(vec![
            activity(1, 0, 100, false),
            activity(1, 10, 20, false),
            activity(1, 100, 150, false),
        ]);
        let edges = serial(&subject);
        assert_eq!(edges.len(), 1, "only the two top level activities are ordered");
        assert_eq!((edges[0].from, edges[0].to, edges[0].kind), (0, 2, EdgeKind::Serial));
    }

    #[test]
    fn separate_tracks_are_never_ordered_against_each_other() {
        let subject = graph(vec![activity(1, 0, 10, false), activity(2, 20, 30, false)]);
        assert!(serial(&subject).is_empty(), "only a flow may cross tracks");
    }

    #[test]
    fn concurrent_work_is_never_ordered_by_where_it_sits() {
        // Async work is correlated by identity and may overlap, so its position on a track is not
        // a dependency. Only a stated flow can put it on a chain.
        let subject = graph(vec![
            activity(1, 0, 10, false),
            activity(1, 10, 20, true),
            activity(1, 20, 30, false),
        ]);
        let edges = serial(&subject);
        assert_eq!(edges.len(), 1);
        assert_eq!((edges[0].from, edges[0].to), (0, 2), "the async activity is stepped over");
    }
}

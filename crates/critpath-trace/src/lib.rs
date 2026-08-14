//! Reading Trace Event Format into a [`Graph`].
//!
//! This format is the reason critpath is language-free. Chrome emits it, and so do Go, `VizTracer`,
//! `PerfMark`, tracing-chrome, `PyTorch`, `TensorFlow` and Bazel, while Perfetto ingests it
//! directly.
//! Its flow events are literal dependency edges, so the graph arrives already drawn by whoever
//! produced the trace rather than guessed at here.

use critpath_core::{Activity, ActivityId, Coverage, EdgeKind, Graph, Micros, Track};
use fitkit_core::Confidence;
use serde_json::Value;

mod bind;
mod read;

pub use read::ParseError;

/// Phases that carry an interval or an edge and are therefore read.
const INTERVAL: [&str; 3] = ["X", "B", "E"];
/// Phases understood to carry neither an interval nor an edge. Skipping them is not a hole.
const IGNORED: [&str; 9] = ["M", "i", "I", "c", "C", "b", "n", "e", "R"];
/// Phases that state a dependency.
const FLOW: [&str; 3] = ["s", "t", "f"];

/// One flow endpoint, before it is bound to the activity enclosing it.
struct FlowPoint {
    id: String,
    track: Track,
    at: Micros,
}

/// Read a trace into a graph.
///
/// # Errors
///
/// [`ParseError`] when the bytes are not JSON, or are JSON of no shape this format defines.
pub fn read(bytes: &[u8]) -> Result<Graph, ParseError> {
    let events = read::events(bytes)?;
    let mut graph = Graph::default();
    let mut open: Vec<(Track, String, String, Micros)> = Vec::new();
    let mut flows: Vec<FlowPoint> = Vec::new();

    for event in &events {
        let Some(phase) = event.get("ph").and_then(Value::as_str) else {
            graph.coverage.unread += 1;
            continue;
        };
        if IGNORED.contains(&phase) {
            continue;
        }
        if FLOW.contains(&phase) {
            match read::flow(event) {
                Some(point) => flows.push(point),
                None => graph.coverage.unread += 1,
            }
            continue;
        }
        if !INTERVAL.contains(&phase) {
            graph.coverage.unread += 1;
            continue;
        }
        read::interval(event, phase, &mut graph, &mut open);
    }

    // A begin with no end states a start time and nothing else. It is a hole, not a zero-length
    // activity, because inventing an end would put a fabricated interval on the critical path.
    graph.coverage.unpaired += open.len();

    graph.edges.extend(bind::serial(&graph.activities));
    let (flow_edges, unbound) = bind::flows(&graph.activities, &flows);
    graph.edges.extend(flow_edges);
    graph.coverage.unbound_flows += unbound;
    graph.edges.sort_unstable_by_key(|e| (e.from, e.to, e.kind == EdgeKind::Serial));
    graph.edges.dedup_by_key(|e| (e.from, e.to));
    Ok(graph)
}

/// Record a completed interval, or open and close a begin/end pair.
fn push(
    graph: &mut Graph,
    track: Track,
    name: String,
    category: String,
    start: Micros,
    end: Micros,
) -> ActivityId {
    // An interval that ends before it starts is a clock fault, not a measurement. It is kept so
    // the count reconciles, and silenced so nothing downstream can decide from it.
    let confidence = if end > start { Confidence::FULL } else { Confidence::ZERO };
    graph.activities.push(Activity { name, category, track, start, end, confidence });
    graph.activities.len() - 1
}

/// Coverage as read so far, for callers that want it before analysis.
pub fn coverage(graph: &Graph) -> Coverage {
    graph.coverage
}

#[cfg(test)]
mod tests {
    use super::read;

    #[test]
    fn a_complete_event_becomes_one_activity() {
        let graph =
            read(br#"[{"ph":"X","name":"eval","cat":"js","pid":1,"tid":1,"ts":0,"dur":50}]"#)
                .unwrap();
        assert_eq!(graph.activities.len(), 1);
        assert_eq!(graph.activities[0].duration(), 50);
        assert!(graph.coverage.is_total());
    }

    #[test]
    fn a_begin_without_an_end_is_a_hole_rather_than_an_activity() {
        let graph = read(br#"[{"ph":"B","name":"eval","pid":1,"tid":1,"ts":0}]"#).unwrap();
        assert!(graph.activities.is_empty());
        assert_eq!(graph.coverage.unpaired, 1);
        assert!(!graph.coverage.is_total());
    }

    #[test]
    fn metadata_and_instants_are_understood_rather_than_counted_as_holes() {
        let graph = read(
            br#"[{"ph":"M","name":"thread_name","pid":1,"tid":1},
                 {"ph":"I","name":"mark","pid":1,"tid":1,"ts":5}]"#,
        )
        .unwrap();
        assert!(graph.coverage.is_total());
    }

    #[test]
    fn an_unknown_phase_is_counted_rather_than_ignored() {
        let graph = read(br#"[{"ph":"~","name":"mystery","pid":1,"tid":1,"ts":0}]"#).unwrap();
        assert_eq!(graph.coverage.unread, 1);
    }

    #[test]
    fn an_interval_that_ends_before_it_starts_decides_nothing() {
        let graph =
            read(br#"[{"ph":"X","name":"skew","pid":1,"tid":1,"ts":10,"dur":-5}]"#).unwrap();
        assert!(!graph.activities[0].is_informative());
    }

    #[test]
    fn the_wrapping_object_and_the_bare_array_read_alike() {
        let bare = read(br#"[{"ph":"X","name":"a","pid":1,"tid":1,"ts":0,"dur":1}]"#).unwrap();
        let wrapped =
            read(br#"{"traceEvents":[{"ph":"X","name":"a","pid":1,"tid":1,"ts":0,"dur":1}]}"#)
                .unwrap();
        assert_eq!(bare.activities, wrapped.activities);
    }
}

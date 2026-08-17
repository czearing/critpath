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
mod correlate;
mod read;

pub use read::ParseError;

/// Phases that carry an interval on a call stack and are therefore read.
const INTERVAL: [&str; 3] = ["X", "B", "E"];
/// Phases that carry an interval correlated by identity, which may overlap and cross threads.
const ASYNC: [&str; 2] = ["b", "e"];
/// Phases that mark a moment rather than an interval.
const INSTANT: [&str; 2] = ["I", "i"];
/// Phases understood to carry neither an interval nor an edge. Skipping them is not a hole.
const IGNORED: [&str; 5] = ["M", "c", "C", "n", "R"];
/// Phases that state a dependency.
const FLOW: [&str; 3] = ["s", "t", "f"];

/// Which activity a flow endpoint attaches to.
///
/// The two ends of a flow do not attach by the same rule, and reading them alike is what makes a
/// trace look like it is missing dependencies it actually stated. A flow *leaves* from work that is
/// running, so its start attaches to the interval containing the instant. A flow *arrives* at work
/// that has not begun yet -- an arrival is the moment something becomes runnable, so by
/// construction nothing is usually running -- and it attaches to the next interval to begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    /// The interval containing the instant.
    Enclosing,
    /// The next interval to begin at or after the instant.
    Next,
}

/// One flow endpoint, before it is bound to an activity.
struct FlowPoint {
    id: String,
    track: Track,
    at: Micros,
    binds: Binding,
}

/// Work opened and not yet closed.
struct Open {
    track: Track,
    name: String,
    category: String,
    start: Micros,
    subject: Option<String>,
}

/// Read a trace into a graph.
///
/// # Errors
///
/// [`ParseError`] when the bytes are not JSON, or are JSON of no shape this format defines.
pub fn read(bytes: &[u8]) -> Result<Graph, ParseError> {
    let events = read::events(bytes)?;
    let mut graph = Graph::default();
    let mut open: Vec<Open> = Vec::new();
    let mut open_async: Vec<(String, Open)> = Vec::new();
    let mut flows: Vec<FlowPoint> = Vec::new();
    let mut marks: Vec<correlate::Mark> = Vec::new();
    let mut window_closes = Micros::MIN;
    let mut window_opens = Micros::MAX;

    for event in &events {
        let Some(phase) = event.get("ph").and_then(Value::as_str) else {
            graph.coverage.unread += 1;
            continue;
        };
        if let Some(ts) = read::at(event) {
            let dur = event.get("dur").and_then(Value::as_i64).unwrap_or(0).max(0);
            window_closes = window_closes.max(ts.saturating_add(dur));
        }
        if IGNORED.contains(&phase) {
            continue;
        }
        // The recording opens at the first event that carries work, not at the first event in the
        // file. Metadata is conventionally stamped at zero, and letting it set the start would
        // stretch the window across the machine's whole uptime and make every span look
        // negligible against it.
        if let Some(ts) = read::at(event) {
            window_opens = window_opens.min(ts);
        }
        if INSTANT.contains(&phase) {
            match read::mark(event) {
                Some(mark) => marks.push(mark),
                None => graph.coverage.unread += 1,
            }
            continue;
        }
        if FLOW.contains(&phase) {
            match read::flow(event, phase) {
                Some(point) => flows.push(point),
                None => graph.coverage.unread += 1,
            }
            continue;
        }
        if ASYNC.contains(&phase) {
            read::asynchronous(event, phase, &mut graph, &mut open_async);
            continue;
        }
        if !INTERVAL.contains(&phase) {
            graph.coverage.unread += 1;
            continue;
        }
        read::interval(event, phase, &mut graph, &mut open);
    }

    // Work still running when the trace stopped is censored, not missing. Holding it open to the
    // end of the window claims the least the evidence allows: it certainly ran that long, and it
    // may have run longer. Zero confidence keeps it off every chain, while its interval still
    // answers a rule that wants to know whether the machine was idle.
    for held in open.into_iter().chain(open_async.into_iter().map(|(_, held)| held)) {
        push(&mut graph, held, window_closes, Confidence::ZERO, false);
        graph.coverage.censored += 1;
    }

    // Marks become intervals only after every interval is known, because a group's trustworthiness
    // is judged against the extent of the recording, which is not known until the end of it.
    correlate::intervals(&mut graph, &marks, window_opens, window_closes);

    graph.edges.extend(bind::serial(&graph));
    let (flow_edges, unbound) = bind::flows(&graph, &flows);
    graph.edges.extend(flow_edges);
    graph.coverage.unbound_flows += unbound;
    graph.edges.sort_unstable_by_key(|e| (e.from, e.to, e.kind == EdgeKind::Serial));
    graph.edges.dedup_by_key(|e| (e.from, e.to));
    Ok(graph)
}

/// Record one interval.
fn push(
    graph: &mut Graph,
    held: Open,
    end: Micros,
    confidence: Confidence,
    concurrent: bool,
) -> ActivityId {
    // An interval that ends before it starts is a clock fault, not a measurement. It is kept so
    // the count reconciles, and silenced so nothing downstream can decide from it.
    let confidence = if end > held.start { confidence } else { Confidence::ZERO };
    graph.activities.push(Activity {
        name: held.name,
        category: held.category,
        track: held.track,
        start: held.start,
        end,
        confidence,
        concurrent,
        subject: held.subject,
        inferred: false,
    });
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
    fn a_begin_without_an_end_is_censored_at_the_window_rather_than_dropped() {
        // The recording stopped while this was running. Dropping it would understate the machine's
        // work and let an idle-gap rule fire over work that was in fact in flight; inventing an end
        // would overstate it. Holding it to the last timestamp seen claims the least the evidence
        // allows, and marking it censored keeps it out of any chain.
        let graph = read(
            br#"[{"ph":"B","name":"eval","pid":1,"tid":1,"ts":0},
                 {"ph":"X","name":"tick","pid":1,"tid":1,"ts":10,"dur":5}]"#,
        )
        .unwrap();
        let held = graph.activities.iter().find(|a| a.name == "eval").unwrap();
        assert_eq!(held.end, 15, "held open to the end of the recording, not beyond");
        assert!(!held.is_informative(), "censored work can decide nothing");
        assert!(held.overlaps(0, 15), "but the machine was still busy with it");
        assert_eq!(graph.coverage.censored, 1);
        assert_eq!(graph.coverage.unpaired, 0, "a cut recording is not a missing event");
        assert!(graph.coverage.is_total(), "every real capture ends mid-flight");
    }

    #[test]
    fn an_end_with_nothing_open_is_a_hole() {
        let graph = read(br#"[{"ph":"E","pid":1,"tid":1,"ts":9}]"#).unwrap();
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

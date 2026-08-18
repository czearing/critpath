//! Reading Trace Event Format into a [`Graph`].
//!
//! This format is the reason critpath is language-free. Chrome emits it, and so do Go, `VizTracer`,
//! `PerfMark`, tracing-chrome, `PyTorch`, `TensorFlow` and Bazel, while Perfetto ingests it
//! directly.
//! Its flow events are literal dependency edges, so the graph arrives already drawn by whoever
//! produced the trace rather than guessed at here.

use std::collections::HashMap;

use critpath_core::{
    Activity, ActivityId, Arrival, Coverage, EdgeKind, Graph, Micros, Phases, Track,
};
use fitkit_core::Confidence;
use serde_json::Value;

mod bind;
mod correlate;
mod read;
mod vocabulary;

pub use read::ParseError;
pub use vocabulary::{Stated, Vocabulary};

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
    read_as(bytes, Vocabulary::default())
}

/// Read a trace into a graph, using one producer's spelling for arrivals and presentations.
///
/// # Errors
///
/// [`ParseError`] when the bytes are not JSON, or are JSON of no shape this format defines.
pub fn read_as(bytes: &[u8], vocabulary: Vocabulary) -> Result<Graph, ParseError> {
    let events = read::events(bytes)?;
    let mut graph = Graph::default();
    let mut open: Vec<Open> = Vec::new();
    let mut open_async: Vec<(String, Open)> = Vec::new();
    let mut flows: Vec<FlowPoint> = Vec::new();
    let mut marks: Vec<correlate::Mark> = Vec::new();
    let mut window_closes = Micros::MIN;
    let mut window_opens = Micros::MAX;
    let mut origins: Vec<(String, usize)> = Vec::new();

    for event in &events {
        let Some(phase) = event.get("ph").and_then(Value::as_str) else {
            graph.coverage.unread += 1;
            continue;
        };
        if let Some(ts) = read::at(event) {
            let dur = event.get("dur").and_then(Value::as_i64).unwrap_or(0).max(0);
            window_closes = window_closes.max(ts.saturating_add(dur));
        }
        // The census runs inside the pass that already visits every event, so asking what a
        // recording contains costs nothing and can never be skipped for speed. It is taken BEFORE
        // the ignored-phase check on purpose: a presentation is a mark, marks carry no work, and
        // so the only place that evidence exists is in an event the graph itself discards.
        census(event, vocabulary, &mut graph, &mut origins);
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
    origins.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    graph.recording.origins = origins;
    graph.presentations.sort_unstable();
    fuse_arrivals(&mut graph);
    bind_arrivals(&mut graph, vocabulary);
    state_presentations(&mut graph);
    Ok(graph)
}

/// Count what this event proves the recording contains.
///
/// Three facts, none of them a judgement: whether a person did something, whether anything reached
/// the screen, and which origins are named. All three are read from what the producer already
/// wrote; none is inferred, and none is compared against a cutoff.
fn census(
    event: &Value,
    vocabulary: Vocabulary,
    graph: &mut critpath_core::Graph,
    origins: &mut Vec<(String, usize)>,
) {
    let name = event.get("name").and_then(Value::as_str).unwrap_or_default();
    if vocabulary.is_presentation(name) {
        graph.recording.presentations += 1;
        if let Some(at) = read::at(event) {
            graph.presentations.push(at);
        }
    }
    if vocabulary.is_stimulus(name) {
        let data = event.get("args").and_then(|args| args.get("data"));
        let kind = data
            .and_then(|data| data.get(vocabulary.stimulus_kind))
            .and_then(Value::as_str)
            .unwrap_or_default();
        // A producer emits one event name for every dispatch, so a page finishing loading and a
        // person clicking arrive spelled alike. Only the kind separates them, and counting
        // dispatches rather than kinds is what would let an idle recording claim it held
        // interactions.
        if vocabulary.is_from_a_person(kind) {
            graph.recording.stimuli += 1;
            if let Some(at) = read::at(event) {
                let (interaction, phases) = match (vocabulary.stated, data) {
                    (Some(stated), Some(data)) => (identity(&stated, data), phases(&stated, data)),
                    _ => (None, None),
                };
                graph.arrivals.push(Arrival {
                    at,
                    kind: kind.to_owned(),
                    activity: None,
                    interaction,
                    phases,
                });
            }
        }
    }
    if let Some(args) = event.get("args") {
        note_origins(args, origins, 0);
    }
}

/// The producer's own identity for the interaction an arrival belongs to.
fn identity(stated: &Stated, data: &Value) -> Option<i64> {
    data.get(stated.identity).and_then(Value::as_i64)
}

/// The split the producer stated, in microseconds.
///
/// Read as differences within one event, never as absolute moments, because the producer states
/// these in its own page clock while the trace is stamped in the machine's. A difference is the
/// same number in both clocks; an absolute value is not, and mixing them is how a tool reports a
/// wait that never happened.
fn phases(stated: &Stated, data: &Value) -> Option<Phases> {
    let field = |name: &str| data.get(name).and_then(Value::as_f64);
    let (began, start, end, latency) = (
        field(stated.began)?,
        field(stated.processing_start)?,
        field(stated.processing_end)?,
        field(stated.latency)?,
    );
    // Rounded and clamped before the cast, so the conversion is exact for every value a producer
    // can state and a nonsense one saturates instead of wrapping into a plausible-looking figure.
    #[allow(clippy::cast_possible_truncation)]
    let micros = |ms: f64| {
        let scaled = (ms * 1000.0).round();
        if scaled.is_finite() {
            scaled.clamp(Micros::MIN as f64, Micros::MAX as f64) as Micros
        } else {
            0
        }
    };
    Some(Phases {
        input_delay: micros(start - began).max(0),
        processing: micros(end - start).max(0),
        presentation_delay: micros(latency - (end - began)).max(0),
        latency: micros(latency).max(0),
    })
}

/// Collapse the several events of one physical interaction into the one the producer measured.
///
/// One press of one finger emits a pointerdown, a pointerup and a click, and a producer that
/// groups them says which group each belongs to. Reporting them separately would say a person
/// interacted three times and would report one wait three times over, which is not a rounding
/// error but a wrong answer to "how many things were slow".
///
/// Two rules, both stated by the producer rather than chosen here. An arrival the producer marks
/// as belonging to no interaction is not one. Among a group, the member stating the greatest
/// latency is the one kept, because that is the latency the person waited and the shorter members
/// are the same wait measured from a later moment.
///
/// Applied only when this recording actually holds stated groupings. A producer that states none
/// is left exactly as it was, so a recording read through a vocabulary without them is unaffected.
fn fuse_arrivals(graph: &mut critpath_core::Graph) {
    if !graph.arrivals.iter().any(|arrival| arrival.interaction.is_some()) {
        return;
    }
    let mut best: HashMap<i64, usize> = HashMap::new();
    let mut keep = vec![false; graph.arrivals.len()];
    for (index, arrival) in graph.arrivals.iter().enumerate() {
        // Unstated arrivals are the same gestures seen through the producer's other spelling for
        // them. Keeping them beside the stated ones would double every interaction.
        let Some(identity) = arrival.interaction else { continue };
        if identity == Stated::NO_INTERACTION {
            continue;
        }
        let latency = arrival.phases.map_or(0, |phases| phases.latency);
        match best.get(&identity) {
            Some(&seen)
                if graph.arrivals[seen].phases.map_or(0, |phases| phases.latency) >= latency => {}
            _ => {
                best.insert(identity, index);
            }
        }
    }
    for &index in best.values() {
        keep[index] = true;
    }
    graph.recording.stated_interactions = best.len();
    let mut index = 0;
    graph.arrivals.retain(|_| {
        index += 1;
        keep[index - 1]
    });
    graph.arrivals.sort_by_key(|arrival| arrival.at);
}

/// Take the end of every stated interaction as a moment something reached the screen.
///
/// It is one, and stated more directly than any separately spelled presentation event: the
/// producer measured this interaction to the frame that answered it and wrote that frame's moment
/// down. A real capture held two frame events in twenty-nine megabytes, which timed every
/// interaction to a frame drawn long afterwards for an unrelated reason and overstated one wait by
/// half again. Reading the ends the producer already stated costs nothing and is exact.
fn state_presentations(graph: &mut critpath_core::Graph) {
    let ends: Vec<Micros> = graph
        .arrivals
        .iter()
        .filter_map(|arrival| {
            let activity = arrival.activity?;
            Some(graph.activities[activity].start + arrival.phases?.latency)
        })
        .collect();
    if ends.is_empty() {
        return;
    }
    graph.presentations.extend(ends);
    graph.presentations.sort_unstable();
    graph.presentations.dedup();
}

/// Bind every arrival to the interval the producer recorded for handling it.
///
/// The census sees an arrival as an event; the graph holds it as an interval; nothing in the
/// format links the two, because they are the same record read twice. The producer's own start
/// timestamp is that link, and it is stated rather than inferred, so no confidence is spent
/// recovering it.
///
/// Indexed and searched rather than scanned per arrival. A recording of a person clicking around
/// for a minute holds hundreds of arrivals against hundreds of thousands of intervals, and the
/// scan is the difference between binding them and appearing to hang.
fn bind_arrivals(graph: &mut critpath_core::Graph, vocabulary: Vocabulary) {
    if graph.arrivals.is_empty() {
        return;
    }
    let mut handlers: Vec<(Micros, usize)> = graph
        .activities
        .iter()
        .enumerate()
        .filter(|(_, activity)| vocabulary.is_stimulus(&activity.name))
        .map(|(id, activity)| (activity.start, id))
        .collect();
    handlers.sort_unstable();
    // Every stimulus interval, not merely the ones an arrival bound to. One gesture is recorded
    // several times over, and the siblings of the bound one are envelopes just as much as it is.
    graph.envelopes = graph
        .activities
        .iter()
        .enumerate()
        .filter(|(_, activity)| {
            vocabulary.is_stimulus(&activity.name) || vocabulary.is_envelope(&activity.name)
        })
        .map(|(id, _)| id)
        .collect();
    for arrival in &mut graph.arrivals {
        // The earliest handler stated to begin exactly when the arrival did. Equality only: a
        // nearby interval is not the same record, and pairing by proximity would invent a link the
        // producer never wrote.
        let found = handlers.partition_point(|&(start, _)| start < arrival.at);
        arrival.activity =
            handlers.get(found).filter(|&&(start, _)| start == arrival.at).map(|&(_, id)| id);
    }
}

/// Add every origin named anywhere in this event's arguments.
///
/// Depth-limited because argument trees are producer-defined and a response header block can nest
/// arbitrarily; the origins that matter are stated near the top, and an unbounded walk would make
/// the census cost scale with payload size rather than with event count.
fn note_origins(value: &Value, origins: &mut Vec<(String, usize)>, depth: usize) {
    if depth > 3 {
        return;
    }
    match value {
        Value::String(text) => {
            if let Some(origin) = origin_of(text) {
                match origins.iter_mut().find(|(seen, _)| *seen == origin) {
                    Some((_, count)) => *count += 1,
                    None => origins.push((origin, 1)),
                }
            }
        }
        Value::Object(fields) => {
            for nested in fields.values() {
                note_origins(nested, origins, depth + 1);
            }
        }
        _ => {}
    }
}

/// The scheme-and-host prefix of a URL, without parsing one.
///
/// Deliberately syntactic. Anything shaped `scheme://host` is an origin, which covers http, https
/// and the extension and internal schemes a browser also writes -- the point of the census is to
/// show the operator every origin present, including the ones they will want to exclude.
fn origin_of(text: &str) -> Option<String> {
    let split = text.find("://")?;
    if split == 0 || !text[..split].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    let rest = &text[split + 3..];
    let end = rest.find('/').unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(format!("{}://{}", &text[..split], &rest[..end]))
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

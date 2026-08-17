//! Turning JSON values into intervals and flow endpoints.

use core::fmt;

use critpath_core::{Graph, Micros, Track};
use fitkit_core::Confidence;
use serde_json::Value;

use super::{Binding, FlowPoint, Open};

/// Why a trace could not be read at all.
#[derive(Debug)]
pub enum ParseError {
    /// The bytes were not JSON.
    NotJson(serde_json::Error),
    /// The JSON was neither an array of events nor an object carrying `traceEvents`.
    NotATrace,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJson(error) => write!(f, "not JSON: {error}"),
            Self::NotATrace => f.write_str("no traceEvents array"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The event array, from either shape the format allows.
///
/// # Errors
///
/// [`ParseError`] when the bytes are not JSON, or carry no event array.
pub fn events(bytes: &[u8]) -> Result<Vec<Value>, ParseError> {
    let value: Value = serde_json::from_slice(bytes).map_err(ParseError::NotJson)?;
    match value {
        Value::Array(events) => Ok(events),
        Value::Object(mut object) => match object.remove("traceEvents") {
            Some(Value::Array(events)) => Ok(events),
            _ => Err(ParseError::NotATrace),
        },
        _ => Err(ParseError::NotATrace),
    }
}

/// A numeric field, in microseconds.
///
/// Timestamps are integers in the format but are sometimes written as floats. A fractional
/// microsecond is below the resolution anything here decides on, so it is dropped, and the value
/// is clamped into range first so the cast cannot wrap.
#[allow(clippy::cast_possible_truncation)]
fn number(event: &Value, field: &str) -> Option<Micros> {
    let value = event.get(field)?;
    value.as_i64().or_else(|| {
        value.as_f64().map(|v| v.clamp(Micros::MIN as f64, Micros::MAX as f64) as Micros)
    })
}

fn track(event: &Value) -> Option<Track> {
    Some(Track { pid: number(event, "pid")?, tid: number(event, "tid")? })
}

fn text(event: &Value, field: &str) -> String {
    event.get(field).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// The timestamp an event carries, if it carries one.
pub fn at(event: &Value) -> Option<Micros> {
    number(event, "ts")
}

/// Read one flow endpoint. Its `id` may be a string or a number in the wild.
///
/// The phase decides where the endpoint attaches, and the format is explicit about it. A start
/// leaves from the work containing it. A step is stated to attach to the enclosing work. An end
/// attaches to the next work to begin, unless it carries the binding point that says otherwise.
///
/// `bp` has exactly one legal value, so any other is a statement this reader does not understand
/// rather than one it may quietly reinterpret, and it returns [`None`] to be counted as a hole.
///
/// The reference implementation also flips an end to enclosing when the category names one of two
/// particular browser subsystems. That is deliberately not copied: it is a compatibility shim for
/// one producer, marked in its own source as pending removal, and a reader that branches on
/// category names knows a framework.
pub fn flow(event: &Value, phase: &str) -> Option<FlowPoint> {
    let binds = match phase {
        "s" | "t" => Binding::Enclosing,
        _ => match event.get("bp") {
            None => Binding::Next,
            Some(Value::String(point)) if point == "e" => Binding::Enclosing,
            Some(_) => return None,
        },
    };
    Some(FlowPoint { id: identity(event)?, track: track(event)?, at: number(event, "ts")?, binds })
}

/// The correlating identity of an async or flow event.
fn identity(event: &Value) -> Option<String> {
    match event.get("id").or_else(|| event.get("id2")) {
        Some(Value::String(id)) => Some(id.clone()),
        Some(Value::Number(id)) => Some(id.to_string()),
        Some(Value::Object(scoped)) => scoped.values().next().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// What the event says the work was done to, as one canonical string.
///
/// Deliberately literal. Every argument the source recorded is kept, in sorted order, and two
/// subjects match only when the source said exactly the same thing about both. That is a strict
/// test and it is meant to be: it can miss a real duplicate whose arguments carry a serial number,
/// and that costs a finding, whereas a loose test invents duplicates that were never there and
/// costs the tool its only claim. Which arguments are meaningful differs per producer, so guessing
/// is the one thing this reader must not do.
fn subject(event: &Value) -> Option<String> {
    let Some(Value::Object(args)) = event.get("args") else {
        return None;
    };
    if args.is_empty() {
        return None;
    }
    let mut fields: Vec<String> =
        args.iter().map(|(field, value)| format!("{field}={value}")).collect();
    fields.sort_unstable();
    Some(fields.join("\u{1}"))
}

/// Read one async interval event, opening or closing a pair correlated by identity.
///
/// Async work is keyed by identity rather than by a call stack: it may overlap, and it may cross
/// threads. Pairing is therefore by id and category, oldest open first, and never by position.
pub fn asynchronous(event: &Value, phase: &str, graph: &mut Graph, open: &mut Vec<(String, Open)>) {
    let (Some(track), Some(ts), Some(id)) = (track(event), number(event, "ts"), identity(event))
    else {
        graph.coverage.unread += 1;
        return;
    };
    let category = text(event, "cat");
    let key = format!("{id}\u{1}{category}\u{1}{}", text(event, "name"));
    if phase == "b" {
        open.push((
            key,
            Open { track, name: text(event, "name"), category, start: ts, subject: subject(event) },
        ));
        return;
    }
    match open.iter().position(|(seen, _)| *seen == key) {
        Some(index) => {
            let (_, held) = open.remove(index);
            super::push(graph, held, ts, Confidence::FULL, true);
        }
        // An async end whose begin was never seen names work with no known start.
        None => graph.coverage.unpaired += 1,
    }
}

/// Read one interval event, opening or closing a pair as the phase requires.
pub fn interval(event: &Value, phase: &str, graph: &mut Graph, open: &mut Vec<Open>) {
    let (Some(track), Some(ts)) = (track(event), number(event, "ts")) else {
        graph.coverage.unread += 1;
        return;
    };
    match phase {
        "X" => {
            let duration = number(event, "dur").unwrap_or(0);
            let held = Open {
                track,
                name: text(event, "name"),
                category: text(event, "cat"),
                start: ts,
                subject: subject(event),
            };
            super::push(graph, held, ts + duration, Confidence::FULL, false);
        }
        "B" => open.push(Open {
            track,
            name: text(event, "name"),
            category: text(event, "cat"),
            start: ts,
            subject: subject(event),
        }),
        _ => match open.iter().rposition(|held| held.track == track) {
            Some(index) => {
                let held = open.remove(index);
                super::push(graph, held, ts, Confidence::FULL, false);
            }
            // An end with nothing open names work whose start was never recorded.
            None => graph.coverage.unpaired += 1,
        },
    }
}

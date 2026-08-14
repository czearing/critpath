//! Turning JSON values into intervals and flow endpoints.

use core::fmt;

use critpath_core::{Graph, Micros, Track};
use serde_json::Value;

use super::{push, FlowPoint};

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

/// Read one flow endpoint. Its `id` may be a string or a number in the wild.
pub fn flow(event: &Value) -> Option<FlowPoint> {
    let id = match event.get("id") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => return None,
    };
    Some(FlowPoint { id, track: track(event)?, at: number(event, "ts")? })
}

/// Read one interval event, opening or closing a pair as the phase requires.
pub fn interval(
    event: &Value,
    phase: &str,
    graph: &mut Graph,
    open: &mut Vec<(Track, String, String, Micros)>,
) {
    let (Some(track), Some(ts)) = (track(event), number(event, "ts")) else {
        graph.coverage.unread += 1;
        return;
    };
    match phase {
        "X" => {
            let duration = number(event, "dur").unwrap_or(0);
            push(graph, track, text(event, "name"), text(event, "cat"), ts, ts + duration);
        }
        "B" => open.push((track, text(event, "name"), text(event, "cat"), ts)),
        _ => match open.iter().rposition(|(open_track, ..)| *open_track == track) {
            Some(index) => {
                let (track, name, category, start) = open.remove(index);
                push(graph, track, name, category, start, ts);
            }
            // An end with nothing open names work whose start was never recorded.
            None => graph.coverage.unpaired += 1,
        },
    }
}

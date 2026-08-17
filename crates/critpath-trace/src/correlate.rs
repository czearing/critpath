//! Recovering an interval from the separate moments a producer recorded it as.
//!
//! Some work is never reported as an interval at all. A network transfer, for instance, is emitted
//! as a handful of instants -- request sent, response received, body received, finished -- and the
//! duration exists only as the gap between two of them. A reader that skips instants therefore
//! reports a program as waiting without ever being able to say what for.
//!
//! Correlating them requires an identity, and the format states none for instants: it defines
//! `id` and `id2` for async and flow events, and nothing at all here. So the identity has to be
//! discovered, and discovery is where this could go badly wrong. The rule used is deliberately
//! blind: every field the producer recorded is treated as a candidate identity, no field is ever
//! named in this source, and each resulting group prices its own trustworthiness by how much of
//! the recording it claims. A field that genuinely identifies a thing -- a request id -- yields
//! many short-lived groups. A field that identifies nothing -- a frame pointer, a boolean, a
//! configuration constant -- yields a handful of groups that each span the whole trace, which is
//! the signature of something that was simply always true.
//!
//! What this must never do is state a dependency. A longest-path decode moves along edges, so a
//! correlation invented here and promoted to an edge would serialise work that ran in parallel and
//! could outrun the real chain. These intervals are therefore recorded as concurrent and inferred:
//! they can be measured, named and reported, and they cannot become the answer.

use std::collections::HashMap;

use critpath_core::{Activity, Graph, Micros, Track};
use fitkit_core::Confidence;

/// One moment the producer recorded, before any of them are correlated.
pub struct Mark {
    /// What the format says the mark is visible within: one thread, one process, or the trace.
    pub scope: String,
    /// Where it was recorded.
    pub track: Track,
    /// When it was recorded.
    pub at: Micros,
    /// Category as the source reported it.
    pub category: String,
    /// Every field the source recorded, flattened to leaves.
    pub fields: Vec<(String, String)>,
}

/// Marks sharing one candidate identity.
struct Group {
    field: String,
    track: Track,
    /// The category, while every mark still agrees on it.
    category: Option<String>,
    first: Micros,
    last: Micros,
    marks: usize,
    /// Fields recorded at the two moments that bound the interval.
    ///
    /// The measurement rests on exactly two marks -- the first and the last -- so describing it by
    /// what the producer wrote at those two moments describes the evidence itself, and costs one
    /// lookup per group rather than a comparison against every mark.
    ///
    /// Agreement across every mark is the obvious alternative and it is worse in both directions.
    /// It is quadratic in the fields a producer records, which on a real browser trace doubles the
    /// running time; and it keeps a field only when nothing contradicts it, which sweeps in every
    /// incidental value the middle of a transfer happened to carry, including whole response
    /// header blocks. The two bounding moments carry what identifies the thing -- what was asked
    /// for, and how it ended.
    opened_by: usize,
    closed_by: usize,
}

/// Recover intervals from marks, and add them to the graph.
///
/// `opens` and `closes` bound the recording, and are what each group's span is judged against.
pub fn intervals(graph: &mut Graph, marks: &[Mark], opens: Micros, closes: Micros) {
    let window = closes.saturating_sub(opens).max(0);
    // Keys borrow from the marks rather than being built. A real trace holds a quarter of a
    // million field observations, and formatting a key for each is most of the cost of reading
    // them at all.
    let mut index: HashMap<(&str, &str, &str), usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();

    for (position, mark) in marks.iter().enumerate() {
        for (field, value) in &mark.fields {
            let key = (mark.scope.as_str(), field.as_str(), value.as_str());
            if let Some(&at) = index.get(&key) {
                let group = &mut groups[at];
                if mark.at < group.first {
                    group.first = mark.at;
                    group.opened_by = position;
                }
                if mark.at > group.last {
                    group.last = mark.at;
                    group.closed_by = position;
                }
                group.marks += 1;
                if group.category.as_deref() != Some(mark.category.as_str()) {
                    group.category = None;
                }
            } else {
                index.insert(key, groups.len());
                groups.push(Group {
                    field: field.clone(),
                    track: mark.track,
                    category: Some(mark.category.clone()),
                    first: mark.at,
                    last: mark.at,
                    marks: 1,
                    opened_by: position,
                    closed_by: position,
                });
            }
        }
    }

    for group in groups {
        // One mark is a moment, not an interval: it states when something happened and nothing
        // about how long it took. Two marks are the least that can bound a duration.
        if group.marks < 2 || group.last <= group.first {
            continue;
        }
        // How much of the recording this group claims. A thing that happened occupies part of the
        // trace; a thing that was merely always true occupies all of it, reaches zero confidence
        // and decides nothing, without this reader ever holding a list of field names or a cutoff.
        let claimed = (group.last - group.first) as f64;
        let confidence = if window > 0 {
            Confidence::new(1.0 - claimed / window as f64)
        } else {
            Confidence::ZERO
        };
        let mut described: Vec<(String, String)> = marks[group.opened_by]
            .fields
            .iter()
            .chain(marks[group.closed_by].fields.iter())
            .cloned()
            .collect();
        described.sort_unstable();
        described.dedup();
        let subject = if described.is_empty() {
            None
        } else {
            Some(
                described
                    .into_iter()
                    .map(|(field, value)| format!("{field}={value}"))
                    .collect::<Vec<_>>()
                    .join("\u{1}"),
            )
        };
        graph.activities.push(Activity {
            // Named for the field that identified it, because that is the only name the source
            // gave the thing as a whole; the individual marks each name a moment in its life.
            name: group.field,
            category: group.category.unwrap_or_default(),
            track: group.track,
            start: group.first,
            end: group.last,
            confidence,
            concurrent: true,
            subject,
            inferred: true,
        });
    }
}

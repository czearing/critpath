//! Reading a source map, and nothing else.
//!
//! The format is a lookup table with a compact encoding, not a model of a program: a list of
//! sources, optionally their original text, and a string of base64 VLQ deltas saying which span of
//! generated code came from which position in which source. Everything in this file is the decode
//! of that string. No field is interpreted, no path is judged, and nothing is inferred.

use serde_json::Value;

use crate::package::package_of;
use crate::Located;

/// One mapping: a run of generated columns that came from a position in one source.
#[derive(Clone, Copy, Debug)]
struct Segment {
    /// Column in the generated line this run starts at, counted from zero.
    generated_column: u32,
    /// Index into the map's sources.
    source: u32,
    /// Line in that source, counted from zero.
    line: u32,
    /// Column in that source, counted from zero.
    column: u32,
}

/// A decoded source map, ready to answer positions.
pub struct SourceMap {
    sources: Vec<String>,
    contents: Vec<Option<Vec<String>>>,
    /// Mappings by generated line, each sorted by generated column.
    rows: Vec<Vec<Segment>>,
}

impl SourceMap {
    /// Decode a map from its JSON bytes, or refuse it.
    ///
    /// Refusal rather than a partial read. A map missing its sources or its mappings cannot place
    /// anything, and a resolver holding half a map answers some positions and silently skips
    /// others, which reads exactly like a program with fewer bottlenecks than it has.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let json: Value = serde_json::from_slice(bytes).ok()?;
        let mut map = Self { sources: Vec::new(), contents: Vec::new(), rows: Vec::new() };
        map.absorb(&json, 0, 0)?;
        Some(map)
    }

    /// Take one map, or one section of an index map, offset to where its generated code sits.
    ///
    /// An index map states its parts and where each begins, so a bundler that concatenates
    /// separately compiled chunks still describes the whole file. Flattening them here means every
    /// question downstream is asked of one table, and a map that arrives in sections cannot become
    /// a class of trace this tool silently declines to resolve.
    fn absorb(&mut self, json: &Value, line_offset: u32, column_offset: u32) -> Option<()> {
        if let Some(Value::Array(sections)) = json.get("sections") {
            for section in sections {
                let offset = section.get("offset")?;
                let line = u32::try_from(offset.get("line")?.as_u64()?).ok()?;
                let column = u32::try_from(offset.get("column")?.as_u64()?).ok()?;
                self.absorb(section.get("map")?, line_offset + line, column_offset + column)?;
            }
            return Some(());
        }

        let root = json.get("sourceRoot").and_then(Value::as_str).unwrap_or_default();
        let sources = json.get("sources")?.as_array()?;
        let base = u32::try_from(self.sources.len()).ok()?;
        let embedded = json.get("sourcesContent").and_then(Value::as_array);
        for (index, source) in sources.iter().enumerate() {
            let named = source.as_str().unwrap_or_default();
            self.sources.push(join(root, named));
            // Split once, at load, because a file's text is asked for as many times as it holds
            // findings, and splitting a megabyte of source per question turns resolution from a
            // lookup into the slowest thing the tool does.
            self.contents.push(
                embedded.and_then(|all| all.get(index)).and_then(Value::as_str).map(|text| {
                    text.split('\n').map(|line| line.trim_end_matches('\r').to_owned()).collect()
                }),
            );
        }

        let mappings = json.get("mappings")?.as_str()?;
        self.decode(mappings, base, line_offset, column_offset);
        Some(())
    }

    /// Walk the VLQ stream, turning its deltas back into absolute positions.
    fn decode(&mut self, mappings: &str, base: u32, line_offset: u32, column_offset: u32) {
        let (mut source, mut line, mut column) = (0i64, 0i64, 0i64);
        for (index, group) in mappings.split(';').enumerate() {
            let generated_line = line_offset as usize + index;
            if group.is_empty() {
                continue;
            }
            // Only the first line of a section starts at the section's column offset; every later
            // line begins at column zero of the generated file, as the spec states.
            let shift = if index == 0 { i64::from(column_offset) } else { 0 };
            let mut generated_column = shift;
            let mut row = Vec::new();
            for field in group.split(',') {
                let values = vlq(field);
                let Some(&first) = values.first() else { continue };
                generated_column += first;
                if values.len() >= 4 {
                    source += values[1];
                    line += values[2];
                    column += values[3];
                    if let (Ok(generated_column), Ok(source), Ok(line), Ok(column)) = (
                        u32::try_from(generated_column),
                        u32::try_from(source),
                        u32::try_from(line),
                        u32::try_from(column),
                    ) {
                        row.push(Segment { generated_column, source: base + source, line, column });
                    }
                }
            }
            if row.is_empty() {
                continue;
            }
            // Sorted rather than assumed sorted. The order is conventional, not guaranteed, and a
            // binary search over an unsorted row silently returns the wrong line. Stable, so that
            // where two mappings claim one column the later one still wins.
            row.sort_by_key(|segment| segment.generated_column);
            if self.rows.len() <= generated_line {
                self.rows.resize_with(generated_line + 1, Vec::new);
            }
            self.rows[generated_line].extend(row);
        }
        for row in &mut self.rows {
            row.sort_by_key(|segment| segment.generated_column);
        }
    }

    /// The original position covering a stated one, read `shift` above the map's own numbering.
    ///
    /// The mapping that applies is the last one beginning at or before the column asked about,
    /// because a segment covers generated code until the next segment starts.
    #[must_use]
    pub fn lookup(&self, line: u32, column: u32, shift: u32) -> Option<Located> {
        self.candidates(line, column, shift).into_iter().next()
    }

    /// Every original line the generated code at a stated position can have come from.
    ///
    /// A map is not obliged to record a mapping per token. Several of the maps measured here record
    /// one mapping per generated line, at column zero, so a generated line holding two original
    /// lines -- a doc comment and the declaration it documents, joined by the transform -- is
    /// recorded only as the first of them. Reading the segment alone therefore answers a line above
    /// the one that ran, and answers it confidently.
    ///
    /// What the map does still state is where the *next* generated code came from, and the original
    /// lines between the two are exactly the ones this generated code can have come from. That
    /// bound comes from the map itself, so widening the answer to it invents nothing. The bound is
    /// only taken when the next mapping is in the same source and later in it; anything else is a
    /// jump to unrelated code, and no bound is claimed. The first candidate is always the segment's
    /// own position, so a caller that wants only what the segment said can take it and stop.
    #[must_use]
    pub fn candidates(&self, line: u32, column: u32, shift: u32) -> Vec<Located> {
        let index = line.checked_sub(shift).map_or(usize::MAX, |l| l as usize);
        let Some(row) = self.rows.get(index) else { return Vec::new() };
        let Some(column) = column.checked_sub(shift) else { return Vec::new() };
        let found = row.partition_point(|segment| segment.generated_column <= column);
        let Some(at) = found.checked_sub(1).and_then(|i| row.get(i)) else { return Vec::new() };
        let Some(source) = self.sources.get(at.source as usize) else { return Vec::new() };
        let last = self
            .following(index, found, row)
            .filter(|next| next.source == at.source && next.line > at.line)
            .map_or(at.line, |next| next.line - 1);
        let package = package_of(source).map(ToOwned::to_owned);
        (at.line..=last)
            .map(|line| Located {
                source: source.clone(),
                line: line + 1,
                column: at.column + 1,
                text: self.text_at(at.source, line),
                package: package.clone(),
            })
            .collect()
    }

    /// The mapping that begins after the one in use, in generated order.
    ///
    /// A blank generated line carries no mapping and says nothing about what it came from, so a
    /// row that is empty ends the search rather than being stepped over: skipping it would claim a
    /// bound across code the map never described.
    fn following<'a>(
        &'a self,
        index: usize,
        found: usize,
        row: &'a [Segment],
    ) -> Option<&'a Segment> {
        row.get(found).or_else(|| self.rows.get(index + 1)?.first())
    }

    /// One line of an original source, if the map carried its text.
    fn text_at(&self, source: u32, line: u32) -> Option<String> {
        self.contents
            .get(source as usize)
            .and_then(Option::as_ref)
            .and_then(|lines| lines.get(line as usize))
            .cloned()
    }

    /// Every source the map names, in its own order.
    #[must_use]
    pub fn sources(&self) -> &[String] {
        &self.sources
    }
}

/// Prefix a source with the map's root, without inventing a separator that was not there.
fn join(root: &str, source: &str) -> String {
    if root.is_empty() {
        return source.to_owned();
    }
    if root.ends_with('/') {
        format!("{root}{source}")
    } else {
        format!("{root}/{source}")
    }
}

/// Decode one base64 VLQ field into its signed values.
///
/// Each digit carries five bits and a continuation flag; the assembled value carries its sign in
/// the low bit. A digit outside the alphabet ends the field rather than being skipped, since a
/// corrupt stream must stop producing positions instead of producing shifted ones.
fn vlq(field: &str) -> Vec<i64> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut values = Vec::new();
    let (mut shift, mut accumulated) = (0u32, 0i64);
    for byte in field.bytes() {
        let Some(digit) = ALPHABET.iter().position(|&known| known == byte) else { break };
        let Ok(digit) = i64::try_from(digit) else { break };
        accumulated += (digit & 31) << shift;
        if digit & 32 != 0 {
            shift += 5;
            if shift > 60 {
                break;
            }
            continue;
        }
        let negative = accumulated & 1 == 1;
        accumulated >>= 1;
        values.push(if negative { -accumulated } else { accumulated });
        shift = 0;
        accumulated = 0;
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map of a two-line generated file, written by hand so the expected answer is known.
    ///
    /// `AAAA` is the first mapping of each line: generated column 0, source 0, original line and
    /// column 0. `;` starts the next generated line, and `AACA` on it moves one line down the
    /// original.
    const MAP: &str = r#"{
        "version": 3,
        "sources": ["src/one.ts"],
        "sourcesContent": ["const tick = () => {\n  return 1;\n}\n"],
        "mappings": "AAAA;AACA"
    }"#;

    #[test]
    fn a_position_resolves_to_the_original_line_and_its_text() {
        let map = SourceMap::parse(MAP.as_bytes()).expect("a v3 map must parse");
        let at = map.lookup(1, 1, 1).expect("the first generated line, counted from one");
        assert_eq!(at.source, "src/one.ts");
        assert_eq!(at.line, 1);
        assert_eq!(at.text.as_deref(), Some("const tick = () => {"));
        let next = map.lookup(2, 1, 1).expect("the second generated line");
        assert_eq!(next.line, 2);
        assert_eq!(next.text.as_deref(), Some("  return 1;"));
    }

    #[test]
    fn the_shift_selects_which_line_is_first() {
        let map = SourceMap::parse(MAP.as_bytes()).expect("a v3 map must parse");
        // The same stated position read as though the producer counted from zero lands a line
        // later. This is the failure the corroboration test exists to catch: both answers are real
        // lines of a real file.
        assert_eq!(map.lookup(1, 1, 0).map(|at| at.line), Some(2));
        assert_eq!(
            map.lookup(0, 0, 1),
            None,
            "a position above the first line resolves to nothing"
        );
    }

    /// A generated line holding two original lines: a doc comment and the declaration under it.
    ///
    /// `AAAA` maps generated line one to original line one. `AAEA` maps generated line two to
    /// original line three, so nothing in the map names line two -- exactly the case measured, in
    /// which the transform joined a comment to the declaration it documents.
    const JOINED: &str = r#"{
        "version": 3,
        "sources": ["src/one.ts"],
        "sourcesContent": ["/** Reports the load. */\nfunction reportPageLoad() {\n  return 1;\n}\n"],
        "mappings": "AAAA;AAEA"
    }"#;

    #[test]
    fn a_generated_line_offers_every_original_line_the_map_leaves_room_for() {
        let map = SourceMap::parse(JOINED.as_bytes()).expect("a v3 map must parse");
        let within = map.candidates(1, 1, 1);
        let lines: Vec<u32> = within.iter().map(|at| at.line).collect();
        assert_eq!(lines, [1, 2], "the next mapping is line three, so lines one and two are open");
        assert_eq!(within[1].text.as_deref(), Some("function reportPageLoad() {"));
        assert_eq!(
            map.lookup(1, 1, 1).map(|at| at.line),
            Some(1),
            "the segment still answers first"
        );
    }

    #[test]
    fn no_room_is_claimed_across_a_gap_the_map_says_nothing_about() {
        // A blank generated line carries no mapping, so what the line before it covers is unknown.
        let blank = JOINED.replace(r#""AAAA;AAEA""#, r#""AAAA;;AAEA""#);
        let map = SourceMap::parse(blank.as_bytes()).expect("a v3 map must parse");
        assert_eq!(map.candidates(1, 1, 1).len(), 1, "an empty row ends the bound");
    }

    #[test]
    fn no_room_is_claimed_across_a_jump_to_another_source() {
        let jumped = JOINED
            .replace(r#"["src/one.ts"]"#, r#"["src/one.ts", "src/two.ts"]"#)
            .replace(r#""AAAA;AAEA""#, r#""AAAA;ACEA""#);
        let map = SourceMap::parse(jumped.as_bytes()).expect("a v3 map must parse");
        let within = map.candidates(1, 1, 1);
        assert_eq!(within.len(), 1, "the next mapping is other code and bounds nothing here");
        assert_eq!(within[0].source, "src/one.ts");
    }

    #[test]
    fn a_source_root_is_prefixed_without_inventing_a_separator() {
        assert_eq!(join("", "a/b.ts"), "a/b.ts");
        assert_eq!(join("webpack://app", "a/b.ts"), "webpack://app/a/b.ts");
        assert_eq!(join("webpack://app/", "a/b.ts"), "webpack://app/a/b.ts");
    }

    #[test]
    fn an_index_map_is_flattened_to_one_table() {
        let index = r#"{
            "version": 3,
            "sections": [
              { "offset": { "line": 0, "column": 0 }, "map": {
                  "version": 3, "sources": ["first.ts"],
                  "sourcesContent": ["alpha\n"], "mappings": "AAAA" } },
              { "offset": { "line": 5, "column": 0 }, "map": {
                  "version": 3, "sources": ["second.ts"],
                  "sourcesContent": ["beta\n"], "mappings": "AAAA" } }
            ]
        }"#;
        let map = SourceMap::parse(index.as_bytes()).expect("an index map must parse");
        assert_eq!(map.sources(), ["first.ts", "second.ts"]);
        assert_eq!(map.lookup(1, 1, 1).map(|at| at.source), Some("first.ts".to_owned()));
        // Generated line 6 counted from one is line 5 counted from zero: where the section begins.
        let second = map.lookup(6, 1, 1).expect("the second section must be reachable");
        assert_eq!(second.source, "second.ts");
        assert_eq!(second.text.as_deref(), Some("beta"));
    }

    #[test]
    fn a_map_missing_what_it_needs_is_refused_rather_than_half_read() {
        assert!(SourceMap::parse(b"not json").is_none());
        assert!(SourceMap::parse(br#"{"version":3,"sources":["a.ts"]}"#).is_none(), "no mappings");
        assert!(SourceMap::parse(br#"{"version":3,"mappings":"AAAA"}"#).is_none(), "no sources");
    }

    #[test]
    fn vlq_decodes_the_signed_deltas_the_format_states() {
        assert_eq!(vlq("A"), [0]);
        assert_eq!(vlq("C"), [1]);
        assert_eq!(vlq("D"), [-1]);
        assert_eq!(vlq("AAAA"), [0, 0, 0, 0]);
        // Continuation: 'g' carries the flag, so the value spans two digits.
        assert_eq!(vlq("gB"), [16]);
        assert!(vlq("").is_empty());
    }

    #[test]
    fn a_row_is_searched_in_column_order_even_if_it_arrived_out_of_order() {
        // Two mappings on one generated line, the later column first in the stream. A binary
        // search over the stream order would answer the wrong original line.
        let map = SourceMap::parse(
            br#"{"version":3,"sources":["a.ts"],
                "sourcesContent":["one\ntwo\n"],"mappings":"AAAA,CACA"}"#,
        )
        .expect("must parse");
        assert_eq!(map.lookup(1, 1, 1).map(|at| at.line), Some(1), "column 0 is the first mapping");
        assert_eq!(map.lookup(1, 2, 1).map(|at| at.line), Some(2), "column 1 is the second");
    }
}

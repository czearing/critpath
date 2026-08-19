//! What can be settled about a program's cost from its source alone.
//!
//! # The standard every rule here meets
//!
//! A rule fires on a *proof*, never on a *count*. There is no threshold anywhere in this file: no
//! "more than three", no "longer than N lines", no score. Every predicate is a statement that is
//! either true of the text or not, and the reason it names is the reason a reader would give.
//!
//! That is the difference between this and a linter with a budget. A threshold is a decision
//! somebody made off-camera, and it has to be re-argued for every codebase; a proof does not.
//!
//! # Why the rules are language-agnostic
//!
//! Nothing below mentions React, or JavaScript, or a game engine. A rule is written against the
//! block tree and against [`Grammar`](crate::grammar::Grammar) *data*, so the same rule that finds
//! a per-frame allocation in a render loop finds a per-row allocation in a report. Supporting a
//! new language is a table entry, and every rule in this file applies to it the moment it exists.

use std::collections::{BTreeMap, BTreeSet};

use crate::degree::{At, Growth, Solved};
use crate::grammar::Grammar;
use crate::tree::{File, Kind};

/// What was proven about a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    /// Work grows faster than the data, and part of the growth is not written here.
    HiddenGrowth,
    /// Work grows faster than the data, all of it visible in one routine.
    NestedGrowth,
    /// A routine reaches itself, so no finite growth describes it.
    UnboundedGrowth,
    /// A call inside a repeat whose result cannot differ between iterations.
    InvariantCall,
    /// A value built inside a repeat that does not depend on the repeat.
    InvariantAllocation,
    /// The same call written twice with the same arguments in one block.
    RepeatedCall,
    /// A freshly built value passed out of a repeat, so its identity changes every iteration.
    IdentityChurn,
    /// A name defined, never called, and never made reachable from outside.
    UnreachableDefinition,
    /// A name brought in and never mentioned again.
    UnusedImport,
}

impl Rule {
    /// One line saying what was proven, in the words a reviewer would use.
    #[must_use]
    pub fn says(self) -> &'static str {
        match self {
            Self::HiddenGrowth => {
                "work grows faster than the data, and the reason is not written here"
            }
            Self::NestedGrowth => "work grows faster than the data",
            Self::UnboundedGrowth => "this reaches itself, so nothing bounds how often it runs",
            Self::InvariantCall => "this call cannot differ between iterations",
            Self::InvariantAllocation => {
                "this value is rebuilt every iteration without depending on it"
            }
            Self::RepeatedCall => "this exact call was already made in this block",
            Self::IdentityChurn => "this value is new every iteration, so nothing can recognise it",
            Self::UnreachableDefinition => "nothing calls this and nothing outside can reach it",
            Self::UnusedImport => "this name is brought in and never mentioned again",
        }
    }
}

/// One proven position.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Spot {
    /// Which rule proved it.
    pub rule: Rule,
    /// The file, by index into the sources.
    pub file: usize,
    /// The line to open, counting from one.
    pub line: usize,
    /// The name the position is about.
    pub name: String,
    /// The chain down to the innermost line carrying the cost, where there is one.
    pub chain: Vec<At>,
}

/// Everything proven about `files`.
///
/// Sorted, so two runs over the same sources report in the same order. A report whose order moves
/// cannot be diffed, and a report that cannot be diffed cannot be acted on.
#[must_use]
pub fn check(files: &[File], solved: &Solved) -> Vec<Spot> {
    let mut spots = Vec::new();
    growth(solved, &mut spots);
    for (index, file) in files.iter().enumerate() {
        let Some(grammar) = crate::grammar::for_extension(extension(&file.path)) else { continue };
        within_repeats(index, file, grammar, &mut spots);
        repeated_calls(index, file, &mut spots);
        unused_imports(index, file, grammar, &mut spots);
    }
    unreachable_definitions(files, solved, &mut spots);
    spots.sort();
    spots.dedup();
    spots
}

/// The extension of a path, lowercased, or the empty string.
fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, tail)| tail)
}

/// Growth read straight off the dynamic program.
fn growth(solved: &Solved, spots: &mut Vec<Spot>) {
    for (index, symbol) in solved.symbols.iter().enumerate() {
        match &solved.growth[index] {
            Growth::Unbounded => spots.push(Spot {
                rule: Rule::UnboundedGrowth,
                file: symbol.file,
                line: symbol.line,
                name: symbol.name.clone(),
                chain: Vec::new(),
            }),
            Growth::Bounded { degree, at, through } if *degree > 1 => {
                // Growth is hidden when it is not all written here: the routine reaches its
                // exponent by calling something that already grows. That is the case a reader
                // cannot catch by looking at the routine, which is the case worth naming apart.
                let rule = if through.is_some() { Rule::HiddenGrowth } else { Rule::NestedGrowth };
                spots.push(Spot {
                    rule,
                    file: symbol.file,
                    line: at.map_or(symbol.line, |at| at.line),
                    name: symbol.name.clone(),
                    chain: solved.chain(index),
                });
            }
            Growth::Bounded { .. } => {}
        }
    }
}

/// Everything provable about what a repeat contains.
fn within_repeats(index: usize, file: &File, grammar: &Grammar, spots: &mut Vec<Spot>) {
    for block in &file.blocks {
        if block.kind != Kind::Repeat {
            continue;
        }
        let inside = contained(file, block.id);
        let varying = varying_names(file, block, &inside);
        for call in &file.calls {
            if !inside.contains(&call.within) {
                continue;
            }
            let mentioned = names_in(&call.args);
            let depends =
                mentioned.iter().any(|name| varying.contains(name)) || varying.contains(&call.name);
            if depends {
                // A fresh value handed to something that depends on the repeat still changes
                // identity every iteration, which is a separate cost from being recomputed.
                if starts_fresh(&call.args, grammar) {
                    spots.push(Spot {
                        rule: Rule::IdentityChurn,
                        file: index,
                        line: call.line,
                        name: call.name.clone(),
                        chain: Vec::new(),
                    });
                }
                continue;
            }
            let rule = if grammar.allocates.contains(&call.name.as_str())
                || grammar.allocates.iter().any(|word| call.name.starts_with(word))
                || file
                    .text
                    .get(call.line - 1)
                    .is_some_and(|line| grammar.allocates.iter().any(|word| word_in(line, word)))
            {
                Rule::InvariantAllocation
            } else {
                Rule::InvariantCall
            };
            spots.push(Spot {
                rule,
                file: index,
                line: call.line,
                name: call.name.clone(),
                chain: Vec::new(),
            });
        }
    }
}

/// The same call, with the same arguments, written twice in one block.
///
/// Exact textual equality, not similarity. Two calls that read the same are the same call, and the
/// second cannot return anything the first did not, unless something between them changed what it
/// depends on -- which is why only calls with no arguments in common with an intervening
/// assignment are reported.
fn repeated_calls(index: usize, file: &File, spots: &mut Vec<Spot>) {
    let mut seen: BTreeMap<(usize, String, String), usize> = BTreeMap::new();
    for call in &file.calls {
        let key = (call.within, call.name.clone(), squeeze(&call.args));
        if let Some(&first) = seen.get(&key) {
            if assigned_between(file, first, call.line).is_empty() {
                spots.push(Spot {
                    rule: Rule::RepeatedCall,
                    file: index,
                    line: call.line,
                    name: call.name.clone(),
                    chain: Vec::new(),
                });
            }
        } else {
            seen.insert(key, call.line);
        }
    }
}

/// A name brought in and never mentioned again.
fn unused_imports(index: usize, file: &File, grammar: &Grammar, spots: &mut Vec<Spot>) {
    for (offset, text) in file.text.iter().enumerate() {
        let line = offset + 1;
        if !grammar.imports.iter().any(|word| starts_word(text, word)) {
            continue;
        }
        for name in imported_names(text, grammar) {
            let used = file
                .text
                .iter()
                .enumerate()
                .any(|(other, elsewhere)| other != offset && mentions(elsewhere, &name));
            if !used {
                spots.push(Spot {
                    rule: Rule::UnusedImport,
                    file: index,
                    line,
                    name,
                    chain: Vec::new(),
                });
            }
        }
    }
}

/// A definition nothing calls and nothing outside can reach.
fn unreachable_definitions(files: &[File], solved: &Solved, spots: &mut Vec<Spot>) {
    let mut called: BTreeSet<&str> = BTreeSet::new();
    for file in files {
        for call in &file.calls {
            called.insert(call.name.as_str());
        }
    }
    for symbol in &solved.symbols {
        if called.contains(symbol.name.as_str()) {
            continue;
        }
        let file = &files[symbol.file];
        let Some(grammar) = crate::grammar::for_extension(extension(&file.path)) else { continue };
        let head = &file.blocks[symbol.block].head;
        let line = file.text.get(symbol.line - 1).map_or("", String::as_str);
        let exported = grammar.exports.iter().any(|word| {
            word_in(head, word)
                || word_in(line, word)
                || mentioned_as_export(file, &symbol.name, grammar)
        });
        if exported {
            continue;
        }
        spots.push(Spot {
            rule: Rule::UnreachableDefinition,
            file: symbol.file,
            line: symbol.line,
            name: symbol.name.clone(),
            chain: Vec::new(),
        });
    }
}

/// Whether a name appears on any line that also carries an export word.
fn mentioned_as_export(file: &File, name: &str, grammar: &Grammar) -> bool {
    file.text
        .iter()
        .any(|line| grammar.exports.iter().any(|word| word_in(line, word)) && mentions(line, name))
}

/// Every block inside `root`, including it, without descending into a nested definition.
fn contained(file: &File, root: usize) -> Vec<usize> {
    let mut out = vec![root];
    let mut queue = vec![root];
    while let Some(id) = queue.pop() {
        for &child in &file.blocks[id].children {
            if file.blocks[child].kind == Kind::Define {
                continue;
            }
            out.push(child);
            queue.push(child);
        }
    }
    out
}

/// Names that may differ between two turns of `block`.
///
/// Deliberately generous: everything the repeat's head mentions, everything any enclosing repeat's
/// head mentions, and everything assigned anywhere inside. Over-stating what varies can only
/// suppress a finding, never invent one, which is the direction an engine that claims proof has to
/// err in.
fn varying_names(file: &File, block: &crate::tree::Block, inside: &[usize]) -> BTreeSet<String> {
    let mut varying = BTreeSet::new();
    let mut current = Some(block.id);
    while let Some(id) = current {
        // Only repeats. A parameter list is the enclosing routine's, and a parameter holds the
        // same value for every turn of a loop inside it -- charging it as varying would silence
        // the very findings this rule exists for.
        if file.blocks[id].kind == Kind::Repeat {
            varying.extend(names_in(&file.blocks[id].head));
        }
        current = file.blocks[id].parent;
    }
    // Every repeat inside as well: a name bound by an inner loop differs between turns of the
    // outer one too, and treating it as fixed would report a call that genuinely changes.
    for &id in inside {
        if file.blocks[id].kind == Kind::Repeat {
            varying.extend(names_in(&file.blocks[id].head));
        }
    }
    let first = block.line;
    let last = block.end_line;
    for (offset, line) in file.text.iter().enumerate() {
        let number = offset + 1;
        if number < first || number > last {
            continue;
        }
        varying.extend(assigned_in(line));
    }
    varying
}

/// Names assigned on a line, read as the identifier before a lone `=`.
fn assigned_in(line: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let characters: Vec<char> = line.chars().collect();
    for (at, &character) in characters.iter().enumerate() {
        if character != '=' {
            continue;
        }
        let before = at.checked_sub(1).map(|i| characters[i]);
        let after = characters.get(at + 1).copied();
        // `==`, `!=`, `<=`, `>=`, `=>` and `+=` are not plain assignments, but `+=` still changes
        // the name, so only the comparisons and the arrow are skipped.
        if matches!(before, Some('=' | '!' | '<' | '>')) || matches!(after, Some('=' | '>')) {
            continue;
        }
        let text: String = characters[..at].iter().collect();
        if let Some(name) = names_in(&text).into_iter().next_back() {
            out.insert(name);
        }
    }
    out
}

/// Names assigned strictly between two lines.
fn assigned_between(file: &File, first: usize, second: usize) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for number in (first + 1)..second {
        if let Some(line) = file.text.get(number - 1) {
            out.extend(assigned_in(line));
        }
    }
    out
}

/// Every identifier in a fragment of text.
fn names_in(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .filter(|word| !word.is_empty() && !word.starts_with(|c: char| c.is_ascii_digit()))
        .map(std::borrow::ToOwned::to_owned)
        .collect()
}

/// Whether an argument list begins with something built on the spot.
fn starts_fresh(args: &str, grammar: &Grammar) -> bool {
    let trimmed = args.trim_start();
    grammar.fresh.iter().any(|token| trimmed.starts_with(token))
}

/// Whether `word` stands as a whole word anywhere in `text`.
fn word_in(text: &str, word: &str) -> bool {
    names_in(text).contains(word)
}

/// Whether `text` begins, after any indentation, with `word` as a whole word.
fn starts_word(text: &str, word: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed
        .strip_prefix(word)
        .is_some_and(|rest| rest.chars().next().map_or(true, |c| !c.is_alphanumeric() && c != '_'))
}

/// Whether `text` mentions `name` as a whole word.
fn mentions(text: &str, name: &str) -> bool {
    names_in(text).contains(name)
}

/// The names an import line brings in.
///
/// Everything the line mentions except the language's own words and the module it came from, which
/// is what stands inside the quoted text the reader already hid.
fn imported_names(text: &str, grammar: &Grammar) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (name, followed_by) in tokens(text) {
        // A name with a path separator after it is where something came from, not what came in.
        if followed_by == Some('.') || followed_by == Some(':') {
            continue;
        }
        if grammar.imports.contains(&name.as_str())
            || grammar.not_calls.contains(&name.as_str())
            || name == "from"
            || name == "as"
            || name == "default"
        {
            continue;
        }
        out.push(name);
    }
    out.sort();
    out.dedup();
    out
}

/// Every identifier in a line, each with the first character that is not a space after it.
fn tokens(text: &str) -> Vec<(String, Option<char>)> {
    let characters: Vec<char> = text.chars().collect();
    let is_name = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = Vec::new();
    let mut at = 0;
    while at < characters.len() {
        if !is_name(characters[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < characters.len() && is_name(characters[at]) {
            at += 1;
        }
        let name: String = characters[start..at].iter().collect();
        let next = characters[at..].iter().copied().find(|c| !c.is_whitespace());
        if !name.starts_with(|c: char| c.is_ascii_digit()) {
            out.push((name, next));
        }
    }
    out
}

/// Text with runs of whitespace collapsed, so two calls that differ only in wrapping match.
fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{check, Rule};
    use crate::{degree::solve, grammar::for_extension, tree::read};

    fn run(sources: &[(&str, &str)]) -> Vec<super::Spot> {
        let files: Vec<_> = sources
            .iter()
            .map(|(path, text)| {
                let extension = path.rsplit_once('.').unwrap().1;
                read(path, text, for_extension(extension).unwrap())
            })
            .collect();
        let solved = solve(&files);
        check(&files, &solved)
    }

    fn lines_of(spots: &[super::Spot], rule: Rule) -> Vec<usize> {
        spots.iter().filter(|s| s.rule == rule).map(|s| s.line).collect()
    }

    #[test]
    fn a_call_that_cannot_change_between_iterations_is_named_at_its_line() {
        let source = "export function draw(items, theme) {\n  for (const item of items) {\n    const style = palette(theme);\n    paint(item, style);\n  }\n}\n";
        let spots = run(&[("a.js", source)]);
        assert_eq!(lines_of(&spots, Rule::InvariantCall), vec![3]);
    }

    #[test]
    fn the_same_call_moved_to_depend_on_the_loop_stops_being_reported() {
        let source = "export function draw(items, theme) {\n  for (const item of items) {\n    const style = palette(item);\n    paint(item, style);\n  }\n}\n";
        let spots = run(&[("a.js", source)]);
        assert!(lines_of(&spots, Rule::InvariantCall).is_empty(), "it now depends on the loop");
    }

    #[test]
    fn a_value_built_every_iteration_for_no_reason_is_named() {
        let source = "export function draw(rows) {\n  for (const row of rows) {\n    const f = new Intl(\"en\");\n    show(row, f);\n  }\n}\n";
        let spots = run(&[("a.js", source)]);
        assert_eq!(lines_of(&spots, Rule::InvariantAllocation), vec![3]);
    }

    #[test]
    fn the_same_call_twice_in_one_block_is_named_at_the_second() {
        let source = "export function page(state) {\n  const a = total(state.rows);\n  const b = total(state.rows);\n  return a + b;\n}\n";
        let spots = run(&[("a.js", source)]);
        assert_eq!(lines_of(&spots, Rule::RepeatedCall), vec![3]);
    }

    #[test]
    fn a_definition_nothing_reaches_is_named() {
        let source =
            "function helper(x) {\n  return x;\n}\nexport function used() {\n  return 1;\n}\n";
        let spots = run(&[("a.js", source)]);
        let names: Vec<_> = spots
            .iter()
            .filter(|s| s.rule == Rule::UnreachableDefinition)
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, vec!["helper".to_owned()]);
    }

    #[test]
    fn an_import_never_mentioned_again_is_named_at_its_line() {
        let source = "import { heavy } from \"pkg\";\nexport function page() {\n  return 1;\n}\n";
        let spots = run(&[("a.js", source)]);
        assert_eq!(lines_of(&spots, Rule::UnusedImport), vec![1]);
    }

    #[test]
    fn growth_hidden_behind_a_call_is_separated_from_growth_written_in_place() {
        let visible = "export function a(xs) {\n  for (const x of xs) {\n    for (const y of xs) {\n      touch(x, y);\n    }\n  }\n}\n";
        let hidden = "function inner(xs) {\n  for (const x of xs) {\n    touch(x);\n  }\n}\nexport function outer(xs) {\n  for (const x of xs) {\n    inner(xs);\n  }\n}\n";
        assert_eq!(lines_of(&run(&[("a.js", visible)]), Rule::NestedGrowth), vec![3]);
        assert_eq!(
            lines_of(&run(&[("b.js", hidden)]), Rule::HiddenGrowth),
            vec![8],
            "the call site, which is the line that would be changed"
        );
    }

    #[test]
    fn a_clean_file_proves_nothing() {
        let source = "export function draw(items) {\n  for (const item of items) {\n    paint(item);\n  }\n}\n";
        assert!(run(&[("a.js", source)]).is_empty(), "nothing is claimed about clean code");
    }
}

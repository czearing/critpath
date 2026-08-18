//! Style rules that nothing can reach, and the ones nothing can decide.
//!
//! The usual version of this check scans templates for whole class strings and deletes whatever it
//! did not see, which is how a build silently removes a style that was assembled at runtime. The
//! rule here refuses instead. It does not ask "did I see this used?" -- it asks "could a name be
//! built here that I cannot enumerate?" When a stylesheet's binding is only ever read with a
//! literal key, the reference set is complete and absence is proof. The moment the binding is
//! indexed by a variable, spread, or passed on whole, every class in that stylesheet becomes
//! undecidable and is reported as such, counted, never deleted and never quietly dropped.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Repo;

/// A rule nothing can reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unused {
    /// File the rule is written in.
    pub file: String,
    /// Line the rule starts on, counting from one.
    pub line: usize,
    /// The selector, as written.
    pub selector: String,
    /// Bytes the whole rule occupies, which is what deleting it removes.
    pub bytes: u64,
}

/// A stylesheet whose use cannot be decided from source, and the construct that made it so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undecidable {
    /// The stylesheet.
    pub file: String,
    /// Why no claim can be made about it.
    pub reason: String,
    /// Classes it declares, none of which can be judged.
    pub classes: usize,
}

/// Examines every stylesheet owned by the repository.
///
/// Only owned components are examined. An installed package's stylesheet is not ours to edit, and
/// reporting one would be a work item nobody can action.
pub fn styles(repo: &Repo) -> (Vec<Unused>, Vec<Undecidable>) {
    let mut unused = Vec::new();
    let mut undecidable = Vec::new();
    // A repository's root is itself a component, so its directory contains every other component's
    // directory. Walking each in turn would read every file once per enclosing component and
    // report each finding that many times. Each component therefore stops at the boundary of the
    // next one, which makes the walk a partition rather than an overlap.
    let owned: Vec<PathBuf> =
        repo.components.iter().filter(|c| c.owned).map(|c| PathBuf::from(&c.directory)).collect();
    for component in repo.components.iter().filter(|c| c.owned) {
        let root = PathBuf::from(&component.directory);
        let sources = source_files(&root, &owned);
        for sheet in sources.iter().filter(|p| is_stylesheet(p)) {
            let Ok(text) = fs::read_to_string(sheet) else {
                continue;
            };
            let rules = rules_in(&text);
            if rules.is_empty() {
                continue;
            }
            let name = sheet.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
            match referenced(&sources, &name) {
                Reach::Certain(seen) => {
                    for rule in rules {
                        if !rule.classes.is_empty()
                            && rule.classes.iter().all(|class| !seen.contains(class))
                        {
                            unused.push(Unused {
                                file: show(sheet),
                                line: rule.line,
                                selector: rule.selector,
                                bytes: rule.bytes,
                            });
                        }
                    }
                }
                Reach::Unknown(reason) => undecidable.push(Undecidable {
                    file: show(sheet),
                    reason,
                    classes: rules.iter().map(|r| r.classes.len()).sum(),
                }),
            }
        }
    }
    unused.sort_by_key(|u| std::cmp::Reverse(u.bytes));
    undecidable.sort_by(|a, b| a.file.cmp(&b.file));
    (unused, undecidable)
}

/// What the reference scan concluded.
enum Reach {
    /// Every reference was a literal, so this set is complete.
    Certain(Vec<String>),
    /// A name could be built that cannot be enumerated.
    Unknown(String),
}

/// One rule, with the classes its selector names.
struct Rule {
    selector: String,
    classes: Vec<String>,
    line: usize,
    bytes: u64,
}

/// Reads the class names a stylesheet declares, and how much room each rule takes.
///
/// Selectors are read only where a selector can appear: outside every block, or inside a block an
/// at-rule opened. That is what keeps a length like `0.5rem` from being mistaken for a class.
fn rules_in(text: &str) -> Vec<Rule> {
    let text = without_comments(text);
    let bytes = text.as_bytes();
    let mut rules = Vec::new();
    let mut depth: Vec<bool> = Vec::new();
    let mut start = 0usize;
    let mut at = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'{' => {
                let selector = text[start..at].trim().to_owned();
                let is_at_rule = selector.starts_with('@');
                if !is_at_rule && depth.iter().all(|opened_by_at_rule| *opened_by_at_rule) {
                    if let Some(end) = closing(bytes, at) {
                        rules.push(Rule {
                            classes: classes_in(&selector),
                            line: text[..start + leading(&text[start..])].matches('\n').count() + 1,
                            bytes: (end + 1 - (start + leading(&text[start..]))) as u64,
                            selector,
                        });
                    }
                }
                depth.push(is_at_rule);
                start = at + 1;
            }
            b'}' => {
                depth.pop();
                start = at + 1;
            }
            b';' => start = at + 1,
            _ => {}
        }
        at += 1;
    }
    rules
}

/// Offset of the first non-whitespace byte, so a rule's span starts at its selector.
fn leading(text: &str) -> usize {
    text.len() - text.trim_start().len()
}

/// Offset of the brace that closes the one at `open`.
fn closing(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// The class names a selector mentions.
fn classes_in(selector: &str) -> Vec<String> {
    let bytes = selector.as_bytes();
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'.' && at + 1 < bytes.len() && starts_name(bytes[at + 1]) {
            let mut end = at + 1;
            while end < bytes.len() && continues_name(bytes[end]) {
                end += 1;
            }
            out.push(selector[at + 1..end].to_owned());
            at = end;
            continue;
        }
        at += 1;
    }
    out
}

fn starts_name(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'-'
}

fn continues_name(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'/' && bytes.get(at + 1) == Some(&b'*') {
            // Keep newlines so line numbers survive the strip.
            let mut end = at + 2;
            while end < bytes.len() && !(bytes[end] == b'*' && bytes.get(end + 1) == Some(&b'/')) {
                if bytes[end] == b'\n' {
                    out.push('\n');
                }
                end += 1;
            }
            at = (end + 2).min(bytes.len());
            continue;
        }
        out.push(bytes[at] as char);
        at += 1;
    }
    out
}

/// Every class name read out of a stylesheet's binding, or the reason no such set exists.
fn referenced(sources: &[PathBuf], sheet: &str) -> Reach {
    let mut seen = Vec::new();
    let mut importers = 0usize;
    for source in sources.iter().filter(|p| is_source(p)) {
        let Ok(text) = fs::read_to_string(source) else {
            continue;
        };
        if !text.contains(sheet) {
            continue;
        }
        let Some(binding) = binding_for(&text, sheet) else {
            // Named in the file but not bound to an identifier: a side-effect import, or a form
            // this reader does not model. Either way the class set cannot be closed.
            return Reach::Unknown(format!(
                "{} names it without binding it, so which classes are read cannot be enumerated",
                show(source)
            ));
        };
        importers += 1;
        match reads_of(&text, &binding) {
            Reach::Certain(found) => seen.extend(found),
            Reach::Unknown(reason) => {
                return Reach::Unknown(format!("{} {reason}", show(source)));
            }
        }
    }
    if importers == 0 {
        return Reach::Unknown(
            "no file in this component imports it, and an importer elsewhere cannot be ruled out"
                .to_owned(),
        );
    }
    Reach::Certain(seen)
}

/// The identifier a source file binds a stylesheet to.
fn binding_for(text: &str, sheet: &str) -> Option<String> {
    for line in text.lines() {
        if !line.contains(sheet) || !line.contains("import") {
            continue;
        }
        let after = line.split_once("import")?.1;
        let name = after.split_once("from")?.0.trim();
        let name = name.trim_start_matches('*').trim().trim_start_matches("as ").trim();
        if !name.is_empty() && name.bytes().all(|b| continues_name(b) || b == b'$') {
            return Some(name.to_owned());
        }
        return None;
    }
    None
}

/// Every literal key read from a binding, or the construct that makes the set unbounded.
fn reads_of(text: &str, binding: &str) -> Reach {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0usize;
    while let Some(offset) = text[at..].find(binding) {
        let start = at + offset;
        at = start + binding.len();
        let before = start.checked_sub(1).map(|i| bytes[i]);
        if before == Some(b'.') {
            // `...binding` spreads it whole; `other.binding` is a different thing entirely.
            if start >= 3 && &text[start - 3..start] == "..." {
                return Reach::Unknown(
                    "spreads it whole rather than reading it by key, so any class could be named \
                     from it"
                        .to_owned(),
                );
            }
            continue;
        }
        if before.is_some_and(continues_name) {
            continue;
        }
        if bytes.get(at).is_some_and(|b| continues_name(*b)) {
            continue;
        }
        let mut cursor = at;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        match bytes.get(cursor) {
            // The import that creates the binding is not a read of it.
            Some(b'f') if text[cursor..].starts_with("from") => {}
            Some(b'.') => {
                let mut end = cursor + 1;
                while bytes.get(end).is_some_and(|b| continues_name(*b)) {
                    end += 1;
                }
                found.push(text[cursor + 1..end].to_owned());
            }
            Some(b'[') => {
                let mut end = cursor + 1;
                while bytes.get(end).is_some_and(u8::is_ascii_whitespace) {
                    end += 1;
                }
                let quote = match bytes.get(end) {
                    Some(b'\'') => b'\'',
                    Some(b'"') => b'"',
                    _ => return Reach::Unknown(
                        "indexes it with something other than a literal, so the class it reads \
                             cannot be enumerated"
                            .to_owned(),
                    ),
                };
                let from = end + 1;
                let mut to = from;
                while bytes.get(to).is_some_and(|b| *b != quote) {
                    to += 1;
                }
                found.push(text[from..to].to_owned());
            }
            // Read whole: spread, passed as an argument, re-exported. Anything downstream could
            // name any class, and this reader is not going to follow it.
            _ => {
                return Reach::Unknown(
                    "reads it whole rather than by key, so any class could be named from it"
                        .to_owned(),
                )
            }
        }
    }
    Reach::Certain(found)
}

fn is_stylesheet(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    name.ends_with(".module.css") || name.ends_with(".module.scss")
}

fn is_source(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    [".ts", ".tsx", ".js", ".jsx", ".mts", ".cts"].iter().any(|extension| name.ends_with(extension))
}

/// Files a component owns, stopping where the next owned component begins.
///
/// Installed packages are skipped: they are not ours to change.
fn source_files(root: &Path, owned: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(at) = queue.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                let name = entry.file_name();
                if name == "node_modules" || name == "dist" || name == "lib" || name == ".git" {
                    continue;
                }
                if owned.iter().any(|boundary| *boundary == entry.path()) {
                    continue;
                }
                queue.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

fn show(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{binding_for, classes_in, reads_of, rules_in, Reach};
    use std::fs;

    #[test]
    fn a_length_is_not_mistaken_for_a_class() {
        let rules = rules_in(".card { margin: 0.5rem; padding: 1.25em; }");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].classes, vec!["card".to_owned()]);
    }

    #[test]
    fn a_selector_inside_an_at_rule_is_still_a_selector() {
        let rules = rules_in("@media (min-width: 40em) { .wide { color: red; } }");
        let classes: Vec<_> = rules.iter().flat_map(|r| r.classes.clone()).collect();
        assert_eq!(classes, vec!["wide".to_owned()]);
    }

    #[test]
    fn a_commented_out_rule_is_not_a_rule() {
        let rules = rules_in("/* .old { color: red; } */\n.new { color: blue; }");
        let classes: Vec<_> = rules.iter().flat_map(|r| r.classes.clone()).collect();
        assert_eq!(classes, vec!["new".to_owned()]);
        assert_eq!(rules[0].line, 2, "line numbers survive the comment strip");
    }

    #[test]
    fn a_rule_reports_the_bytes_deleting_it_removes() {
        let text = ".a { color: red; }";
        assert_eq!(rules_in(text)[0].bytes, text.len() as u64);
    }

    #[test]
    fn a_literal_read_is_certain() {
        let text = "import styles from './x.module.css';\nuse(styles.alpha, styles['beta']);";
        let Reach::Certain(mut found) = reads_of(text, "styles") else {
            panic!("literal reads are enumerable");
        };
        found.sort();
        assert_eq!(found, vec!["alpha".to_owned(), "beta".to_owned()]);
    }

    #[test]
    fn a_computed_key_refuses_rather_than_guessing() {
        let text = "import styles from './x.module.css';\nuse(styles[name]);";
        assert!(
            matches!(reads_of(text, "styles"), Reach::Unknown(_)),
            "a variable key could name any class"
        );
    }

    #[test]
    fn passing_the_binding_whole_refuses() {
        let text = "import styles from './x.module.css';\nuse({ ...styles });";
        assert!(matches!(reads_of(text, "styles"), Reach::Unknown(_)));
    }

    #[test]
    fn a_name_that_merely_contains_the_binding_is_not_a_read() {
        let text = "import styles from './x.module.css';\nconst stylesheet = 1; styles.only;";
        let Reach::Certain(found) = reads_of(text, "styles") else {
            panic!("nothing here is dynamic");
        };
        assert_eq!(found, vec!["only".to_owned()]);
    }

    #[test]
    fn a_component_stops_at_the_next_component_so_nothing_is_read_twice() {
        // The repository root is itself a component and contains every other one. Without the
        // boundary, every finding under `packages/inner` is reported once for the root and once
        // for the package, which is how a report ends up with each item printed twice.
        let root = std::env::temp_dir().join("critpath-repo-partition");
        let _ = fs::remove_dir_all(&root);
        let inner = root.join("packages/inner");
        fs::create_dir_all(inner.join("src")).expect("dirs");
        fs::write(inner.join("src/a.tsx"), "x").expect("write");
        fs::write(root.join("top.tsx"), "x").expect("write");

        let owned = vec![root.clone(), inner.clone()];
        let from_root = super::source_files(&root, &owned);
        let from_inner = super::source_files(&inner, &owned);
        assert_eq!(from_root.len(), 1, "the root stops at the package boundary");
        assert_eq!(from_inner.len(), 1);
        assert!(
            from_root.iter().all(|p| !from_inner.contains(p)),
            "no file belongs to two components"
        );
    }

    #[test]
    fn the_binding_is_read_from_the_import() {
        let text = "import sheet from './Card.module.css';";
        assert_eq!(binding_for(text, "Card.module.css").as_deref(), Some("sheet"));
    }

    #[test]
    fn a_compound_selector_names_every_class_in_it() {
        assert_eq!(
            classes_in(".a .b > .c:hover"),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }
}

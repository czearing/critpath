//! Turning a stated position in generated code into the line a person would edit.
//!
//! This crate reads what a build already wrote down. A source map is a lookup table the compiler
//! emitted, in the same family as DWARF and PDB, and consulting one is what every production
//! profiler does to put a sampled address on a line. Nothing here inspects code to decide whether
//! it is slow, because no reading of source can decide that: every non-trivial semantic property
//! of a program is undecidable, so cost must keep coming from the measurement and source may only
//! ever supply the name.
//!
//! Two things separate this from calling a mapping library.
//!
//! The first is that no numbering convention is assumed. Producers disagree about whether the
//! first line is zero or one, and the disagreement is invisible: both readings return a real file
//! and an adjacent, plausible line, so a tool that picks the documented one ships confident,
//! well-formatted, consistently wrong locations. The convention is therefore *proved* per map,
//! from the trace's own evidence, before any position is reported.
//!
//! The second is that a resolution is only believed when something independent agrees with it. A
//! map carries the original text; a trace carries the name of the function it ran. Neither knows
//! about the other, so a position is accepted only where the text at that line names that
//! function, and refused where it does not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use critpath_core::Site;

mod map;
mod package;

pub use map::SourceMap;
pub use package::{package_of, Fixability};

/// A position in original source, with everything needed to act on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    /// The original file, as the build recorded it.
    pub source: String,
    /// The line, counted from one, as an editor shows it.
    pub line: u32,
    /// The column, counted from one.
    pub column: u32,
    /// The text of that line, when the map carried the original source.
    pub text: Option<String>,
    /// Which dependency the file belongs to, when it belongs to one.
    pub package: Option<String>,
}

impl Located {
    /// Whether this is code the repository under test can change.
    pub fn fixability(&self) -> Fixability {
        if self.package.is_some() {
            Fixability::Dependency
        } else {
            Fixability::Repository
        }
    }

    /// `file:line`, the form every editor and terminal already understands.
    pub fn at(&self) -> String {
        format!("{}:{}", self.source, self.line)
    }
}

/// What could be established about one stated position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Resolved, and the original line at that position names the function the trace ran.
    ///
    /// Two independent derivations agreeing. This is the only outcome that may be reported as the
    /// exact line without qualification.
    Proved(Located),
    /// Resolved using the convention proved elsewhere in the same map, with nothing here to check
    /// it against.
    ///
    /// The producer named no function, or the map carried no original text for this file. Weaker
    /// than [`Resolution::Proved`] and reported as such, never merged with it.
    Derived(Located),
    /// A map was found and a convention proved, but it maps nothing at that position.
    Unmapped,
    /// A map was found, but no position in it could be corroborated, so no convention is known.
    ///
    /// Refused rather than guessed. Reporting under an unproved convention is how a resolver
    /// becomes confidently one line wrong.
    Unproved,
    /// No map was supplied for that script.
    ///
    /// Ordinary and not a fault: third-party scripts ship without maps. It is counted so that a
    /// report can distinguish "nothing to resolve" from "nothing found".
    Absent,
}

impl Resolution {
    /// The position, when one was established at all.
    pub fn located(&self) -> Option<&Located> {
        match self {
            Self::Proved(at) | Self::Derived(at) => Some(at),
            _ => None,
        }
    }

    /// Whether an independent source agreed with this position.
    pub const fn is_proved(&self) -> bool {
        matches!(self, Self::Proved(_))
    }
}

/// How far a stated line and column are from the map's own numbering.
///
/// Only ever `0` or `1`, and never chosen: the value is the one that made the trace agree with the
/// original source, and a map for which neither did is left without a convention.
type Shift = u32;

/// The shifts tried, in the order they are tried.
///
/// One first, because that is the convention measured on real captures, and the order only decides
/// ties where both readings corroborate. Both are attempted every time regardless, so a producer
/// counting the other way is resolved correctly with no configuration.
const SHIFTS: [Shift; 2] = [1, 0];

/// A map, and what has been proved about how to read it.
struct Loaded {
    map: SourceMap,
    /// The shift proved by corroboration, when the evidence was unanimous.
    shift: Option<Shift>,
    /// How many positions voted for the shift.
    votes: usize,
    /// How many positions contradicted every candidate shift.
    against: usize,
}

/// The maps available, and the conventions proved for them.
///
/// Loaded from a directory rather than fetched. A reader that reaches the network to resolve a
/// symbol is no longer a pure function of its inputs, and the map that matters is the one from the
/// build that was measured, which only the operator can identify.
pub struct Resolver {
    directory: PathBuf,
    loaded: BTreeMap<String, Option<Loaded>>,
}

/// What resolving a whole set of positions established, for the report to state plainly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Calibration {
    /// Scripts a map was found for.
    pub mapped: usize,
    /// Scripts no map was supplied for.
    pub unmapped: usize,
    /// Maps whose numbering was proved by corroborating positions.
    pub proved: usize,
    /// Maps where no position corroborated, so nothing from them may be reported.
    pub unproved: usize,
    /// Positions that agreed with the original source.
    pub agreements: usize,
    /// Positions that agreed with no reading of the map.
    pub disagreements: usize,
}

impl Resolver {
    /// A resolver over a directory of `.map` files.
    pub fn at(directory: impl AsRef<Path>) -> Self {
        Self { directory: directory.as_ref().to_path_buf(), loaded: BTreeMap::new() }
    }

    /// The file a script's map would be stored under.
    ///
    /// The name a build writes into `sourceMappingURL` is the script's own file name with `.map`
    /// appended, so that is what is looked for. A query string is dropped, since it selects a
    /// version of the resource rather than naming a different file.
    fn map_name(script: &str) -> String {
        let last = script.rsplit('/').next().unwrap_or(script);
        let bare = last.split(['?', '#']).next().unwrap_or(last);
        format!("{bare}.map")
    }

    /// Load a script's map once, remembering the answer either way.
    fn load(&mut self, script: &str) -> Option<&mut Loaded> {
        let name = Self::map_name(script);
        let directory = &self.directory;
        self.loaded
            .entry(name.clone())
            .or_insert_with(|| {
                let bytes = std::fs::read(directory.join(&name)).ok()?;
                let map = SourceMap::parse(&bytes)?;
                Some(Loaded { map, shift: None, votes: 0, against: 0 })
            })
            .as_mut()
    }

    /// Prove how each map is numbered, from positions whose function name the source confirms.
    ///
    /// Must be called before [`Resolver::resolve`], and is the reason this type exists rather than
    /// a free function: the evidence that makes one position trustworthy comes from the other
    /// positions in the same map, so they have to be seen together.
    pub fn calibrate<'a>(&mut self, sites: impl IntoIterator<Item = Site<'a>>) -> Calibration {
        let mut census = Calibration::default();
        for site in sites {
            let Some(loaded) = self.load(site.script) else { continue };
            if site.symbol.is_empty() {
                continue;
            }
            let mut agreed = None;
            for shift in SHIFTS {
                let Some(at) = loaded.map.lookup(site.line, site.column, shift) else { continue };
                if at.text.as_deref().is_some_and(|text| names(text, site.symbol)) {
                    agreed = Some(shift);
                    break;
                }
            }
            if let Some(shift) = agreed {
                census.agreements += 1;
                loaded.votes += 1;
                // A map read two ways cannot be trusted either way. Rather than let a majority
                // decide, the convention is withdrawn: half-right locations are worse than
                // none, because nothing in the report tells the reader which half.
                if loaded.shift.is_some_and(|proved| proved != shift) {
                    loaded.shift = None;
                    loaded.against += 1;
                } else {
                    loaded.shift = Some(shift);
                }
            } else {
                census.disagreements += 1;
                loaded.against += 1;
            }
        }
        for entry in self.loaded.values() {
            match entry {
                Some(loaded) if loaded.shift.is_some() => {
                    census.mapped += 1;
                    census.proved += 1;
                }
                Some(_) => {
                    census.mapped += 1;
                    census.unproved += 1;
                }
                None => census.unmapped += 1,
            }
        }
        census
    }

    /// How many positions voted for a map's convention, and how many contradicted it.
    pub fn evidence(&self, script: &str) -> Option<(usize, usize)> {
        let loaded = self.loaded.get(&Self::map_name(script))?.as_ref()?;
        Some((loaded.votes, loaded.against))
    }

    /// Where one stated position is in original source.
    ///
    /// The convention is settled first, by [`Self::calibrate`], on the segments alone. Only then is
    /// the answer allowed to move within what the map bounds: where exactly one of the original
    /// lines the generated code can have come from names the function the trace said was running,
    /// that line is the answer and the source says so. Where none of them does, or more than one
    /// does, nothing has been shown and the segment's own position is reported unconfirmed. More
    /// than one is refused rather than settled by preferring the nearest, because a rule that picks
    /// between equally supported lines reports a guess in the same words as a proof.
    pub fn resolve(&self, site: Site<'_>) -> Resolution {
        let Some(Some(loaded)) = self.loaded.get(&Self::map_name(site.script)) else {
            return Resolution::Absent;
        };
        let Some(shift) = loaded.shift else { return Resolution::Unproved };
        let within = loaded.map.candidates(site.line, site.column, shift);
        let Some(first) = within.first() else { return Resolution::Unmapped };
        if site.symbol.is_empty() {
            return Resolution::Derived(first.clone());
        }
        let mut naming = within
            .iter()
            .filter(|at| at.text.as_deref().is_some_and(|text| names(text, site.symbol)));
        match (naming.next(), naming.next()) {
            (Some(only), None) => Resolution::Proved(only.clone()),
            _ => Resolution::Derived(first.clone()),
        }
    }
}

/// Whether a line of original source names the function a trace said it was running.
///
/// The trace writes a qualified name where it can -- `WebSink.prototype.flushQueue` is reported as
/// `WebSink.flushQueue` -- so the last segment is what a declaration actually contains. Matching
/// on that is a weaker test than matching the whole name and deliberately so: it can accept a
/// wrong line whose text happens to contain the same identifier, which costs a little precision,
/// where the stricter test rejects every correct qualified name and costs the entire capability.
fn names(text: &str, symbol: &str) -> bool {
    let bare = symbol.rsplit('.').next().unwrap_or(symbol);
    !bare.is_empty() && text.contains(bare)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_is_looked_for_under_the_scripts_own_name() {
        assert_eq!(Resolver::map_name("https://h/assets/app.js"), "app.js.map");
        assert_eq!(Resolver::map_name("https://h/a/app.js?vsn=d"), "app.js.map");
        assert_eq!(Resolver::map_name("app.js"), "app.js.map");
    }

    #[test]
    fn a_declaration_joined_to_its_comment_is_still_proved() {
        // A generated line the transform built from a doc comment and the declaration under it.
        // The map records only the comment's line, so reading the segment alone answers one line
        // above the function that ran. The second generated line is what proves the numbering.
        let map = r#"{
            "version": 3,
            "sources": ["src/one.ts"],
            "sourcesContent": ["/** Reports the load. */\nfunction reportPageLoad() {\nfunction plain() {\n}\n"],
            "mappings": "AAAA;AAEA;AACA"
        }"#;
        let directory =
            std::env::temp_dir().join(format!("critpath-joined-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("a temporary directory must be creatable");
        std::fs::write(directory.join("app.js.map"), map).expect("the map must be writable");

        let site = |line, column, symbol| Site { script: "https://h/app.js", line, column, symbol };
        let mut resolver = Resolver::at(&directory);
        let census = resolver.calibrate([site(2, 1, "plain")]);
        assert_eq!(census.agreements, 1, "the plain declaration must prove the numbering");

        let at = match resolver.resolve(site(1, 120, "reportPageLoad")) {
            Resolution::Proved(at) => at,
            other => {
                panic!("the source names the function on a line the map leaves room for: {other:?}")
            }
        };
        assert_eq!(at.line, 2, "the declaration, not the comment the segment pointed at");
        assert_eq!(at.text.as_deref(), Some("function reportPageLoad() {"));

        // A name that no line in that room contains is not placed on one of them anyway.
        let derived = resolver.resolve(site(1, 120, "somethingElse"));
        assert!(matches!(derived, Resolution::Derived(ref at) if at.line == 1), "{derived:?}");
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn two_lines_that_both_name_it_are_not_chosen_between() {
        // The comment names the function it documents, so both original lines the generated line
        // leaves room for contain the name. Nothing distinguishes them, and preferring either
        // would report a guess in the words used for a proof.
        let map = r#"{
            "version": 3,
            "sources": ["src/one.ts"],
            "sourcesContent": ["/** What reportPageLoad does. */\nfunction reportPageLoad() {\nfunction plain() {\n}\n"],
            "mappings": "AAAA;AAEA;AACA"
        }"#;
        let directory =
            std::env::temp_dir().join(format!("critpath-ambiguous-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("a temporary directory must be creatable");
        std::fs::write(directory.join("app.js.map"), map).expect("the map must be writable");

        let site = |line, column, symbol| Site { script: "https://h/app.js", line, column, symbol };
        let mut resolver = Resolver::at(&directory);
        resolver.calibrate([site(2, 1, "plain")]);
        let answer = resolver.resolve(site(1, 120, "reportPageLoad"));
        assert!(
            matches!(answer, Resolution::Derived(ref at) if at.line == 1),
            "two supported lines must leave the segment's own position, unconfirmed: {answer:?}"
        );
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn a_qualified_name_is_matched_by_its_last_segment() {
        assert!(names("WebSink.prototype.flushQueue = function () {", "WebSink.flushQueue"));
        assert!(names("const tick = (): void => {", "tick"));
        assert!(!names("if (cancelled) {", "tick"));
        assert!(!names("anything at all", ""), "an unnamed function must never corroborate");
    }
}

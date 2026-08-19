//! What a language looks like, as data rather than as code.
//!
//! Nothing in this crate parses a specific language. A grammar names the few lexical facts needed
//! to fold a file into blocks -- what hides text, what opens and closes a block, what repeats, and
//! what defines something callable -- and every rule downstream works on the resulting tree. That
//! is what keeps the engine indifferent to whether it is reading a web app or a game: adding a
//! language is a table entry, and a rule written for one language already applies to the rest.
//!
//! A grammar deliberately does not describe expressions. Anything that needs to know what an
//! expression *means* is a claim about one language's semantics, and would have to be rewritten
//! for the next one.

/// The lexical shape of one language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grammar {
    /// What the language is called, for the report.
    pub name: &'static str,
    /// File extensions this grammar reads, without the dot.
    pub extensions: &'static [&'static str],
    /// Openers that hide the rest of the line.
    pub line_comment: &'static [&'static str],
    /// Openers and closers that hide everything between them.
    pub block_comment: &'static [(&'static str, &'static str)],
    /// Quote characters that hide text until the matching quote.
    pub quotes: &'static [char],
    /// Whether a backslash escapes the next character inside quoted text.
    pub escapes: bool,
    /// The character that opens a block.
    pub open: char,
    /// The character that closes a block.
    pub close: char,
    /// Words that introduce a block entered more than once.
    ///
    /// The whole engine rests on this list. A block under one of these runs an unknown number of
    /// times, so everything inside it is charged a degree of repetition.
    pub repeats: &'static [&'static str],
    /// Words that introduce something callable.
    pub defines: &'static [&'static str],
    /// Method names that call their argument once per element.
    ///
    /// A loop written as a call. Separated from [`repeats`](Self::repeats) because it is matched
    /// against the text before an opening delimiter rather than against a leading keyword.
    pub iterating_calls: &'static [&'static str],
    /// Words that take a parenthesised list without calling anything.
    ///
    /// Without this the parameter list of a definition reads as a call to the thing being defined,
    /// which makes every routine reach itself and the whole call graph one unbounded cycle.
    pub not_calls: &'static [&'static str],
    /// Words that build a fresh value every time they are evaluated.
    ///
    /// Building the same thing repeatedly inside a repeat is work the program does not need to
    /// do, and the position of the word is the position of the work.
    pub allocates: &'static [&'static str],
    /// Fragments that, at the start of an argument, mean a value with a new identity each time.
    ///
    /// Two structurally equal values built separately are not the same value, and anything that
    /// compares by identity -- a cache, a memo, a change check -- will see a difference that is
    /// not there.
    pub fresh: &'static [&'static str],
    /// Words that bring a name in from elsewhere.
    pub imports: &'static [&'static str],
    /// Words that make a name reachable from outside the file.
    ///
    /// A definition that is neither exported nor called is dead, but only a language's own word
    /// for reachability can say which is which.
    pub exports: &'static [&'static str],
}

/// Control words that take a parenthesised list in every language here.
const CONTROL: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "catch", "return", "typeof", "await", "yield",
    "function", "fn", "def", "func", "match", "foreach", "using", "lock", "with", "assert",
];

/// The languages the engine reads.
///
/// Braces and C-style comments cover the overwhelming majority of what a repository under
/// performance review is written in, so they share one shape with different keyword lists.
pub const GRAMMARS: &[Grammar] = &[
    Grammar {
        name: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"],
        line_comment: &["//"],
        block_comment: &[("/*", "*/")],
        quotes: &['"', '\'', '`'],
        escapes: true,
        open: '{',
        close: '}',
        repeats: &["for", "while", "do"],
        defines: &["function", "=>"],
        iterating_calls: &[
            "map",
            "forEach",
            "filter",
            "reduce",
            "reduceRight",
            "flatMap",
            "some",
            "every",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "sort",
            "concat",
            "join",
        ],
        not_calls: CONTROL,
        allocates: &["new", "Object", "Array", "Map", "Set", "Date", "RegExp", "Intl", "JSON"],
        fresh: &["{", "[", "=>", "function", "new "],
        imports: &["import", "require"],
        exports: &["export"],
    },
    Grammar {
        name: "Rust",
        extensions: &["rs"],
        line_comment: &["//"],
        block_comment: &[("/*", "*/")],
        quotes: &['"'],
        escapes: true,
        open: '{',
        close: '}',
        repeats: &["for", "while", "loop"],
        defines: &["fn"],
        iterating_calls: &["map", "for_each", "filter", "fold", "flat_map", "any", "all", "find"],
        not_calls: CONTROL,
        allocates: &["new", "vec", "format", "to_string", "to_owned", "to_vec", "clone", "collect"],
        fresh: &["|", "vec!", "Box::new", "String::"],
        imports: &["use"],
        exports: &["pub"],
    },
    Grammar {
        name: "C-family",
        extensions: &[
            "c", "h", "cc", "cpp", "hpp", "cxx", "m", "mm", "cs", "java", "go", "kt", "swift",
            "scala",
        ],
        line_comment: &["//"],
        block_comment: &[("/*", "*/")],
        quotes: &['"', '\''],
        escapes: true,
        open: '{',
        close: '}',
        repeats: &["for", "while", "do", "foreach"],
        defines: &["func", "fn", "fun", "def", "function", "void", "public", "private", "static"],
        iterating_calls: &["map", "forEach", "filter", "reduce", "select", "where"],
        not_calls: CONTROL,
        allocates: &[
            "new",
            "malloc",
            "calloc",
            "make",
            "alloc",
            "strdup",
            "clone",
            "arrayOf",
            "listOf",
            "mutableListOf",
            "mapOf",
        ],
        fresh: &["{", "[", "->", "new ", "make("],
        imports: &["import", "using", "include"],
        exports: &["export", "public"],
    },
];

/// The grammar for a file extension, if the engine reads that language.
#[must_use]
pub fn for_extension(extension: &str) -> Option<&'static Grammar> {
    let lowered = extension.to_ascii_lowercase();
    GRAMMARS.iter().find(|grammar| grammar.extensions.contains(&lowered.as_str()))
}

#[cfg(test)]
mod tests {
    use super::{for_extension, GRAMMARS};

    #[test]
    fn a_language_is_recognised_by_extension_whatever_its_case() {
        assert_eq!(for_extension("TSX").map(|g| g.name), Some("JavaScript"));
        assert_eq!(for_extension("rs").map(|g| g.name), Some("Rust"));
        assert!(for_extension("txt").is_none(), "a grammar is never guessed at");
    }

    #[test]
    fn no_extension_is_claimed_by_two_grammars() {
        // Two grammars over one extension would make the reading depend on table order, which is
        // the kind of silent tie that decides an answer without saying so.
        let mut seen: Vec<&str> = Vec::new();
        for grammar in GRAMMARS {
            for extension in grammar.extensions {
                assert!(!seen.contains(extension), "{extension} is claimed twice");
                seen.push(extension);
            }
        }
    }
}

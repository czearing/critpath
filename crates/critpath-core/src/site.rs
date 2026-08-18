//! The code position a producer stated alongside a piece of work.
//!
//! A trace names work by whatever the runtime called it, and that name is rarely a place. The one
//! exception is worth everything: some producers write the script, line and column of the function
//! they are about to run, on the same event that measures how long it took. When that is present,
//! cost and location come from a single record and no correlation is needed to say where the time
//! went.
//!
//! Nothing here interprets the position. It is lifted out of the subject the reader already
//! assembled, exactly as the producer wrote it, and handed on to be resolved against whatever the
//! build emitted. In particular the numbering convention is *not* decided here: producers disagree
//! about whether the first line is zero or one, so committing to either in this file would make
//! the reader correct on one toolchain and quietly one line wrong on the next.

/// A position in generated code, as the producer stated it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site<'a> {
    /// The script the producer named.
    pub script: &'a str,
    /// The line, in whatever base the producer counts from.
    pub line: u32,
    /// The column, in whatever base the producer counts from.
    pub column: u32,
    /// What the producer called the function there. Empty when it named none.
    ///
    /// Not decoration. It is the only thing in the trace that can be checked against the original
    /// source, so it is what turns a resolution from plausible into proved.
    pub symbol: &'a str,
}

/// Read the leaf field name out of one `path=value` pair, without allocating.
fn field(pair: &str) -> Option<(&str, &str)> {
    let (path, value) = pair.split_once('=')?;
    let leaf = path.rsplit('.').next().unwrap_or(path);
    Some((leaf, value))
}

/// Strip the quotes a JSON string value carries, leaving anything else alone.
fn unquoted(value: &str) -> &str {
    value.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(value)
}

/// The code position stated inside one assembled subject, if it states a whole one.
///
/// All three of script, line and column are required. A position missing any of them cannot be
/// resolved to a line, and half a position invites a reader to fill in the rest, which is the one
/// thing a tool that claims to name the exact line must never do.
#[must_use]
pub fn site_of(subject: &str) -> Option<Site<'_>> {
    let (mut script, mut line, mut column, mut symbol) = (None, None, None, "");
    for pair in subject.split('\u{1}') {
        let Some((leaf, value)) = field(pair) else { continue };
        match leaf {
            "url" => script = Some(unquoted(value)),
            "lineNumber" => line = value.parse::<u32>().ok(),
            "columnNumber" => column = value.parse::<u32>().ok(),
            "functionName" => symbol = unquoted(value),
            _ => {}
        }
    }
    let script = script?;
    if script.is_empty() {
        // A producer that knows the position but not the script writes an empty url. There is
        // nothing to resolve against, and guessing the script from anything else in the event is
        // how a line number ends up attributed to the wrong file entirely.
        return None;
    }
    Some(Site { script, line: line?, column: column?, symbol })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape the reader assembles for a Chrome `FunctionCall`: every argument flattened
    /// to `data.<field>=<json>`, sorted, joined by U+0001.
    const REAL: &str =
        "data.columnNumber=26\u{1}data.frame=\"A90C\"\u{1}data.functionName=\"tick\"\
                        \u{1}data.isolate=\"683\"\u{1}data.lineNumber=1819\u{1}data.sampleTraceId=5\
                        \u{1}data.scriptId=\"10\"\u{1}data.url=\"https://host/assets/app.js\"";

    #[test]
    fn a_position_is_read_from_the_subject_the_reader_assembles() {
        let site = site_of(REAL).expect("the real shape must parse");
        assert_eq!(site.script, "https://host/assets/app.js");
        assert_eq!(site.line, 1819);
        assert_eq!(site.column, 26);
        assert_eq!(site.symbol, "tick");
    }

    #[test]
    fn a_subject_without_a_position_states_none() {
        assert_eq!(site_of("data.url=\"https://host/a.js\""), None, "no line and no column");
        assert_eq!(site_of("data.available=1024"), None);
        assert_eq!(site_of(""), None);
    }

    #[test]
    fn a_position_without_a_script_is_refused() {
        // Instant events carry stack frames whose top frame often has an empty url and only a
        // script id. Nothing can resolve that, and inventing a script for it is worse than silence.
        let subject = "data.columnNumber=46\u{1}data.lineNumber=69\u{1}data.url=\"\"";
        assert_eq!(site_of(subject), None);
    }

    #[test]
    fn an_unnamed_function_still_yields_a_position() {
        let subject = "data.columnNumber=1\u{1}data.functionName=\"\"\u{1}data.lineNumber=2\
                       \u{1}data.url=\"https://host/a.js\"";
        let site = site_of(subject).expect("a position without a name is still a position");
        assert_eq!(site.symbol, "", "and it must say so rather than invent one");
    }

    #[test]
    fn nested_fields_are_matched_by_their_leaf_not_their_path() {
        // Producers nest differently. Matching the whole path would work on one and silently find
        // nothing on the next, which is a resolver that reports every trace as unresolvable.
        let subject = "args.data.frame.columnNumber=3\u{1}args.data.frame.lineNumber=4\
                       \u{1}args.data.frame.url=\"https://host/b.js\"";
        let site = site_of(subject).expect("depth must not matter");
        assert_eq!((site.line, site.column), (4, 3));
    }
}

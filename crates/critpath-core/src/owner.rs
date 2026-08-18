//! Which principal a piece of work belongs to.
//!
//! A recording made on a real machine contains other people's programs. A browser extension runs
//! its own scripts, issues its own requests and lands its own tasks in the same trace as the page
//! under test, and on a measured capture of one product 19 of 68 findings belonged to an extension
//! rather than to the product. Reporting those is worse than reporting nothing: they are true,
//! expensive, and nobody on the receiving team can act on a single one of them.
//!
//! Ownership is therefore derived, never listed. The operator declares which origin is under test
//! -- one fact the recording cannot supply, because the obvious heuristic picks an extension -- and
//! every subject is then classified by the script origin the trace already states for it. No
//! function name, framework name or ignore list appears anywhere here, and none may: the moment
//! this file knows that a symbol belongs to a browser it has learned a framework, and the same
//! reader stops working on the next producer.

/// Whose code a piece of work is, relative to the origin declared under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The trace states this work's origin, and it is the one under test.
    UnderTest,
    /// The trace states this work's origin, and it is some other program.
    ///
    /// Withheld from the report rather than ranked in it. Withheld and not deleted, because a
    /// count of what was set aside is the only way an operator can tell a filter that worked from
    /// a filter that ate the evidence.
    Elsewhere,
    /// The trace states no origin for this work at all.
    ///
    /// Everything a browser, runtime or kernel does on its own behalf lands here, because a
    /// producer writes a script url for script and writes none for its own internals. That is the
    /// same fact as "this is not the product's code" without knowing one name of one browser, but
    /// it is weaker evidence than [`Owner::Elsewhere`] -- an unnamed thing might still be the
    /// product's -- so it is reported apart from both.
    Unstated,
}

impl Owner {
    /// Whether work with this ownership may be reported as the declared origin's problem.
    #[must_use]
    pub const fn is_under_test(self) -> bool {
        matches!(self, Self::UnderTest)
    }
}

/// The scheme-and-host prefix of the first URL appearing anywhere in some text.
///
/// Deliberately syntactic, and deliberately not anchored at the start. Anything shaped
/// `scheme://host` is an origin, which covers http, https and the extension and internal schemes a
/// browser also writes. It must match mid-string because the only place a url survives is inside
/// the subject the reader assembles, where it is written as `data.url="http://host/path"` and any
/// anchored test silently finds nothing -- which is a filter that quietly classifies every subject
/// as unowned.
#[must_use]
pub fn origin_of(text: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(offset) = text[from..].find("://") {
        let split = from + offset;
        // Walk back over the scheme itself. A scheme is alphanumeric with dashes, so the first
        // character that is not is where the surrounding text ends and the url begins.
        let start = text[..split]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| c.is_ascii_alphanumeric() || c == '-')
            .last()
            .map_or(split, |(index, _)| index);
        let rest = &text[split + 3..];
        let end = rest
            .find(|c: char| c == '/' || c == '"' || c == '\'' || c.is_whitespace())
            .unwrap_or(rest.len());
        if start < split && end > 0 {
            return Some(&text[start..split + 3 + end]);
        }
        from = split + 3;
    }
    None
}

/// Classify one recorded subject against the origin under test.
///
/// A subject is every argument the producer wrote, joined; any of them may carry the url. Naming
/// the declared origin anywhere wins outright, deliberately: a request the product makes to
/// somebody else's server names both, and that is the product's own request to fix. The rule can
/// therefore only ever set aside work that never mentions the product at all, which is the
/// conservative direction for a filter to fail in.
#[must_use]
pub fn owner_of(subject: &str, declared: &str) -> Owner {
    let mut stated = false;
    for value in subject.split('\u{1}') {
        if let Some(origin) = origin_of(value) {
            if origin == declared {
                return Owner::UnderTest;
            }
            stated = true;
        }
    }
    if stated {
        Owner::Elsewhere
    } else {
        Owner::Unstated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_is_read_without_parsing_a_url() {
        assert_eq!(origin_of("http://localhost:8080/assets/app.js"), Some("http://localhost:8080"));
        assert_eq!(
            origin_of("chrome-extension://abc/background.js"),
            Some("chrome-extension://abc")
        );
        assert_eq!(origin_of("https://example.com"), Some("https://example.com"));
        assert_eq!(origin_of("ResponseBodyLoader::OnStateChange"), None);
        assert_eq!(origin_of("://nohost"), None);
    }

    #[test]
    fn an_origin_is_found_inside_the_subject_the_reader_assembles() {
        // The one shape that actually occurs. An anchored match returns None here, and a filter
        // whose classifier always returns None files every finding as unowned while looking
        // perfectly correct in isolation.
        assert_eq!(
            origin_of("data.url=\"http://localhost:8080/assets/app.js\""),
            Some("http://localhost:8080"),
        );
        assert_eq!(
            origin_of("args.data.url=\"chrome-extension://ceff/SiteFactory.mjs\""),
            Some("chrome-extension://ceff"),
        );
        assert_eq!(origin_of("data.available=1024"), None);
    }

    #[test]
    fn work_naming_the_declared_origin_is_under_test() {
        let subject = "data.url=\"http://localhost:8080/app.js\"";
        assert_eq!(owner_of(subject, "http://localhost:8080"), Owner::UnderTest);
    }

    #[test]
    fn work_naming_only_another_origin_belongs_elsewhere() {
        let subject = "data.url=\"chrome-extension://ceff/SiteFactory.mjs\"";
        assert_eq!(owner_of(subject, "http://localhost:8080"), Owner::Elsewhere);
    }

    #[test]
    fn a_request_the_product_makes_to_a_third_party_stays_under_test() {
        let subject =
            "data.url=\"https://api.example.com/v1\"\u{1}data.frame_url=\"http://localhost:8080/\"";
        assert_eq!(
            owner_of(subject, "http://localhost:8080"),
            Owner::UnderTest,
            "naming the product anywhere must win, so the filter can only ever fail conservatively",
        );
    }

    #[test]
    fn work_naming_no_origin_is_unstated_rather_than_elsewhere() {
        assert_eq!(owner_of("data.available=1024", "http://localhost:8080"), Owner::Unstated);
        assert_eq!(owner_of("", "http://localhost:8080"), Owner::Unstated);
    }
}

//! How one producer spells the things every producer has.
//!
//! critpath reasons about intervals and edges and knows no technology. But "something arrived from
//! outside" and "something reached the screen" are not phases the Trace Event Format defines, and
//! every producer spells them differently. That spelling is knowledge about the EMITTER, not about
//! the application, so it lives here as data an operator can point at rather than as conditionals
//! scattered through the reader.
//!
//! What a vocabulary may contain: names and categories the producer uses. What it may never
//! contain: a duration cutoff, a weighting, a list of subjects to ignore, or anything naming a
//! framework. Those are judgements, and one of them anywhere voids the claim that a finding
//! survived the evidence rather than a rule.

/// The words one producer uses for events every producer emits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vocabulary {
    /// What this producer calls the trace it is.
    pub name: &'static str,
    /// Event names that carry something arriving from outside the program.
    pub stimulus: &'static [&'static str],
    /// Field on such an event naming which kind of thing arrived.
    pub stimulus_kind: &'static str,
    /// Kinds of arrival that came from a person rather than from the program itself.
    ///
    /// A producer emits one event name for every dispatched event, so a page finishing loading and
    /// a person clicking arrive spelled the same. Only the kind separates them, and on a real
    /// capture 333 dispatches held 0 of these -- which is the difference between "your menu is
    /// fine" and "nobody opened your menu".
    pub from_a_person: &'static [&'static str],
    /// Event names stating that something reached the screen.
    pub presentation: &'static [&'static str],
}

impl Vocabulary {
    /// The vocabulary of Chromium's tracing, which is what a browser writes.
    pub const CHROME: Self = Self {
        name: "chrome",
        stimulus: &["EventDispatch", "EventTiming"],
        stimulus_kind: "type",
        from_a_person: &[
            "click",
            "auxclick",
            "dblclick",
            "mousedown",
            "mouseup",
            "pointerdown",
            "pointerup",
            "touchstart",
            "touchend",
            "keydown",
            "keyup",
            "keypress",
            "input",
            "change",
            "contextmenu",
            "wheel",
            "submit",
        ],
        presentation: &["FramePresented", "firstContentfulPaint", "largestContentfulPaint"],
    };

    /// A vocabulary that recognises nothing, for a producer whose spelling is not known.
    ///
    /// Not a fallback that guesses: a recording read with this can still answer why it finished,
    /// and is refused for anything needing a stimulus, which is the correct outcome when the
    /// reader genuinely cannot tell what a stimulus looks like.
    pub const UNKNOWN: Self = Self {
        name: "unknown",
        stimulus: &[],
        stimulus_kind: "",
        from_a_person: &[],
        presentation: &[],
    };

    /// Every vocabulary an operator can name.
    pub const ALL: &'static [Self] = &[Self::CHROME, Self::UNKNOWN];

    /// Look one up by the word an operator would type.
    #[must_use]
    pub fn named(word: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|known| known.name == word)
    }

    /// Whether this event name is one the producer uses for an arrival.
    #[must_use]
    pub fn is_stimulus(&self, name: &str) -> bool {
        self.stimulus.contains(&name)
    }

    /// Whether this kind of arrival came from a person.
    #[must_use]
    pub fn is_from_a_person(&self, kind: &str) -> bool {
        self.from_a_person.contains(&kind)
    }

    /// Whether this event name states that something reached the screen.
    #[must_use]
    pub fn is_presentation(&self, name: &str) -> bool {
        self.presentation.contains(&name)
    }
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self::CHROME
    }
}

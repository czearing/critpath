//! What a recording was made to answer, and whether it can.
//!
//! A measurement engine that returns an empty finding list when the evidence was never captured is
//! worse than one that returns nothing at all: absence of findings and absence of evidence read
//! identically, and silence is the output people act on. So a question is asked explicitly, its
//! admissibility is checked against a census of what the recording actually contains, and an
//! inadmissible question is refused by name.

use fitkit_core::Refusal;

use crate::{ActivityId, Micros};

/// One moment when something arrived from a person, and the work that ran because of it.
///
/// Kept as evidence rather than as a count, because "was anything interacted with" and "how long
/// did that interaction take to answer" are the same question asked at two resolutions, and only
/// the second needs the moment itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrival {
    /// When the producer said it arrived.
    pub at: Micros,
    /// What kind of thing arrived, in the producer's own word for it.
    pub kind: String,
    /// The interval the producer recorded for handling it.
    ///
    /// [`None`] when the producer stated the arrival but recorded no interval for it, which is
    /// evidence of an interaction whose cost was never measured. That is a different fact from an
    /// interaction that was fast, and collapsing the two would be the same mistake as reporting an
    /// empty finding list for a recording that holds no interactions at all.
    pub activity: Option<ActivityId>,
    /// The producer's own identity for the physical interaction this belongs to.
    ///
    /// One press of one finger emits several events, and a producer that groups them says so. Two
    /// arrivals sharing this are one interaction and must be reported once; [`None`] means the
    /// producer stated no grouping for this arrival.
    pub interaction: Option<i64>,
    /// The split the producer stated for this arrival, when it stated one.
    pub phases: Option<Phases>,
}

/// Where an interaction's latency went, as the producer itself measured it.
///
/// Interaction to Next Paint is defined as input delay plus processing plus presentation delay,
/// and a producer that states all three has answered the question directly. The three are recorded
/// separately because they are three different repairs: input delay is the main thread being busy
/// when the finger landed, processing is the handler, and presentation delay is everything between
/// the handler returning and the pixels changing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phases {
    /// From the input reaching the machine to the program's handlers starting.
    pub input_delay: Micros,
    /// The program's own handlers.
    pub processing: Micros,
    /// From the handlers returning to the frame that answered the interaction.
    pub presentation_delay: Micros,
    /// The whole latency, which is what a person felt.
    pub latency: Micros,
}

impl Phases {
    /// Which of the three took the longest, named as the producer's own phase.
    #[must_use]
    pub fn largest(&self) -> (&'static str, Micros) {
        let mut worst = ("waiting for the main thread before the handler ran", self.input_delay);
        if self.processing > worst.1 {
            worst = ("running the program's own handlers", self.processing);
        }
        if self.presentation_delay > worst.1 {
            worst = ("waiting for the screen after the handlers returned", self.presentation_delay);
        }
        worst
    }
}

/// What the recording is being asked about.
///
/// This is not a mode that changes how anything is measured. The same graph, the same chain and
/// the same rules apply to every question; what changes is which evidence must be present before
/// an answer means anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Question {
    /// Why the recording finished when it did.
    ///
    /// Admissible for any trace that produced a chain, which is why it is the default: it presumes
    /// nothing about how the recording was made.
    #[default]
    Finish,
    /// How the product answered what the operator did to it.
    ///
    /// Presumes the operator actually did something. That presumption is exactly what must be
    /// checked, because a recording of an idle page yields a confident, detailed and entirely
    /// irrelevant report about loading.
    Response,
}

impl Question {
    /// The word an operator uses for this question.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Finish => "finish",
            Self::Response => "response",
        }
    }
}

/// What a recording was found to contain.
///
/// A census, not a judgement. Counting is done in the pass that already reads every event, so
/// asking these questions later costs nothing and cannot be skipped for speed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    /// Moments where something arrived from outside the program.
    ///
    /// Without one of these no interaction happened, and nothing about responsiveness can be said.
    pub stimuli: usize,
    /// Moments where the program put something on screen.
    ///
    /// Responsiveness is measured to a presentation, so a recording holding stimuli but no
    /// presentations can time the handler and never the thing the operator waited for.
    pub presentations: usize,
    /// Interactions the producer measured itself, hardware timestamp to answering frame.
    ///
    /// A producer that states one has proved something reached the screen more directly than a
    /// separately spelled presentation event does, so this satisfies the same evidence.
    pub stated_interactions: usize,
    /// Every origin the recording names, most-mentioned first.
    ///
    /// Kept whole rather than reduced to a guess. Which origin is under test is NOT derivable: on
    /// a real capture the first main-frame commit belonged to a browser extension, so the obvious
    /// heuristic picks the wrong one. The operator declares it and this list is what verifies the
    /// declaration.
    pub origins: Vec<(String, usize)>,
}

impl Recording {
    /// Whether the recording names this origin at all.
    #[must_use]
    pub fn names(&self, origin: &str) -> bool {
        self.origins.iter().any(|(seen, _)| seen == origin)
    }

    /// The origins worth suggesting when a declared one was not found.
    #[must_use]
    pub fn suggestions(&self) -> String {
        self.origins
            .iter()
            .take(4)
            .map(|(origin, count)| format!("{origin} ({count})"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A question, together with the origin whose code the answer is supposed to be about.
///
/// The origin is a declaration and never a filter list. It says which of the origins present in
/// the recording is the thing under test; ownership of every subject is then DERIVED from the
/// script origin the trace already states. The engine still holds no list of function names, no
/// framework names and no cutoff, and it never learns that a given symbol belongs to the browser
/// rather than to the product -- it observes that the symbol has no script origin, which is the
/// same fact without the knowledge.
#[derive(Clone, Debug, Default)]
pub struct Asked {
    /// What is being asked.
    pub question: Question,
    /// The origin under test, when the operator declared one.
    pub origin: Option<String>,
}

impl Asked {
    /// Ask why the recording finished when it did.
    #[must_use]
    pub const fn finish() -> Self {
        Self { question: Question::Finish, origin: None }
    }

    /// Ask how the product answered the operator.
    #[must_use]
    pub const fn response() -> Self {
        Self { question: Question::Response, origin: None }
    }

    /// Name the origin under test.
    #[must_use]
    pub fn about(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// Whether this recording can answer this question.
    ///
    /// # Errors
    ///
    /// A refusal naming the evidence that is absent, in terms whose remedy is a different
    /// recording rather than a different flag. This is the whole point: the operator must be able
    /// to tell "I looked and it is fine" from "I could not look". What was present instead is not
    /// squeezed into the reason -- a refusal carries a fixed sentence so it cannot be built from
    /// the very data it is refusing to trust -- so the caller prints [`Recording::suggestions`]
    /// beside it.
    pub fn admits(&self, recording: &Recording) -> Result<(), Refusal> {
        if let Some(origin) = &self.origin {
            if !recording.names(origin) {
                return Err(Refusal::uninformative(
                    "the recording never names the origin declared under test, so nothing in it \
                     is known to be that origin's code",
                ));
            }
        }
        if self.question == Question::Response {
            if recording.stimuli == 0 {
                return Err(Refusal::uninformative(
                    "the recording holds no moment where anything arrived from a person, so no \
                     interaction was performed and none can be judged; record again while doing \
                     the thing in question",
                ));
            }
            if recording.presentations == 0 && recording.stated_interactions == 0 {
                return Err(Refusal::uninformative(
                    "the recording holds interactions but never states when anything reached the \
                     screen, so a handler can be timed and the wait the operator felt cannot; \
                     record again with presentation reporting enabled",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> Recording {
        Recording {
            stimuli: 0,
            presentations: 48,
            stated_interactions: 0,
            origins: vec![("http://localhost:8080".to_owned(), 840)],
        }
    }

    #[test]
    fn an_idle_recording_cannot_answer_for_an_interaction() {
        let refusal = Asked::response().admits(&idle()).expect_err("idle holds no stimulus");
        assert!(
            refusal.to_string().contains("no interaction was performed"),
            "the refusal must say the evidence is absent, not that nothing is wrong: {refusal}",
        );
    }

    #[test]
    fn the_same_idle_recording_still_answers_for_the_finish() {
        assert!(
            Asked::finish().admits(&idle()).is_ok(),
            "one unanswerable question must not suppress an answerable one",
        );
    }

    #[test]
    fn an_undeclared_origin_is_refused_rather_than_quietly_matching_nothing() {
        let refusal = Asked::finish()
            .about("http://localhost:3000")
            .admits(&idle())
            .expect_err("that origin is absent");
        assert!(
            refusal.to_string().contains("never names the origin"),
            "a typo must be refused, not silently match nothing: {refusal}",
        );
        assert!(
            idle().suggestions().contains("http://localhost:8080"),
            "and the operator must be shown what was actually there",
        );
    }

    #[test]
    fn interactions_without_a_presentation_are_refused_separately() {
        let recording =
            Recording { stimuli: 3, presentations: 0, stated_interactions: 0, ..idle() };
        let refusal = Asked::response().admits(&recording).expect_err("no presentation");
        assert!(
            refusal.to_string().contains("reached the screen"),
            "the two missing-evidence cases must be distinguishable: {refusal}",
        );
    }
}

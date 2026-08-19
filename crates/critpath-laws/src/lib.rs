//! Rules that explain a critical path, and the gate each one speaks through.
//!
//! Every rule here fires on repetition, on emptiness, or on a comparison against something
//! measured in the same trace. None of them holds a tuned number, because a threshold is a claim
//! about a machine, a network and a workload that the trace never made. A rule that cannot be
//! stated without a constant does not belong in this crate.

use critpath_core::{ActivityId, Graph, Micros, Owner};
use critpath_graph::CriticalPath;
use fitkit_core::{Evidence, Refusal, Span};
use fitkit_ledger::{ask, Citation};

mod laws;
mod repair;

pub use repair::{choose, Repair};

/// What the rules read: a trace and the chain recovered from it.
#[derive(Clone, Copy, Debug)]
pub struct Observation<'a> {
    /// Everything that was read.
    pub graph: &'a Graph,
    /// The chain that determined when the work finished.
    pub path: &'a CriticalPath,
    /// The origin the operator declared under test, when one was declared.
    ///
    /// Carried into the rules' input rather than applied to their output by the caller, so that
    /// the one place findings are produced is also the one place they are attributed. A caller
    /// that filtered afterwards could forget to, and a forgotten filter here means another
    /// program's cost billed to this one.
    pub declared: Option<&'a str>,
}

impl Observation<'_> {
    /// Elapsed time attributable to the chain.
    pub fn total(&self) -> Micros {
        self.path.total()
    }
}

/// The timeline window `[from, until)` as a span of microseconds since the recording began.
///
/// A span is a unitless region of the problem, and for a trace the problem is the timeline, so a
/// finding cites the stretch of it the finding was measured over. Timestamps before the origin are
/// clamped to it rather than wrapped, since a source that records against an epoch of its own
/// would otherwise produce a span covering most of the address space.
pub(crate) fn window(from: Micros, until: Micros) -> Span {
    let index = |value: Micros| usize::try_from(value.max(0)).unwrap_or(usize::MAX);
    Span::new(index(from), index(until))
}

/// One thing a rule proved about the chain.
///
/// A cost is carried as [`Evidence`] rather than as a number: the span of the timeline it was
/// measured over and the trust the intervals behind it were recorded with. A bare number is where
/// a tuned constant gets in, with nothing behind it and no way to ask what it was measured from.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// The same work, by category and name, appears more than once on the chain.
    ///
    /// Repetition on the chain is waste by definition: the second occurrence delayed the finish
    /// and produced what the first already had.
    RepeatedWork {
        /// Category, name, and what the source said the work was done to.
        ///
        /// The subject is carried into the finding rather than dropped, because it is the only
        /// part a person can act on: the name says a resource was appended to, the subject says
        /// which file it was.
        key: (String, String, String),
        /// Where they were, in chain order.
        occurrences: Vec<ActivityId>,
        /// Time the repeats added to the chain, over the stretch of the timeline they ran in.
        cost: Evidence<Micros>,
    },
    /// The chain waited while nothing ran anywhere in the trace.
    ///
    /// Not contention and not slow code. Time in which the machine had nothing to do, which is
    /// always a dependency that could have been issued earlier.
    DeadWait {
        /// The activity that was waiting to start.
        before: ActivityId,
        /// The work it was waiting for, when the chain came from somewhere.
        ///
        /// A wait of several seconds with nothing named is a number, not a finding. What ends a
        /// wait is the work the chain arrived from, which the graph already holds.
        waited_on: Option<ActivityId>,
        /// Whether the source stated that dependency rather than it following from track order.
        ///
        /// False makes this a scheduling wait: the track was idle and nothing in the trace says
        /// what for, so the wait is reported as unattributed instead of being given a subject it
        /// has not earned.
        stated: bool,
        /// How long nothing ran, over exactly the gap it ran in.
        cost: Evidence<Micros>,
    },
    /// The largest activity in the trace is not on the chain.
    ///
    /// Stated because it is the finding a ranked profile gets wrong: this work can be deleted
    /// entirely and the finish will not move.
    OffPath {
        /// The largest activity.
        activity: ActivityId,
        /// How long it ran for.
        duration: Micros,
        /// At most how much longer it could run before it becomes the constraint.
        ///
        /// The difference between "this does not matter" and "this does not matter yet". An upper
        /// bound, because the recorded dependencies are a subset of the real ones and adding one
        /// can only take room away.
        room: Micros,
    },
    /// Something was in flight while the chain sat waiting.
    ///
    /// The finding that turns a wait into a subject. A chain can spend seconds waiting while every
    /// rule about work stays silent, because nothing was running to blame -- and the report then
    /// says only that the program waited, which no one can act on.
    ///
    /// What is proved here is overlap and nothing more: this work was in flight across that much
    /// of the chain's waiting. Whether the chain was waiting *for* it is a claim the trace does not
    /// make, because no dependency between them was ever stated, so it carries no cost and can
    /// never be selected as a repair. It is reported because a person who knows what the work was
    /// can settle in seconds a question the trace cannot settle at all.
    WaitedWhileInFlight {
        /// The work that was in flight.
        during: ActivityId,
        /// How much of the chain's waiting it covered.
        overlap: Micros,
        /// How many separate waits on the chain it spanned.
        waits: usize,
    },
}

impl Finding {
    /// Time the chain would lose if this were fully resolved.
    ///
    /// Zero for anything that costs the chain nothing, so it can never be selected as a repair.
    pub fn cost(&self) -> Micros {
        self.charge().map_or(0, |charge| charge.value)
    }

    /// The measurement behind [`cost`](Self::cost), for a finding that costs the chain time.
    ///
    /// [`None`] where the cost is zero, since there is no measurement to cite for a claim that was
    /// never made. This is what a selection weighs, so the weight arrives carrying the region it
    /// speaks for and the trust it is held with rather than as a number on its own.
    pub fn charge(&self) -> Option<&Evidence<Micros>> {
        match self {
            Self::RepeatedWork { cost, .. } | Self::DeadWait { cost, .. } => Some(cost),
            Self::OffPath { .. } | Self::WaitedWhileInFlight { .. } => None,
        }
    }

    /// The magnitude the finding's own sentence quotes.
    ///
    /// Used only to order a report, never to claim a saving. Two findings that cost the chain
    /// nothing provable are still not equally interesting, and without this the largest thing in
    /// the trace prints below the smallest.
    pub fn evidence(&self) -> Micros {
        match self {
            Self::RepeatedWork { cost, .. } | Self::DeadWait { cost, .. } => cost.value,
            Self::OffPath { duration, .. } => *duration,
            Self::WaitedWhileInFlight { overlap, .. } => *overlap,
        }
    }

    /// Whose code this finding is about, relative to the origin declared under test.
    ///
    /// Read from the subjects the finding already names, so a finding is attributed to exactly the
    /// work it would have someone change. The strongest evidence present wins: any subject naming
    /// the declared origin makes the whole finding the product's, since a finding about a mix of
    /// the product's work and somebody else's is still the product's to answer for.
    ///
    /// A dead wait is attributed to what the chain waited *on* where the trace stated it, because
    /// that is the work a repair would move; the waiting activity itself is a symptom and is only
    /// consulted when nothing was stated to wait on.
    pub fn owner(&self, graph: &Graph, declared: &str) -> Owner {
        let subject_of = |id: &ActivityId| {
            graph.activities.get(*id).and_then(|a| a.subject.as_deref()).unwrap_or_default()
        };
        let mut stated = false;
        let mut check = |subject: &str| match critpath_core::owner_of(subject, declared) {
            Owner::UnderTest => true,
            Owner::Elsewhere => {
                stated = true;
                false
            }
            Owner::Unstated => false,
        };
        let decided = match self {
            Self::RepeatedWork { key, .. } => check(&key.2),
            Self::DeadWait { before, waited_on, .. } => {
                check(subject_of(waited_on.as_ref().unwrap_or(before)))
            }
            Self::OffPath { activity, .. } => check(subject_of(activity)),
            Self::WaitedWhileInFlight { during, .. } => check(subject_of(during)),
        };
        if decided {
            Owner::UnderTest
        } else if stated {
            Owner::Elsewhere
        } else {
            Owner::Unstated
        }
    }
}

/// The trace format every rule ultimately reads through.
pub const FORMAT: Citation = Citation {
    key: "TraceEventFormat",
    source: "Trace Event Format, chromium/src/docs/trace_event_format.md",
};

/// The result that makes a chain worth recovering at all.
pub const CRITICAL_PATH: Citation = Citation {
    key: "WProf2013",
    source: "Wang et al., Demystifying Page Load Performance with WProf, NSDI 2013",
};

/// A rule that declined to answer, and why.
#[derive(Clone, Debug)]
pub struct Silence {
    /// The rule that stayed silent.
    pub rule: &'static str,
    /// What it would have needed.
    pub because: Refusal,
}

/// What the rules could and could not prove.
#[derive(Clone, Debug, Default)]
pub struct Proof {
    /// Everything proved about the code under test.
    ///
    /// When no origin was declared this is everything proved, because with nothing declared there
    /// is nothing to attribute against and quietly guessing an owner is the failure this exists to
    /// prevent.
    pub findings: Vec<Finding>,
    /// Proved, but about a program other than the one under test.
    ///
    /// Set aside rather than deleted. A filter that silently removes evidence and one that had
    /// nothing to remove produce the same report, and the operator has to be able to tell them
    /// apart.
    pub withheld: Vec<Finding>,
    /// Proved, but about work the trace states no origin for.
    ///
    /// Browser, runtime and kernel internals land here, and so does anything the producer simply
    /// did not label. Kept separate from both other lists because it is a weaker claim than
    /// either: not shown to be the product's, and not shown not to be.
    pub unattributed: Vec<Finding>,
    /// Rules that refused, kept so silence is never mistaken for a clean result.
    pub silent: Vec<Silence>,
}

impl Proof {
    /// How many findings were proved in total, wherever they were filed.
    pub fn proved(&self) -> usize {
        self.findings.len() + self.withheld.len() + self.unattributed.len()
    }
    /// Whether a clean bill of health can be believed.
    ///
    /// No findings and no silent rules means the trace was searched in full and nothing was found.
    /// No findings while a rule refused means only that nobody looked.
    pub fn is_conclusive(&self) -> bool {
        self.silent.is_empty()
    }
}

/// Everything the rules can prove about this observation.
///
/// A rule that refuses does not silence the others. Each states its own conditions, so a trace
/// that defeats one can still be answered by the rest, and the refusals are returned alongside the
/// findings rather than in place of them.
pub fn findings(observation: Observation<'_>) -> Proof {
    let mut proof = Proof::default();
    ask_into(&laws::RepeatedWork::default(), &observation, "repeated work", &mut proof);
    ask_into(&laws::DeadWait::default(), &observation, "dead wait", &mut proof);
    ask_into(&laws::OffPath::default(), &observation, "off-path dominance", &mut proof);
    ask_into(
        &laws::WaitedWhileInFlight::default(),
        &observation,
        "in flight while waiting",
        &mut proof,
    );
    // Ordered by what each costs the chain, because a report that leads with the cheapest finding
    // makes the reader do the ranking the tool exists to do. Where nothing is provably owed, the
    // larger measurement leads, so proof still outranks magnitude and magnitude outranks nothing.
    proof.findings.sort_by_key(|finding| core::cmp::Reverse((finding.cost(), finding.evidence())));
    // Attribution last, over an already ordered list, so each list keeps that order and the sort
    // is paid once rather than three times.
    if let Some(declared) = observation.declared {
        let mut under_test = Vec::new();
        for finding in core::mem::take(&mut proof.findings) {
            match finding.owner(observation.graph, declared) {
                Owner::UnderTest => under_test.push(finding),
                Owner::Elsewhere => proof.withheld.push(finding),
                Owner::Unstated => proof.unattributed.push(finding),
            }
        }
        proof.findings = under_test;
    }
    proof
}

/// Ask one rule and file its answer, whichever kind it is.
fn ask_into<'a, L>(law: &L, observation: &Observation<'a>, rule: &'static str, proof: &mut Proof)
where
    L: fitkit_ledger::Law<Input = Observation<'a>, Output = Vec<Finding>>,
{
    match ask(law, observation) {
        Ok(found) => proof.findings.extend(found),
        Err(because) => proof.silent.push(Silence { rule, because }),
    }
}

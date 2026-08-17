//! Rules that explain a critical path, and the gate each one speaks through.
//!
//! Every rule here fires on repetition, on emptiness, or on a comparison against something
//! measured in the same trace. None of them holds a tuned number, because a threshold is a claim
//! about a machine, a network and a workload that the trace never made. A rule that cannot be
//! stated without a constant does not belong in this crate.

use critpath_core::{ActivityId, Graph, Micros};
use critpath_graph::CriticalPath;
use fitkit_core::Refusal;
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
}

impl Observation<'_> {
    /// Elapsed time attributable to the chain.
    pub fn total(&self) -> Micros {
        self.path.total()
    }
}

/// One thing a rule proved about the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// The same work, by category and name, appears more than once on the chain.
    ///
    /// Repetition on the chain is waste by definition: the second occurrence delayed the finish
    /// and produced what the first already had.
    RepeatedWork {
        /// Category and name shared by the occurrences.
        key: (String, String),
        /// Where they were, in chain order.
        occurrences: Vec<ActivityId>,
        /// Time the repeats added to the chain.
        cost: Micros,
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
        /// How long nothing ran.
        cost: Micros,
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
}

impl Finding {
    /// Time the chain would lose if this were fully resolved.
    ///
    /// Zero for anything that costs the chain nothing, so it can never be selected as a repair.
    pub fn cost(&self) -> Micros {
        match self {
            Self::RepeatedWork { cost, .. } | Self::DeadWait { cost, .. } => *cost,
            Self::OffPath { .. } => 0,
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
    /// Everything proved.
    pub findings: Vec<Finding>,
    /// Rules that refused, kept so silence is never mistaken for a clean result.
    pub silent: Vec<Silence>,
}

impl Proof {
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
    // Ordered by what each costs the chain, because a report that leads with the cheapest finding
    // makes the reader do the ranking the tool exists to do.
    proof.findings.sort_by_key(|finding| core::cmp::Reverse(finding.cost()));
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

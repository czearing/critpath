//! Rules that explain a critical path, and the gate each one speaks through.
//!
//! Every rule here fires on repetition, on emptiness, or on a comparison against something
//! measured in the same trace. None of them holds a tuned number, because a threshold is a claim
//! about a machine, a network and a workload that the trace never made. A rule that cannot be
//! stated without a constant does not belong in this crate.

use critpath_core::{ActivityId, Graph, Micros};
use critpath_graph::CriticalPath;
use fitkit_core::Answer;
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

/// Everything the rules can prove about this observation.
///
/// # Errors
///
/// A refusal from the first rule whose conditions the trace does not meet. Coverage is the usual
/// one: a trace with unread events may be missing exactly the activity a rule would have named, so
/// silence from an incomplete trace is indistinguishable from a clean result and is refused.
pub fn findings(observation: Observation<'_>) -> Answer<Vec<Finding>> {
    let mut found = ask(&laws::RepeatedWork::default(), &observation)?;
    found.extend(ask(&laws::DeadWait::default(), &observation)?);
    found.extend(ask(&laws::OffPath::default(), &observation)?);
    Ok(found)
}

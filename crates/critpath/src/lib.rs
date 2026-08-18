//! Why a program finished when it did.
//!
//! Give critpath a trace in Trace Event Format and it recovers the chain of dependent work that
//! determined the finish, proves what is wrong with that chain, and says how much room there is
//! before fixing it stops paying. It never starts anything, never drives a browser and never
//! learns a framework: whatever produced the trace already knows those things, and the analysis is
//! the same whether the trace came from React, a shader compiler or a build system.
//!
//! When the evidence will not carry a verdict, every entry point returns a [`Refusal`] rather than
//! a number. That is the design, not a limitation.
//!
//! [`Refusal`]: fitkit_core::Refusal

use critpath_core::{Coverage, Graph};
use critpath_graph::{critical_path, CriticalPath};
use critpath_laws::{choose, findings, Finding, Observation, Proof, Repair};
use fitkit_core::{Answer, Refusal};

mod render;

pub use critpath_core::{Asked, EdgeKind, Question, Recording};
pub use critpath_laws::{Finding as Proven, Silence};
pub use critpath_trace::{read_as, ParseError, Vocabulary};
pub use render::report;

/// Everything critpath concluded about one trace.
#[derive(Clone, Debug)]
pub struct Analysis {
    /// The chain that decided the finish.
    pub path: CriticalPath,
    /// What the rules proved, and which of them declined to speak.
    pub proof: Proof,
    /// What was left unread, carried rather than dropped.
    pub coverage: Coverage,
    /// The trace the conclusions refer to.
    pub graph: Graph,
}

impl Analysis {
    /// What the rules proved.
    pub fn findings(&self) -> &[Finding] {
        &self.proof.findings
    }

    /// Choose repairs worth making, given how many changes are affordable.
    ///
    /// # Errors
    ///
    /// A refusal when nothing on offer costs the chain time, or when the chain has no margin left.
    pub fn repair(&self, budget: usize) -> Answer<Repair> {
        choose(&self.proof.findings, budget, self.path.margin)
    }
}

/// Read a trace and analyse it.
///
/// # Errors
///
/// A refusal when the bytes are not a trace, or when the chain itself cannot be recovered. A rule
/// that declines is not an error: it is recorded in [`Proof::silent`], so one unanswerable
/// question never suppresses the answers to the others.
pub fn analyse(bytes: &[u8]) -> Answer<Analysis> {
    analyse_for(bytes, &Asked::finish(), Vocabulary::default())
}

/// Read a trace and analyse it, for a stated question about a stated origin.
///
/// The question does not change how anything is measured -- the same graph, chain and rules apply
/// to all of them. What it changes is whether the recording is allowed to answer at all. A
/// recording of an idle page will happily produce a confident, detailed report about loading when
/// what was asked was whether a menu is slow, and that report is worse than silence.
///
/// # Errors
///
/// A refusal when the bytes are not a trace, when the chain cannot be recovered, or when the
/// recording lacks the evidence the question presumes.
pub fn analyse_for(bytes: &[u8], asked: &Asked, vocabulary: Vocabulary) -> Answer<Analysis> {
    let mut graph = critpath_trace::read_as(bytes, vocabulary).map_err(|error| match error {
        ParseError::NotJson(_) => Refusal::uninformative(
            "the input is not JSON; a browser writes a protobuf trace unless asked for JSON",
        ),
        ParseError::NotATrace => {
            Refusal::uninformative("the JSON carries no trace events this reader understands")
        }
    })?;
    // Admissibility is checked before any analysis, so an inadmissible question costs nothing and
    // cannot be answered by accident.
    asked.admits(&graph.recording)?;
    graph.coverage.contradicted += critpath_graph::contradictions(&graph);
    let path = critical_path(&graph)?;
    let proof = findings(Observation { graph: &graph, path: &path });
    Ok(Analysis { coverage: graph.coverage, proof, path, graph })
}

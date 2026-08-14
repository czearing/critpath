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
use critpath_laws::{choose, findings, Finding, Observation, Repair};
use fitkit_core::{Answer, Refusal};

mod render;

pub use critpath_laws::Finding as Proven;
pub use critpath_trace::ParseError;
pub use render::report;

/// Everything critpath concluded about one trace.
#[derive(Clone, Debug)]
pub struct Analysis {
    /// The chain that decided the finish.
    pub path: CriticalPath,
    /// What the rules proved about it.
    pub findings: Vec<Finding>,
    /// What was left unread, carried rather than dropped.
    pub coverage: Coverage,
    /// The trace the conclusions refer to.
    pub graph: Graph,
}

impl Analysis {
    /// Choose repairs worth making, given how many changes are affordable.
    ///
    /// # Errors
    ///
    /// A refusal when nothing on offer costs the chain time, or when the chain has no margin left.
    pub fn repair(&self, budget: usize) -> Answer<Repair> {
        choose(&self.findings, budget, self.path.margin)
    }
}

/// Read a trace and analyse it.
///
/// # Errors
///
/// A refusal when the bytes are not a trace, when the chain cannot be recovered, or when a rule
/// declines to speak about what it was given.
pub fn analyse(bytes: &[u8]) -> Answer<Analysis> {
    let mut graph = critpath_trace::read(bytes)
        .map_err(|_| Refusal::uninformative("the input is not a trace this reader understands"))?;
    graph.coverage.contradicted += critpath_graph::contradictions(&graph);
    let path = critical_path(&graph)?;
    let found = findings(Observation { graph: &graph, path: &path })?;
    Ok(Analysis { coverage: graph.coverage, findings: found, path, graph })
}

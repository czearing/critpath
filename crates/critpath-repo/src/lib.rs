//! A repository read as a weighted dependency graph, with nothing built and nothing run.
//!
//! Nothing here knows a language, a package manager or a framework. A component is a named unit
//! of shipped weight; an edge is a dependency one component declares on another. Anything that
//! can state those two facts can be analysed -- a web app's installed modules, a game's asset
//! packs, a service's vendored crates -- so no rule below is ever allowed to name a technology.
//!
//! The reasoning is the same shape as the trace side of this tool: reachability is a least fixed
//! point, and what a component actually holds in place is a dominator subtree. Neither needs the
//! program to execute, which is the whole point.

use std::collections::HashMap;

use fitkit_core::Confidence;

mod dominate;
mod npm;
mod plan;
mod unused;

pub use npm::{read, Refusal};
pub use plan::{Plan, Removal};
pub use unused::{styles as unused_styles, Undecidable, Unused};

/// Index of a component inside a [`Repo`].
pub type ComponentId = usize;

/// One named unit of weight that something may depend on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    /// Name as the repository states it.
    pub name: String,
    /// Directory the weight was measured in, as an absolute path.
    pub directory: String,
    /// Bytes on disk, excluding any nested component's directory, because that is its own weight.
    pub weight: u64,
    /// Components this one declares a dependency on.
    pub declares: Vec<ComponentId>,
    /// Names it declares that no directory on disk answers to. Counted, never assumed absent.
    pub unresolved: Vec<String>,
    /// True when the component is part of the repository rather than something installed into it.
    pub owned: bool,
}

/// A repository, read.
#[derive(Clone, Debug, Default)]
pub struct Repo {
    /// Every component found, installed or owned.
    pub components: Vec<Component>,
    /// The component whose shipped weight is being asked about.
    pub entry: ComponentId,
    /// Names of every owned component, so an operator who named no entry can be shown the choices.
    pub entries: Vec<String>,
    /// What the read could not decide. Printed, never silently dropped.
    pub refusals: Vec<String>,
    /// Bytes of installed weight the read measured in total, reachable or not.
    pub installed: u64,
    by_name: HashMap<String, ComponentId>,
}

impl Repo {
    /// Looks a component up by the exact name the repository states.
    pub fn id_of(&self, name: &str) -> Option<ComponentId> {
        self.by_name.get(name).copied()
    }

    /// Weight, dominance and reachability, computed from the entry.
    ///
    /// Separate from the read because the read touches the disk once and this is a pure function
    /// of what it found, so it can be re-asked from a different entry without walking again.
    pub fn hold(&self) -> Held {
        dominate::hold(self)
    }

    /// The cheapest honest description of how much of this repository is even in play.
    ///
    /// Confidence is the share of declared dependencies that resolved to something on disk. An
    /// install that is half missing cannot support a claim about what it holds, and saying so is
    /// more useful than an answer computed over the half that happened to be there.
    pub fn confidence(&self) -> Confidence {
        let declared: usize = self.components.iter().map(|c| c.declares.len()).sum();
        let missing: usize = self.components.iter().map(|c| c.unresolved.len()).sum();
        let total = declared + missing;
        if total == 0 {
            return Confidence::FULL;
        }
        Confidence::new(declared as f64 / total as f64)
    }
}

/// What the entry reaches, and what each component holds in place.
#[derive(Clone, Debug, Default)]
pub struct Held {
    /// True for every component reachable from the entry by declared dependencies.
    pub reachable: Vec<bool>,
    /// The immediate dominator of each reachable component. The entry dominates itself.
    pub dominator: Vec<ComponentId>,
    /// Weight that becomes unreachable if this component does: its own plus all it alone holds.
    ///
    /// This is the number a removal actually delivers. A component's own weight is not, because a
    /// widely shared library loses nothing when one route to it goes.
    pub retained: Vec<u64>,
    /// Total weight reachable from the entry.
    pub reached: u64,
}

impl Held {
    /// Components the entry cannot reach at all, heaviest first.
    ///
    /// Installed and never referenced by anything the entry needs. The only claim made is
    /// reachability by declaration; whether the install itself can be pruned is a separate
    /// question this does not answer.
    pub fn unreachable(&self, repo: &Repo) -> Vec<ComponentId> {
        let mut out: Vec<ComponentId> = (0..repo.components.len())
            .filter(|&id| !self.reachable[id] && !repo.components[id].owned)
            .collect();
        out.sort_by_key(|&id| std::cmp::Reverse(repo.components[id].weight));
        out
    }

    /// The best set of removals within a budget of changes, and what the next one would add.
    pub fn plan(&self, repo: &Repo, budget: usize) -> Plan {
        plan::choose(self, repo, budget)
    }
}

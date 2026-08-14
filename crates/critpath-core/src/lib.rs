//! The vocabulary critpath reasons in.
//!
//! Nothing here knows a language, a framework or a browser. An activity is a named interval on a
//! track; an edge is a stated dependency between two of them. Anything that can emit those two
//! facts can be diagnosed, whether it is a React route, a three.js frame, a Go service or a shell
//! script, so no rule downstream is ever allowed to name a technology.

use fitkit_core::Confidence;

/// Microseconds since the trace clock started. The unit the source reports in.
pub type Micros = i64;

/// A serial execution context. Work on one track cannot overlap itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Track {
    /// Process the work ran in.
    pub pid: i64,
    /// Thread the work ran on.
    pub tid: i64,
}

/// Index of an activity inside a [`Graph`].
pub type ActivityId = usize;

/// One named interval of work.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    /// Name as the source reported it.
    pub name: String,
    /// Category as the source reported it. Empty when the source gave none.
    pub category: String,
    /// Where it ran.
    pub track: Track,
    /// When it started.
    pub start: Micros,
    /// When it ended.
    pub end: Micros,
    /// How far the interval itself is trusted. Zero means it decided nothing.
    pub confidence: Confidence,
}

impl Activity {
    /// Wall time the interval covers. Never negative.
    pub fn duration(&self) -> Micros {
        self.end.saturating_sub(self.start).max(0)
    }

    /// The key repetition is judged on: what ran, not when or where it ran.
    ///
    /// Two activities sharing a key are the same work done twice. Track and timing are excluded
    /// deliberately, since the same call on another thread is still the same call.
    pub fn key(&self) -> (&str, &str) {
        (self.category.as_str(), self.name.as_str())
    }

    /// Whether the interval can support a decision.
    pub fn is_informative(&self) -> bool {
        !self.confidence.is_zero() && self.end > self.start
    }
}

/// Why one activity depends on another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// The source stated the dependency, as a flow between two activities.
    Flow,
    /// Both activities ran on one track, which executes serially, so order is causality.
    Serial,
}

/// A stated dependency. `to` cannot begin its work until `from` has done its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The activity depended upon.
    pub from: ActivityId,
    /// The activity that waits.
    pub to: ActivityId,
    /// How the dependency was established.
    pub kind: EdgeKind,
}

/// What the reader could not account for.
///
/// Load bearing. A hole in the trace and a healthy trace produce the same findings, so the holes
/// are counted and carried rather than dropped, and a verdict is refused while any remain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Begin events that never received a matching end.
    pub unpaired: usize,
    /// Flow endpoints that fell outside every activity, so the edge could not be attached.
    pub unbound_flows: usize,
    /// Events the reader did not understand at all.
    pub unread: usize,
    /// Dependencies the clock denies: the source said one activity waited on another that had not
    /// started yet.
    ///
    /// Counted rather than dropped. Either the trace is inconsistent or the reader attached an
    /// endpoint to the wrong activity, and both mean a rule could be looking at the wrong chain.
    pub contradicted: usize,
}

impl Coverage {
    /// Whether every event in the source was accounted for.
    pub fn is_total(&self) -> bool {
        self.unpaired == 0 && self.unbound_flows == 0 && self.unread == 0 && self.contradicted == 0
    }

    /// Number of unaccounted events.
    pub fn holes(&self) -> usize {
        self.unpaired + self.unbound_flows + self.unread + self.contradicted
    }
}

/// Activities and the dependencies between them.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    /// Every interval read, indexed by [`ActivityId`].
    pub activities: Vec<Activity>,
    /// Every dependency read.
    pub edges: Vec<Edge>,
    /// What could not be read.
    pub coverage: Coverage,
}

impl Graph {
    /// Distinct tracks the activities ran on.
    pub fn tracks(&self) -> Vec<Track> {
        let mut seen: Vec<Track> = self.activities.iter().map(|a| a.track).collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }

    /// How many dependencies the source stated itself, rather than order implying them.
    pub fn stated_edges(&self) -> usize {
        self.edges.iter().filter(|e| e.kind == EdgeKind::Flow).count()
    }

    /// The activity holding the longest interval, if any is informative.
    pub fn longest(&self) -> Option<ActivityId> {
        self.activities
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_informative())
            .max_by_key(|(_, a)| a.duration())
            .map(|(id, _)| id)
    }
}

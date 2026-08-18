//! The vocabulary critpath reasons in.
//!
//! Nothing here knows a language, a framework or a browser. An activity is a named interval on a
//! track; an edge is a stated dependency between two of them. Anything that can emit those two
//! facts can be diagnosed, whether it is a React route, a three.js frame, a Go service or a shell
//! script, so no rule downstream is ever allowed to name a technology.

use fitkit_core::Confidence;

mod ask;

pub use ask::{Asked, Question, Recording};

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
    /// Whether this work may overlap other work on its track.
    ///
    /// Asynchronous work is correlated by an identifier rather than by a call stack, so it can
    /// overlap and can cross threads. Order on a track is therefore not causality for it, and it
    /// is kept out of serial ordering: only a stated flow may put it on a chain.
    pub concurrent: bool,
    /// What the source said this work was done *to*, if it said anything.
    ///
    /// The difference between a loop and a repeat. A name alone cannot tell them apart, because a
    /// request loop that fetches seventy different resources reports the same name seventy times
    /// and is doing seventy different things. What the source recorded alongside the name -- the
    /// url, the script, the file -- is the only evidence in a trace that two intervals did the
    /// same work rather than the same kind of work.
    pub subject: Option<String>,
    /// Whether the extent was correlated from separate moments rather than reported as an interval.
    ///
    /// A producer that records a lifetime as a start mark and a finish mark states a duration
    /// without ever stating an interval. Recovering it is worth doing, but the result is weaker
    /// evidence than an interval the source measured itself, and the two must never be confused.
    /// A rule that ranks magnitude -- "the largest thing here" -- may only weigh what was
    /// observed, because an inferred extent competes on a footing it did not earn.
    pub inferred: bool,
}

impl Activity {
    /// Wall time the interval covers. Never negative.
    pub fn duration(&self) -> Micros {
        self.end.saturating_sub(self.start).max(0)
    }

    /// The key repetition is judged on: what ran, not when or where it ran.
    ///
    /// Two activities sharing a key are the same *kind* of work. Track and timing are excluded
    /// deliberately, since the same call on another thread is still the same call.
    pub fn key(&self) -> (&str, &str) {
        (self.category.as_str(), self.name.as_str())
    }

    /// The key redundancy is judged on: the same work, on the same thing.
    ///
    /// [`None`] when the source recorded no subject, and a rule that cannot form this key must
    /// stay silent rather than fall back to the name. Naming alone would convict every loop in
    /// every program of repeating itself.
    pub fn identity(&self) -> Option<(&str, &str, &str)> {
        self.subject.as_deref().map(|subject| (self.category.as_str(), self.name.as_str(), subject))
    }

    /// Whether the interval can support a decision.
    pub fn is_informative(&self) -> bool {
        !self.confidence.is_zero() && self.end > self.start
    }

    /// Whether the interval may take part in deciding the chain.
    ///
    /// Stricter than [`Activity::is_informative`], and the distinction is load bearing. An extent
    /// correlated from separate moments is a real measurement and rules may read it, but a chain
    /// is an account of what caused the finish, and this reader inferred the correlation rather
    /// than the source stating it. Barring it from edges is not enough on its own: a longest path
    /// is a maximum, so a single wide inferred interval can be a chain all by itself, with no
    /// dependency needed. That is exactly how a correlation spanning the recording would come to
    /// be reported as the reason a program was slow.
    pub fn decides(&self) -> bool {
        self.is_informative() && !self.inferred
    }

    /// Whether the machine was busy with this work at any point between `from` and `to`.
    ///
    /// Deliberately looser than [`Activity::is_informative`]. Work whose end was never recorded
    /// cannot join a chain, but it was still running, and a rule that claims the machine was idle
    /// has to answer for it.
    pub fn overlaps(&self, from: Micros, to: Micros) -> bool {
        self.start < to && self.end > from && self.end > self.start
    }
}

/// What the reader could not account for.
///
/// Load bearing, and deliberately itemised. A hole and a healthy trace produce the same findings,
/// so the holes are counted and carried rather than dropped. They are kept apart by *what they
/// threaten*, because an observation window that closed mid-flight is missing evidence while an
/// event the reader could not read is ignorance, and treating the two alike either refuses every
/// real trace or trusts one it should not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Events the reader did not understand at all.
    ///
    /// Threatens everything. An unknown event could be any work, anywhere.
    pub unread: usize,
    /// Ends whose matching begin was never seen, so the work has no known start.
    pub unpaired: usize,
    /// Flow endpoints that fell outside every activity, so the edge could not be attached.
    ///
    /// Threatens chain membership: a missing edge can move work onto the chain.
    pub unbound_flows: usize,
    /// Dependencies the clock denies: the source said one activity waited on another that had not
    /// started yet.
    pub contradicted: usize,
    /// Work that had not finished when the trace stopped.
    ///
    /// Threatens nothing, because it is handled rather than ignored: the interval is held open to
    /// the end of the observation window, which is the most that can be claimed and never less
    /// than the truth. Counted so the report can say the window closed on live work.
    pub censored: usize,
}

impl Coverage {
    /// Whether every event in the source was accounted for.
    pub fn is_total(&self) -> bool {
        self.holes() == 0
    }

    /// Number of unaccounted events. Censored work is accounted for, so it is not counted here.
    pub fn holes(&self) -> usize {
        self.unread + self.unpaired + self.unbound_flows + self.contradicted
    }

    /// Whether anything threatens a claim about which work was running.
    ///
    /// Censored work is excluded: it is held open to the end of the window, so a rule asking
    /// whether the machine was busy already sees it.
    pub fn intervals_are_complete(&self) -> bool {
        self.unread == 0 && self.unpaired == 0
    }

    /// Whether anything threatens a claim about which work is on the chain.
    pub fn edges_are_complete(&self) -> bool {
        self.unread == 0 && self.unbound_flows == 0 && self.contradicted == 0
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
/// Superseded by the itemised [`Coverage`] above.
#[doc(hidden)]
pub type CoverageAlias = Coverage;

/// Activities and the dependencies between them.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    /// Every interval read, indexed by [`ActivityId`].
    pub activities: Vec<Activity>,
    /// Every dependency read.
    pub edges: Vec<Edge>,
    /// What could not be read.
    pub coverage: Coverage,
    /// What the recording was found to contain, for deciding which questions it can answer.
    pub recording: Recording,
}

impl Graph {
    /// Activity indices grouped by track, each group ordered by start then by longest first.
    ///
    /// Built once and shared. Every question about nesting, ordering or what encloses a moment is
    /// answered inside one track, and a real trace holds enough activities that asking those
    /// questions against the whole list is the difference between a second and an hour.
    pub fn by_track(&self) -> Vec<(Track, Vec<ActivityId>)> {
        let mut groups: Vec<(Track, Vec<ActivityId>)> = Vec::new();
        let mut order: Vec<ActivityId> = (0..self.activities.len()).collect();
        order.sort_unstable_by_key(|&id| {
            let activity = &self.activities[id];
            (activity.track, activity.start, core::cmp::Reverse(activity.end))
        });
        for id in order {
            let track = self.activities[id].track;
            match groups.last_mut() {
                Some((seen, group)) if *seen == track => group.push(id),
                _ => groups.push((track, vec![id])),
            }
        }
        groups
    }

    /// Distinct tracks that observed work ran on.
    ///
    /// Inferred intervals are excluded. They are not work on a track -- they are moments the
    /// producer recorded, correlated after the fact -- and they are already barred from serial
    /// ordering and from stating any dependency. Counting them as tracks would mean a trace whose
    /// network marks came from another process suddenly had several tracks with no order between
    /// them, and a reader would refuse a trace it had understood perfectly well a moment earlier.
    pub fn tracks(&self) -> Vec<Track> {
        let mut seen: Vec<Track> =
            self.activities.iter().filter(|a| !a.inferred).map(|a| a.track).collect();
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

    /// Time each activity spent doing something other than waiting on work nested inside it.    ///
    /// The difference between a thing that ran and a thing that did something. A task loop, a
    /// dispatcher or any other frame encloses the work it calls, so its interval already contains
    /// that work's cost; charging it again counts the same microsecond twice and makes the most
    /// generic name in the trace look like the most expensive. Self time is a fact about the
    /// intervals, not a judgement about which names are wrappers, so it separates the two without
    /// knowing anything about the framework, the runtime or the language that produced them.
    ///
    /// Nesting is read per track, since only work on one track can enclose other work. Concurrent
    /// intervals are allowed to overlap by the format, so they enclose nothing and are charged
    /// their whole duration.
    pub fn self_times(&self) -> Vec<Micros> {
        let mut self_time: Vec<Micros> =
            self.activities.iter().map(|a| a.duration().max(0)).collect();
        for (_, ids) in self.by_track() {
            // Sorted by start then by widest first, so a parent is always seen before its
            // children and the stack top is the innermost interval still open.
            let mut stack: Vec<ActivityId> = Vec::new();
            for id in ids {
                let here = &self.activities[id];
                if here.concurrent {
                    continue;
                }
                while stack.last().is_some_and(|&open| self.activities[open].end <= here.start) {
                    stack.pop();
                }
                if let Some(&parent) = stack.last() {
                    // Only the part of the child that actually lies inside the parent is the
                    // parent's to discount, so a child overhanging its parent cannot drive the
                    // parent's self time below zero.
                    let inside = here.end.min(self.activities[parent].end) - here.start;
                    self_time[parent] = (self_time[parent] - inside.max(0)).max(0);
                }
                stack.push(id);
            }
        }
        self_time
    }

    /// The given activities together with everything that ran nested inside them.
    ///
    /// Work nested inside a chain step ran because that step ran, so it is on the chain as surely
    /// as the step is. Without this a chain made of task frames contains no work at all, and every
    /// rule that reads the chain goes quiet on exactly the traces that matter most.
    pub fn with_nested(&self, roots: &[ActivityId]) -> Vec<ActivityId> {
        let mut included = vec![false; self.activities.len()];
        for &id in roots {
            if let Some(slot) = included.get_mut(id) {
                *slot = true;
            }
        }
        for (_, ids) in self.by_track() {
            let mut stack: Vec<ActivityId> = Vec::new();
            for id in ids {
                let here = &self.activities[id];
                if here.concurrent {
                    continue;
                }
                while stack.last().is_some_and(|&open| self.activities[open].end <= here.start) {
                    stack.pop();
                }
                if stack.last().is_some_and(|&parent| included[parent]) {
                    included[id] = true;
                }
                stack.push(id);
            }
        }
        let mut all: Vec<ActivityId> =
            (0..self.activities.len()).filter(|&id| included[id]).collect();
        all.sort_by_key(|&id| self.activities[id].start);
        all
    }
}

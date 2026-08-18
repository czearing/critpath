//! Where the work a finding names actually lives in source.
//!
//! Two facts are combined here and nothing else is added. The trace states a code position on some
//! of its intervals, and the build states what that position was before it was bundled. Between
//! them they name a file and a line, which is what turns "this chain spent 400ms" into a place
//! somebody can open.
//!
//! Most intervals state no position. They do not need to: work nested inside a call ran because
//! that call ran, so an interval with no position of its own inherits the position of the
//! innermost call enclosing it. That is a relation the trace already recorded, not an attribution
//! rule, and it is why a chain made entirely of anonymous task frames can still be placed.
//!
//! Cost stays where it was measured. The census below charges each line the self time of the
//! interval that stated it -- time that interval spent doing something other than waiting on work
//! nested inside it -- so a dispatcher enclosing everything is not credited with everything.

use std::collections::BTreeMap;

use critpath_core::{site_of, ActivityId, Graph, Micros};
use critpath_source::{Calibration, Located, Resolution, Resolver};

/// One place in original source, and what the measurement charged it.
#[derive(Clone, Debug)]
pub struct Place {
    /// Where it is.
    pub at: Located,
    /// Time charged to this line, excluding work nested inside it.
    pub cost: Micros,
    /// How many separate intervals the producer attributed to this line.
    pub calls: usize,
    /// Whether the original source at that line names the function the trace ran.
    pub proved: bool,
}

/// One dependency, and what the measurement charged it.
#[derive(Clone, Debug)]
pub struct Dependency {
    /// The installed package, as the path names it.
    pub package: String,
    /// Time charged to lines inside it.
    pub cost: Micros,
    /// How many distinct lines inside it were charged.
    pub lines: usize,
    /// How many separate intervals were attributed to it.
    pub calls: usize,
}

/// Everything that could be established about where the measured work lives.
pub struct Isolation {
    /// What each position-stating activity resolved to.
    resolved: BTreeMap<ActivityId, Resolution>,
    /// The innermost interval enclosing each activity.
    enclosing: Vec<Option<ActivityId>>,
    /// What proving the maps' numbering established.
    pub calibration: Calibration,
    /// Every line charged any time, costliest first.
    pub places: Vec<Place>,
    /// Every dependency charged any time, costliest first.
    pub dependencies: Vec<Dependency>,
    /// How many intervals stated a position at all.
    pub stated: usize,
    /// How many of those resolved to a line.
    pub placed: usize,
    /// How many placed lines the original source confirms by naming the function that ran.
    pub confirmed: usize,
}

impl Isolation {
    /// Resolve every position the trace states, and charge each line what was measured at it.
    pub fn of(graph: &Graph, resolver: &mut Resolver) -> Self {
        let self_times = graph.self_times();
        let sites: Vec<(ActivityId, critpath_core::Site<'_>)> = graph
            .activities
            .iter()
            .enumerate()
            .filter_map(|(id, activity)| Some((id, site_of(activity.subject.as_deref()?)?)))
            .collect();
        // Every position is seen before any is answered. The evidence that makes one of them
        // trustworthy -- that the original text names the function the trace ran -- comes from the
        // other positions in the same map, so calibration cannot be done one at a time.
        let calibration = resolver.calibrate(sites.iter().map(|&(_, site)| site));

        let mut answers = BTreeMap::new();
        let mut by_line: BTreeMap<(String, u32), Place> = BTreeMap::new();
        let mut answered = 0;
        let mut confirmed = 0;
        for &(id, site) in &sites {
            let resolution = resolver.resolve(site);
            if let Some(at) = resolution.located() {
                answered += 1;
                confirmed += usize::from(resolution.is_proved());
                let key = (at.source.clone(), at.line);
                let place = by_line.entry(key).or_insert_with(|| Place {
                    at: at.clone(),
                    cost: 0,
                    calls: 0,
                    proved: false,
                });
                place.cost += self_times.get(id).copied().unwrap_or_default();
                place.calls += 1;
                place.proved |= resolution.is_proved();
            }
            answers.insert(id, resolution);
        }

        let mut places: Vec<Place> = by_line.into_values().collect();
        // Costliest first, then by place, so a report is ordered by what was measured and two runs
        // over the same trace print the same thing.
        places.sort_by(|left, right| {
            right.cost.cmp(&left.cost).then_with(|| left.at.at().cmp(&right.at.at()))
        });

        let mut totals: BTreeMap<&str, Dependency> = BTreeMap::new();
        for place in &places {
            let Some(package) = place.at.package.as_deref() else { continue };
            let entry = totals.entry(package).or_insert_with(|| Dependency {
                package: package.to_owned(),
                cost: 0,
                lines: 0,
                calls: 0,
            });
            entry.cost += place.cost;
            entry.lines += 1;
            entry.calls += place.calls;
        }
        let mut dependencies: Vec<Dependency> = totals.into_values().collect();
        dependencies.sort_by(|left, right| {
            right.cost.cmp(&left.cost).then_with(|| left.package.cmp(&right.package))
        });

        Self {
            resolved: answers,
            enclosing: graph.enclosures(),
            calibration,
            places,
            dependencies,
            stated: sites.len(),
            placed: answered,
            confirmed,
        }
    }

    /// Where one activity's code is, following enclosure outward until something states a position.
    ///
    /// Returns how many frames outward the answer came from, so a report can say whether a line is
    /// the work itself or the call that ran it. The walk is bounded by the number of activities,
    /// because a cycle in enclosure would otherwise be an infinite loop rather than a wrong answer.
    pub fn at(&self, id: ActivityId) -> Option<(&Located, usize)> {
        let mut here = Some(id);
        for depth in 0..self.enclosing.len() {
            let current = here?;
            if let Some(at) = self.resolved.get(&current).and_then(Resolution::located) {
                return Some((at, depth));
            }
            here = *self.enclosing.get(current)?;
        }
        None
    }

    /// Whether the original source agreed with the position reported for one activity.
    pub fn is_proved(&self, id: ActivityId) -> bool {
        let mut here = Some(id);
        for _ in 0..self.enclosing.len() {
            let Some(current) = here else { return false };
            if let Some(resolution) = self.resolved.get(&current) {
                if resolution.located().is_some() {
                    return resolution.is_proved();
                }
            }
            here = match self.enclosing.get(current) {
                Some(parent) => *parent,
                None => return false,
            };
        }
        false
    }

    /// Whether anything at all could be placed.
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }
}

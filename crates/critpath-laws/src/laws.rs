//! The rules, each gated on what it needs before it is allowed to speak.
//!
//! Every rule is a [`Law`], so the only way to reach its result is through `ask`, which runs the
//! gate first. A rule can therefore never answer about a trace it was not entitled to read.

use core::marker::PhantomData;

use critpath_core::{Activity, Micros};
use fitkit_core::{Answer, Refusal};
use fitkit_ledger::{Citation, Law};

use crate::{Finding, Observation, CRITICAL_PATH, FORMAT};

/// Refuse while any event in the source went unaccounted for.
///
/// The gate every rule shares. A rule that finds nothing in a complete trace has proved something;
/// the same silence over a trace with holes has proved only that the reader dropped events, and
/// the two are indistinguishable from the outside.
fn read_everything(observation: &Observation<'_>) -> Answer<()> {
    if observation.graph.coverage.is_total() {
        Ok(())
    } else {
        Err(Refusal::unreported("events in the trace were not accounted for"))
    }
}

/// Refuse a chain with nothing on it.
fn has_a_chain(observation: &Observation<'_>) -> Answer<()> {
    if observation.path.steps.is_empty() {
        return Err(Refusal::uninformative("the chain has no steps"));
    }
    Ok(())
}

/// The same work, done twice, on the path that decides the finish.
///
/// Threshold free by construction. It fires on a repeated key, and repetition is a fact about the
/// trace rather than a judgement about how long something ought to take.
#[derive(Debug, Default)]
pub struct RepeatedWork<'a>(PhantomData<&'a ()>);

impl<'a> Law for RepeatedWork<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        FORMAT
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        read_everything(observation)?;
        has_a_chain(observation)
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        let graph = observation.graph;
        let mut groups: Vec<((&str, &str), Vec<usize>)> = Vec::new();
        for id in observation.path.activities() {
            let key = graph.activities[id].key();
            if let Some(group) = groups.iter_mut().find(|(seen, _)| *seen == key) {
                group.1.push(id);
            } else {
                groups.push((key, vec![id]));
            }
        }
        Ok(groups
            .into_iter()
            .filter(|(_, occurrences)| occurrences.len() > 1)
            .map(|(key, occurrences)| Finding::RepeatedWork {
                key: (key.0.to_owned(), key.1.to_owned()),
                // Everything after the first occurrence is time the chain spent recomputing what
                // it already had, so the first is the work and the rest are the cost.
                cost: occurrences[1..]
                    .iter()
                    .map(|&id| graph.activities[id].duration())
                    .sum::<Micros>(),
                occurrences,
            })
            .collect())
    }
}

/// Time on the chain during which the whole machine was idle.
///
/// Also threshold free: the rule needs no view on how long a wait may be, only on whether anything
/// was running during it.
#[derive(Debug, Default)]
pub struct DeadWait<'a>(PhantomData<&'a ()>);

impl<'a> Law for DeadWait<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        CRITICAL_PATH
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        read_everything(observation)?;
        has_a_chain(observation)
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        let graph = observation.graph;
        let mut findings = Vec::new();
        for step in &observation.path.steps {
            if step.wait_before == 0 {
                continue;
            }
            let starts = graph.activities[step.activity].start;
            let opened = starts - step.wait_before;
            // Contention is a different problem with a different fix, so the rule only fires when
            // nothing at all was running: an idle machine is always a dependency issued too late.
            let busy = graph
                .activities
                .iter()
                .filter(|a| Activity::is_informative(a))
                .any(|a| a.start < starts && a.end > opened);
            if !busy {
                findings.push(Finding::DeadWait { before: step.activity, cost: step.wait_before });
            }
        }
        Ok(findings)
    }
}

/// The largest thing in the trace, when it turns out not to matter.
///
/// The finding a ranked profile inverts. Reported so the report can say plainly that the work at
/// the top of the flame graph can be deleted without the finish moving.
#[derive(Debug, Default)]
pub struct OffPath<'a>(PhantomData<&'a ()>);

impl<'a> Law for OffPath<'a> {
    type Input = Observation<'a>;
    type Output = Vec<Finding>;

    fn citation(&self) -> Citation {
        CRITICAL_PATH
    }

    fn admits(&self, observation: &Self::Input) -> Answer<()> {
        read_everything(observation)
    }

    fn derive(&self, observation: &Self::Input) -> Answer<Self::Output> {
        let Some(longest) = observation.graph.longest() else {
            return Ok(Vec::new());
        };
        if observation.path.holds(longest) {
            return Ok(Vec::new());
        }
        Ok(vec![Finding::OffPath {
            activity: longest,
            duration: observation.graph.activities[longest].duration(),
        }])
    }
}

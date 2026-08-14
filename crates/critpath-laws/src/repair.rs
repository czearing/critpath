//! Which repairs to make, and where making more of them stops paying.

use critpath_core::Micros;
use fitkit_core::{Answer, Margin, Refusal};
use fitkit_dp::{optimise_subset, SubsetResult, MAX_POOL};

use crate::Finding;

/// Enumeration is affordable up to this many findings: 2^20 subsets is about a million scorings.
///
/// A statement about this machine, not about traces. Beyond it the search is a beam and says so.
const AFFORDABLE: usize = 20;

/// States kept per size once the search is a beam.
const BEAM: usize = 256;

/// A chosen set of repairs and what it is worth.
#[derive(Clone, Debug, PartialEq)]
pub struct Repair {
    /// Indices into the findings that were selected.
    pub chosen: Vec<usize>,
    /// Time the chain loses if all of them land, never more than the margin allows.
    pub recovered: Micros,
    /// Whether every combination was enumerated, so this is the proven best set.
    ///
    /// False means the search was a beam and a better set may exist. Reported rather than
    /// smoothed over, because an unproven optimum is a different claim.
    pub proven: bool,
}

/// Choose at most `budget` repairs, maximising time removed from the chain.
///
/// Recovery is capped at the chain's [`Margin`]. Past that point a different chain is the
/// constraint and further work on this one buys nothing, so the optimiser will not spend a change
/// to buy time the schedule cannot realise. A chain with no margin selects nothing, which is the
/// correct and unpopular answer.
///
/// `budget` is required and has no default. How many changes a team can afford is not a property
/// of the trace, and inventing one here would be inventing the answer.
///
/// # Errors
///
/// Refuses when there are more findings than the mask can hold, or when nothing on offer costs the
/// chain anything, since selecting among worthless repairs would report a decision that was not
/// made on evidence.
pub fn choose(findings: &[Finding], budget: usize, margin: Margin) -> Answer<Repair> {
    if findings.len() > MAX_POOL {
        return Err(Refusal::outside_provenance("more findings than the subset mask can hold"));
    }
    if !findings.iter().any(|finding| finding.cost() > 0) {
        return Err(Refusal::uninformative("no finding costs the chain any time"));
    }
    let ceiling = margin.get();
    let pool = findings.len();
    // Ties are broken toward fewer changes: recovery is integer microseconds, so scaling it past
    // the pool size makes one microsecond outrank every possible saving in change count.
    let scale = (pool + 1) as f64;
    let result: SubsetResult = optimise_subset(pool, AFFORDABLE, BEAM, |members| {
        let count = members.count_ones() as usize;
        if count > budget {
            return f64::NEG_INFINITY;
        }
        let recovered: Micros =
            (0..pool).filter(|i| members & (1 << i) != 0).map(|i| findings[i].cost()).sum();
        (recovered as f64).min(ceiling).mul_add(scale, -(count as f64))
    });
    let chosen: Vec<usize> = result.indices().filter(|&i| i < pool).collect();
    let recovered: Micros = chosen.iter().map(|&i| findings[i].cost()).sum();
    Ok(Repair { recovered: recovered.min(as_micros(ceiling)), chosen, proven: result.is_proven() })
}

/// A margin back into the microseconds it was measured in.
///
/// Clamped into range first, so the cast cannot truncate: an unbounded margin becomes the largest
/// representable time rather than a wrapped negative one.
#[allow(clippy::cast_possible_truncation)]
fn as_micros(value: f64) -> Micros {
    value.clamp(0.0, Micros::MAX as f64) as Micros
}

#[cfg(test)]
mod tests {
    use fitkit_core::{Margin, RefusalKind};

    use super::{choose, Finding};

    fn wait(cost: i64) -> Finding {
        Finding::DeadWait { before: 0, cost }
    }

    #[test]
    fn the_budget_binds() {
        let findings = vec![wait(10), wait(9), wait(8)];
        let repair = choose(&findings, 2, Margin::UNBOUNDED).unwrap();
        assert_eq!(repair.chosen, vec![0, 1], "the two largest, and no more");
        assert_eq!(repair.recovered, 19);
        assert!(repair.proven);
    }

    #[test]
    fn nothing_is_bought_beyond_the_margin() {
        // Two repairs worth 10 each, but only 10 of slack: the second buys nothing, so it is not
        // spent even though the budget would allow it.
        let findings = vec![wait(10), wait(10)];
        let repair = choose(&findings, 2, Margin::new(10.0)).unwrap();
        assert_eq!(repair.chosen.len(), 1, "one change reaches the ceiling, so one is chosen");
        assert_eq!(repair.recovered, 10);
    }

    #[test]
    fn a_chain_with_no_margin_selects_nothing() {
        let findings = vec![wait(10)];
        assert!(choose(&findings, 1, Margin::NONE).unwrap().chosen.is_empty());
    }

    #[test]
    fn findings_that_cost_nothing_are_refused_rather_than_ranked() {
        let findings = vec![Finding::OffPath { activity: 0, duration: 500 }];
        assert_eq!(
            choose(&findings, 1, Margin::UNBOUNDED).unwrap_err().kind(),
            RefusalKind::Uninformative
        );
    }
}

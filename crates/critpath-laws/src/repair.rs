//! Which repairs to make, and where making more of them stops paying.

use critpath_core::Micros;
use fitkit_core::{Answer, Margin, Refusal};

use crate::Finding;

/// A chosen set of repairs and what it is worth.
#[derive(Clone, Debug, PartialEq)]
pub struct Repair {
    /// Indices into the findings that were selected.
    pub chosen: Vec<usize>,
    /// Time the chain loses if all of them land, never more than the margin allows.
    pub recovered: Micros,
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
/// Refuses when nothing on offer costs the chain anything, since selecting among worthless
/// repairs would report a decision that was not made on evidence.
pub fn choose(findings: &[Finding], budget: usize, margin: Margin) -> Answer<Repair> {
    if !findings.iter().any(|finding| finding.cost() > 0) {
        return Err(Refusal::uninformative("no finding costs the chain any time"));
    }
    // This selection was a subset search: exhaustive below twenty findings and a beam of 256
    // above. Both were wrong, and the beam was wrong twice -- it imported two constants that
    // decided the answer, and it reported an optimum it had not proved.
    //
    // The costs here are disjoint by construction, which is what makes a search unnecessary.
    // A repeat is charged in self time, and an activity has one identity, so no activity is
    // counted by two repeat findings. A wait is the gap before a chain step, and the gaps between
    // consecutive steps do not overlap. Self time is time something ran and a dead wait is time
    // nothing ran, so the two kinds cannot claim the same microsecond either. Total recovery is
    // therefore additive, and the only constraint is how many changes are affordable.
    //
    // Additive value under a plain count of items is a uniform matroid, and Rado-Edmonds says
    // greedy attains the exact optimum on a matroid for any linear objective -- it is the theorem
    // that characterises matroids. So taking the costliest findings is not an approximation of the
    // search that was here before; it is the answer the search was trying to find, obtained
    // exactly, in n log n rather than 2^n. A table would tabulate states already determined.
    let ceiling = as_micros(margin.get());
    let mut order: Vec<usize> = (0..findings.len()).filter(|&i| findings[i].cost() > 0).collect();
    // Ties broken by position so the report is stable between runs on the same trace.
    order.sort_by_key(|&i| (core::cmp::Reverse(findings[i].cost()), i));
    // Two criteria, in order: recover as much as the margin can realise, then do it with as few
    // changes as possible. Taking the costliest first satisfies both at once, because no other set
    // of the same size can reach further and no smaller set can reach the same total. Stopping the
    // moment the ceiling is reached is what keeps the second criterion honest: a change that buys
    // time the schedule cannot realise is a change nobody should be asked to make.
    let mut chosen = Vec::new();
    let mut recovered: Micros = 0;
    for index in order {
        if chosen.len() >= budget || recovered >= ceiling {
            break;
        }
        recovered += findings[index].cost();
        chosen.push(index);
    }
    chosen.sort_unstable();
    Ok(Repair { recovered: recovered.min(ceiling), chosen })
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
        Finding::DeadWait { before: 0, waited_on: None, stated: false, cost }
    }

    #[test]
    fn the_budget_binds() {
        let findings = vec![wait(10), wait(9), wait(8)];
        let repair = choose(&findings, 2, Margin::UNBOUNDED).unwrap();
        assert_eq!(repair.chosen, vec![0, 1], "the two largest, and no more");
        assert_eq!(repair.recovered, 19);
    }

    #[test]
    fn the_choice_matches_exhaustive_enumeration_on_every_budget() {
        // The search this replaced enumerated subsets below twenty findings and guessed above.
        // Greedy is exact here rather than approximate, so the test proves it: enumerate every
        // subset by brute force and check the greedy reaches the same recovery with no more
        // changes, across every budget and several margins.
        let costs = [7_i64, 3, 11, 11, 1, 5, 9, 2];
        let findings: Vec<Finding> = costs.iter().map(|&c| wait(c)).collect();
        for ceiling in [f64::INFINITY, 40.0, 20.0, 11.0, 1.0] {
            let margin =
                if ceiling.is_infinite() { Margin::UNBOUNDED } else { Margin::new(ceiling) };
            let cap = if ceiling.is_infinite() { i64::MAX } else { super::as_micros(ceiling) };
            for budget in 0..=costs.len() {
                let mut best = (0_i64, usize::MAX);
                for members in 0..(1_u64 << costs.len()) {
                    let picked: Vec<usize> =
                        (0..costs.len()).filter(|i| members & (1 << i) != 0).collect();
                    if picked.len() > budget {
                        continue;
                    }
                    let sum: i64 = picked.iter().map(|&i| costs[i]).sum::<i64>().min(cap);
                    if sum > best.0 || (sum == best.0 && picked.len() < best.1) {
                        best = (sum, picked.len());
                    }
                }
                let repair = choose(&findings, budget, margin);
                let (got, used) = repair.as_ref().map_or((0, 0), |r| (r.recovered, r.chosen.len()));
                assert_eq!(got, best.0, "budget {budget}, ceiling {ceiling}: recovery differs");
                assert!(used <= best.1, "budget {budget}: greedy spent more changes than needed");
            }
        }
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
        let findings = vec![Finding::OffPath { activity: 0, duration: 500, room: 0 }];
        assert_eq!(
            choose(&findings, 1, Margin::UNBOUNDED).unwrap_err().kind(),
            RefusalKind::Uninformative
        );
    }
}

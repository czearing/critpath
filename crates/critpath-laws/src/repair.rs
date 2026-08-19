//! Which repairs to make, and where making more of them stops paying.

use critpath_core::Micros;
use fitkit_core::{Answer, Confidence, Evidence, Margin, Refusal, Span};
use fitkit_dp::{Terms, MAX_POOL};

use crate::Finding;

/// A chosen set of repairs and what it is worth.
#[derive(Clone, Debug, PartialEq)]
pub struct Repair {
    /// Indices into the findings that were selected.
    pub chosen: Vec<usize>,
    /// Time the chain loses if all of them land, never more than the margin allows.
    pub recovered: Micros,
    /// The regions of the timeline that argued for the selection.
    ///
    /// Named rather than asserted: what the choice rests on, traced back through the terms it was
    /// stated in, so a reader can go and look instead of taking the total on trust.
    pub support: Vec<Span>,
    /// How far the selection is trusted: the weakest evidence anywhere behind it.
    pub trust: Confidence,
    /// Findings that cost the chain time but whose measurement cannot support a decision.
    ///
    /// A cost recorded over an empty stretch, or held at no confidence, is a number rather than a
    /// measurement. Such a finding is still reported, but it cannot be weighed against one that
    /// was measured, so it is counted here rather than dropped into the pool at face value or
    /// allowed to refuse the whole plan.
    pub unweighable: usize,
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
/// Refuses when nothing on offer costs the chain anything, since selecting among worthless repairs
/// would report a decision that was not made on evidence; when no cost that was measured can be
/// weighed at all; and when the budget is wider than the mask the terms are stated over.
pub fn choose(findings: &[Finding], budget: usize, margin: Margin) -> Answer<Repair> {
    if !findings.iter().any(|finding| finding.cost() > 0) {
        return Err(Refusal::uninformative("no finding costs the chain any time"));
    }
    if budget > MAX_POOL {
        return Err(Refusal::incoherent("a plan wider than the terms that state it"));
    }
    // Only a cost measured over a real stretch, at some confidence, can be weighed. The rest are
    // counted and reported rather than admitted at face value, because a weight resting on nothing
    // is the constant this crate exists to keep out. The measurement is carried alongside the
    // index from here on, so no later step has to assume one is there.
    let (weighable, unweighable) = weigh(findings);
    if weighable.is_empty() {
        return Err(Refusal::unreported("every cost on offer rests on no span or no confidence"));
    }
    let terms = state(&weighable, budget)?;

    // Value is additive and the budget is a plain count, so the feasible sets are a uniform
    // matroid and Rado-Edmonds makes taking the costliest first exactly optimal for a linear
    // objective. The margin does not break that. Capping recovery makes the objective
    // min(sum, ceiling), which is concave rather than linear, and that is ordinarily the point at
    // which greedy degrades to the 1-1/e guarantee for monotone submodular maximisation. It does
    // not degrade here, because min(., ceiling) is non-decreasing in the sum: whatever maximises
    // the sum at a given count also maximises the capped value, so the same order is still right.
    //
    // So no search runs. The terms above are the statement and this is the solution, obtained
    // exactly in n log n rather than 2^n. The library's own exhaustive solver is the oracle that
    // proves the two agree, and it lives in the tests, where taking exponential time is affordable
    // and being wrong is not.
    let ceiling = as_micros(margin.get());
    // Two criteria, in order: recover as much as the margin can realise, then do it with as few
    // changes as possible. Taking the costliest first satisfies both at once, because no other set
    // of the same size can reach further and no smaller set can reach the same total. Stopping the
    // moment the ceiling is reached is what keeps the second criterion honest: a change that buys
    // time the schedule cannot realise is a change nobody should be asked to make.
    let mut chosen = Vec::new();
    let mut members: u64 = 0;
    let mut recovered: Micros = 0;
    for (slot, &(index, charge)) in weighable.iter().enumerate() {
        if chosen.len() >= budget || recovered >= ceiling {
            break;
        }
        recovered += charge.value;
        members |= 1 << slot;
        chosen.push(index);
    }
    chosen.sort_unstable();
    Ok(Repair {
        recovered: recovered.min(ceiling),
        chosen,
        support: terms.support(members),
        trust: terms.trust(members),
        unweighable,
    })
}

/// A margin back into the microseconds it was measured in.
///
/// Clamped into range first, so the cast cannot truncate: an unbounded margin becomes the largest
/// representable time rather than a wrapped negative one.
#[allow(clippy::cast_possible_truncation)]
fn as_micros(value: f64) -> Micros {
    value.clamp(0.0, Micros::MAX as f64) as Micros
}

/// The findings whose cost is a measurement, costliest first, and how many were not.
///
/// Ties broken by position so the report is stable between runs on the same trace.
fn weigh(findings: &[Finding]) -> (Vec<(usize, Evidence<Micros>)>, usize) {
    let mut weighable: Vec<(usize, Evidence<Micros>)> = Vec::new();
    let mut unweighable = 0;
    for (index, finding) in findings.iter().enumerate() {
        if finding.cost() <= 0 {
            continue;
        }
        match finding.charge() {
            Some(charge) if charge.is_informative() => weighable.push((index, *charge)),
            _ => unweighable += 1,
        }
    }
    weighable.sort_by_key(|&(index, charge)| (core::cmp::Reverse(charge.value), index));
    // The terms are stated over a mask, which holds sixty-four items. Keeping the costliest that
    // many loses nothing: a selection of at most `budget` items maximising an additive value can
    // always be taken from the `budget` costliest, by the exchange argument, and a budget above
    // the mask width is refused. So anything past position sixty-four is outside every optimal
    // selection, rather than merely unlikely to appear in one.
    weighable.truncate(MAX_POOL);
    (weighable, unweighable)
}

/// The objective, said out loud.
///
/// Each item is worth what its own measurement says, carrying the span it speaks for and the trust
/// it is held with, so no weight here can be a number somebody tuned until the output looked right.
///
/// There are no pairwise terms, and that absence is the load-bearing claim: the costs are disjoint
/// by construction. A repeat is charged in self time and an activity has one identity, so no
/// activity is counted by two repeat findings. A wait is the gap before a chain step, and the gaps
/// between consecutive steps do not overlap. Self time is time something ran and a dead wait is
/// time nothing ran, so the two kinds cannot claim the same microsecond either. Saying it in terms
/// rather than in a comment is the point: a redundancy or a complement between two findings would
/// have to be written as `together`, and there is no such line here to overlook.
///
/// This is the one place the problem is stated, and the tests solve *this* rather than a copy of
/// it. A statement the answer is not derived from is decoration, and would drift from the answer
/// without anything failing.
fn state(weighable: &[(usize, Evidence<Micros>)], budget: usize) -> Answer<Terms> {
    let mut terms = Terms::over(weighable.len())?;
    for (slot, &(_, charge)) in weighable.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let worth = charge.value as f64;
        terms = terms.worth(slot, Evidence::new(charge.span, charge.confidence, worth))?;
    }
    terms.at_most(budget)
}

#[cfg(test)]
mod tests {
    use fitkit_core::{Confidence, Evidence, Margin, RefusalKind, Span};
    use fitkit_dp::{optimise_subset, Terms};

    use super::{choose, Finding};

    /// A dead wait costing `cost`, measured over its own stretch of the timeline.
    ///
    /// The stretch matters. A weight has to cite a region, and giving every finding the same one
    /// would be citing nothing.
    fn at(start: usize, cost: i64) -> Finding {
        let width = usize::try_from(cost.max(1)).unwrap_or(1);
        Finding::DeadWait {
            before: 0,
            waited_on: None,
            stated: false,
            cost: Evidence::certain(Span::new(start, start + width), cost),
        }
    }

    /// The eight costs both oracles below are run against, laid out end to end on the timeline.
    fn spread(costs: &[i64]) -> Vec<Finding> {
        let mut findings = Vec::new();
        let mut start = 0;
        for &cost in costs {
            findings.push(at(start, cost));
            let width = usize::try_from(cost.max(1)).unwrap_or(1);
            start += width;
        }
        findings
    }

    fn stated(findings: &[Finding], budget: usize) -> Terms {
        let (weighable, _) = super::weigh(findings);
        super::state(&weighable, budget).unwrap()
    }

    #[test]
    fn the_budget_binds() {
        let findings = spread(&[10, 9, 8]);
        let repair = choose(&findings, 2, Margin::UNBOUNDED).unwrap();
        assert_eq!(repair.chosen, vec![0, 1], "the two largest, and no more");
        assert_eq!(repair.recovered, 19);
    }

    #[test]
    fn the_choice_matches_the_librarys_own_exhaustive_search() {
        // The statement and the solution are separate here: the terms say what the problem is, and
        // the greedy answers it without searching. This is what makes that legitimate rather than
        // a shortcut. State the same terms, hand them to the library's exhaustive solver, and
        // require the two to agree on every budget.
        //
        // Unbounded margin, because the terms deliberately cannot express a cap: saturation is a
        // global effect and an objective that stops at pairs has nowhere to put it. The ceiling
        // gets its own oracle in the next test.
        let costs = [7_i64, 3, 11, 11, 1, 5, 9, 2];
        let findings = spread(&costs);
        for budget in 1..=costs.len() {
            let terms = stated(&findings, budget);
            let found = optimise_subset(&terms, costs.len(), 1).expect("a pool offers subsets");
            let found = found.get();
            assert!(found.is_proven(), "the oracle has to have enumerated, not guessed");
            let repair = choose(&findings, budget, Margin::UNBOUNDED).unwrap();
            #[allow(clippy::cast_precision_loss)]
            let ours = repair.recovered as f64;
            assert!(
                (found.score() - ours).abs() < 1e-9,
                "budget {budget}: greedy recovered {ours}, exhaustive search found {}",
                found.score()
            );
            assert_eq!(
                repair.chosen.len(),
                found.len(),
                "budget {budget}: the two disagree on how many changes it takes"
            );
        }
    }

    #[test]
    fn the_choice_matches_exhaustive_enumeration_under_every_ceiling() {
        // The ceiling lives outside the terms, so it needs its own oracle. Brute force every
        // subset and check the greedy reaches the same capped recovery with no more changes.
        let costs = [7_i64, 3, 11, 11, 1, 5, 9, 2];
        let findings = spread(&costs);
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
    fn the_selection_names_the_regions_that_argued_for_it() {
        let findings = vec![at(0, 10), at(100, 9), at(200, 8)];
        let repair = choose(&findings, 2, Margin::UNBOUNDED).unwrap();
        assert_eq!(
            repair.support,
            vec![Span::new(0, 10), Span::new(100, 109)],
            "the spans behind the two chosen, and nothing behind the one left out"
        );
        assert_eq!(repair.trust, Confidence::FULL);
    }

    #[test]
    fn a_shakily_measured_cost_drags_the_trust_of_the_whole_plan_down() {
        let mut findings = vec![at(0, 10), at(100, 9)];
        let Finding::DeadWait { cost, .. } = &mut findings[0] else { panic!("a dead wait") };
        cost.confidence = Confidence::new(0.25);
        let repair = choose(&findings, 2, Margin::UNBOUNDED).unwrap();
        assert_eq!(repair.trust, Confidence::new(0.25), "a plan is as good as its weakest support");
    }

    #[test]
    fn a_cost_resting_on_nothing_is_counted_rather_than_weighed() {
        // A measurement over an empty stretch is a number. It must not enter the pool as though it
        // were evidence, and it must not take the rest of the plan down with it.
        let mut findings = vec![at(0, 10), at(100, 9)];
        let Finding::DeadWait { cost, .. } = &mut findings[1] else { panic!("a dead wait") };
        cost.span = Span::new(100, 100);
        let repair = choose(&findings, 2, Margin::UNBOUNDED).unwrap();
        assert_eq!(repair.unweighable, 1);
        assert_eq!(repair.chosen, vec![0], "only the measured one is spent a change on");
    }

    #[test]
    fn a_plan_wider_than_the_terms_is_refused_rather_than_truncated() {
        let findings = vec![at(0, 10)];
        assert_eq!(
            choose(&findings, 65, Margin::UNBOUNDED).unwrap_err().kind(),
            RefusalKind::Incoherent
        );
    }

    #[test]
    fn nothing_is_bought_beyond_the_margin() {
        // Two repairs worth 10 each, but only 10 of slack: the second buys nothing, so it is not
        // spent even though the budget would allow it.
        let findings = vec![at(0, 10), at(100, 10)];
        let repair = choose(&findings, 2, Margin::new(10.0)).unwrap();
        assert_eq!(repair.chosen.len(), 1, "one change reaches the ceiling, so one is chosen");
        assert_eq!(repair.recovered, 10);
    }

    #[test]
    fn a_chain_with_no_margin_selects_nothing() {
        let findings = vec![at(0, 10)];
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

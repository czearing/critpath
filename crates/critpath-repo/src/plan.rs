//! Choosing which removals to make, under a budget of changes.
//!
//! This is the one step that is an optimisation rather than a graph walk, and it is not a sort.
//! Two things defeat ranking: savings overlap, because a component and the thing above it claim
//! the same bytes, so a ranked list promises a total it can never deliver; and removals have
//! prerequisites, because cutting deep is pointless once something above it has gone.
//!
//! That is precedence-constrained knapsack, which is strongly NP-hard over an arbitrary graph.
//! Dominance is what makes it tractable: the dominator relation is a TREE by construction, and
//! tree-constrained knapsack has an exact bottom-up dynamic program in O(nodes * budget^2). So
//! the answer here is an optimum, not a heuristic, and it is only an optimum because the previous
//! step produced a tree.
//!
//! `fitkit-dp`'s subset solver is not used, deliberately: it assumes candidates are independent,
//! and nested dominator subtrees are the opposite of independent.

use crate::{ComponentId, Held, Repo};

/// One change: stop reaching a component, and what stops being reachable with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    /// The component that would no longer be reached.
    pub component: ComponentId,
    /// Weight that becomes unreachable, its own plus everything only it holds.
    pub frees: u64,
}

/// The best set of removals within a budget, with the margin to the next one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The chosen set. Never two components where one holds the other.
    pub removals: Vec<Removal>,
    /// Weight the whole set frees. Not the sum of any ranking, because subtrees nest.
    pub frees: u64,
    /// What one more change beyond the budget would add. Zero when nothing is left to gain.
    pub margin: u64,
    /// Changes the plan was allowed.
    pub budget: usize,
    /// True when the set is an exact optimum rather than a search that ran out of room.
    pub exact: bool,
}

/// Solves the tree knapsack over the dominator tree.
pub fn choose(held: &Held, repo: &Repo, budget: usize) -> Plan {
    let count = repo.components.len();
    // One past the budget, so the margin is the honest cost of the change we did not take.
    let cap = budget + 1;
    let mut plan = Plan { budget, exact: true, ..Plan::default() };
    if count == 0 || budget == 0 {
        return plan;
    }

    let mut children: Vec<Vec<ComponentId>> = vec![Vec::new(); count];
    for id in 0..count {
        if !held.reachable[id] || id == repo.entry {
            continue;
        }
        let parent = held.dominator[id];
        if parent != usize::MAX && parent != id {
            children[parent].push(id);
        }
    }

    let order = postorder(&children, repo.entry);
    let mut best = vec![Vec::new(); count];
    let mut cut = vec![Vec::new(); count];
    let mut split: Vec<Vec<Vec<usize>>> = vec![Vec::new(); count];

    for &id in &order {
        // Distribute the budget across the children first: `spread[k]` is the most that can be
        // freed strictly below `id` using at most k changes.
        let mut spread = vec![0u64; cap + 1];
        let mut choices = Vec::with_capacity(children[id].len());
        for &child in &children[id] {
            let mut next = vec![0u64; cap + 1];
            let mut taken = vec![0usize; cap + 1];
            for k in 0..=cap {
                for give in 0..=k {
                    let total = spread[k - give] + best[child][give];
                    if total > next[k] {
                        next[k] = total;
                        taken[k] = give;
                    }
                }
            }
            spread = next;
            choices.push(taken);
        }

        let mut here = vec![0u64; cap + 1];
        let mut cutting = vec![false; cap + 1];
        for k in 0..=cap {
            here[k] = spread[k];
            // Cutting this component costs one change and frees everything it holds. It is never
            // worth also cutting inside what has already gone, so the two options are exclusive.
            if id != repo.entry && k >= 1 && held.retained[id] > here[k] {
                here[k] = held.retained[id];
                cutting[k] = true;
            }
        }
        best[id] = here;
        cut[id] = cutting;
        split[id] = choices;
    }

    let entry = repo.entry;
    plan.frees = best[entry][budget];
    plan.margin = best[entry][cap].saturating_sub(best[entry][budget]);
    collect(entry, budget, entry, &children, &cut, &split, held, &mut plan.removals);
    plan.removals.sort_by_key(|removal| std::cmp::Reverse(removal.frees));
    plan
}

/// Walks the decisions back out of the tables.
#[allow(clippy::too_many_arguments)]
fn collect(
    id: ComponentId,
    budget: usize,
    entry: ComponentId,
    children: &[Vec<ComponentId>],
    cut: &[Vec<bool>],
    split: &[Vec<Vec<usize>>],
    held: &Held,
    out: &mut Vec<Removal>,
) {
    if budget == 0 {
        return;
    }
    if id != entry && cut[id][budget] {
        out.push(Removal { component: id, frees: held.retained[id] });
        return;
    }
    // Undo the children in the order they were folded in, last first, because each table row was
    // built on top of the row before it.
    let mut left = budget;
    for index in (0..children[id].len()).rev() {
        let give = split[id][index][left];
        let child = children[id][index];
        collect(child, give, entry, children, cut, split, held, out);
        left -= give;
    }
}

/// Post-order over the dominator tree, iteratively, so a deep install cannot overflow the stack.
fn postorder(children: &[Vec<ComponentId>], root: ComponentId) -> Vec<ComponentId> {
    let mut order = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((id, next)) = stack.pop() {
        match children[id].get(next) {
            Some(&child) => {
                stack.push((id, next + 1));
                stack.push((child, 0));
            }
            None => order.push(id),
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use crate::{Component, Repo};

    fn component(name: &str, weight: u64, declares: &[usize]) -> Component {
        Component {
            name: name.to_owned(),
            directory: String::new(),
            weight,
            declares: declares.to_vec(),
            unresolved: Vec::new(),
            owned: false,
        }
    }

    fn repo(components: Vec<Component>) -> Repo {
        Repo { components, entry: 0, ..Repo::default() }
    }

    #[test]
    fn one_change_takes_the_branch_that_holds_the_most() {
        let repo = repo(vec![
            component("app", 0, &[1, 3]),
            component("thin", 1, &[2]),
            component("heavy", 900, &[]),
            component("fat", 500, &[]),
        ]);
        let held = repo.hold();
        let plan = held.plan(&repo, 1);
        assert_eq!(plan.frees, 901, "the 1-byte module holds 901 bytes");
        assert_eq!(plan.removals.len(), 1);
        assert_eq!(plan.removals[0].component, 1);
    }

    #[test]
    fn a_ranking_by_own_weight_would_have_chosen_worse() {
        // `fat` is 500 times heavier than `thin`, and worth less to remove. This is the whole
        // reason the tool computes dominance instead of sorting a treemap.
        let repo = repo(vec![
            component("app", 0, &[1, 3]),
            component("thin", 1, &[2]),
            component("heavy", 900, &[]),
            component("fat", 500, &[]),
        ]);
        let held = repo.hold();
        let by_own_weight = held.plan(&repo, 1).removals[0].component;
        assert_ne!(by_own_weight, 3, "own weight would have picked `fat`");
    }

    #[test]
    fn the_plan_never_pays_twice_for_nested_savings() {
        // Cutting `outer` already removes `inner`. A ranked list would claim 1100 + 1000.
        let repo = repo(vec![
            component("app", 0, &[1]),
            component("outer", 100, &[2]),
            component("inner", 1000, &[]),
        ]);
        let held = repo.hold();
        let plan = held.plan(&repo, 2);
        assert_eq!(plan.frees, 1100, "nested subtrees are counted once");
        assert_eq!(plan.removals.len(), 1, "the second change would add nothing");
    }

    #[test]
    fn the_margin_states_what_the_next_change_would_add() {
        let repo = repo(vec![
            component("app", 0, &[1, 2]),
            component("big", 900, &[]),
            component("small", 9, &[]),
        ]);
        let held = repo.hold();
        let plan = held.plan(&repo, 1);
        assert_eq!(plan.frees, 900);
        assert_eq!(plan.margin, 9, "the change we did not take is worth 9");
    }

    #[test]
    fn spending_more_than_there_is_to_gain_stops() {
        let repo = repo(vec![component("app", 0, &[1]), component("only", 7, &[])]);
        let held = repo.hold();
        let plan = held.plan(&repo, 5);
        assert_eq!(plan.frees, 7);
        assert_eq!(plan.margin, 0);
        assert_eq!(plan.removals.len(), 1);
    }

    #[test]
    fn a_budget_of_none_promises_nothing() {
        let repo = repo(vec![component("app", 0, &[1]), component("only", 7, &[])]);
        let plan = repo.hold().plan(&repo, 0);
        assert_eq!(plan.frees, 0);
        assert!(plan.removals.is_empty());
    }
}

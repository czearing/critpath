//! Reachability and dominance over the declared graph.
//!
//! Both are fixed points, and neither needs the program to run. Reachability answers what the
//! entry could possibly load. Dominance answers the question a size ranking gets wrong: a
//! component's own weight is what it is, but what its removal *delivers* is everything reachable
//! only through it. Chrome's heap panel makes the same distinction between shallow and retained
//! size, and for the same reason -- the collector frees what nothing else holds.

use crate::{ComponentId, Held, Repo};

/// Reachability, immediate dominators and retained weight, from the repository's entry.
pub fn hold(repo: &Repo) -> Held {
    let count = repo.components.len();
    let mut held = Held {
        reachable: vec![false; count],
        dominator: vec![usize::MAX; count],
        retained: vec![0; count],
        reached: 0,
    };
    if count == 0 {
        return held;
    }

    // Reverse postorder from the entry. It doubles as the reachable set, and dominator iteration
    // converges in one pass over it for a reducible graph and a few more otherwise.
    let order = reverse_postorder(repo, repo.entry);
    let mut rank = vec![usize::MAX; count];
    for (position, &id) in order.iter().enumerate() {
        rank[id] = position;
        held.reachable[id] = true;
        held.reached += repo.components[id].weight;
    }

    let predecessors = predecessors_of(repo, &held.reachable);

    held.dominator[repo.entry] = repo.entry;
    let mut settled = false;
    while !settled {
        settled = true;
        for &id in order.iter().skip(1) {
            let mut candidate = usize::MAX;
            for &from in &predecessors[id] {
                if held.dominator[from] == usize::MAX {
                    continue;
                }
                candidate = if candidate == usize::MAX {
                    from
                } else {
                    common(&held.dominator, &rank, from, candidate)
                };
            }
            if candidate != usize::MAX && held.dominator[id] != candidate {
                held.dominator[id] = candidate;
                settled = false;
            }
        }
    }

    // Retained weight, accumulated child-before-parent. A component's immediate dominator always
    // has a lower reverse-postorder rank, so walking the order backwards is enough; every subtree
    // is added exactly once, which is what stops overlapping savings being double-counted.
    for &id in &order {
        held.retained[id] = repo.components[id].weight;
    }
    for &id in order.iter().skip(1).rev() {
        let parent = held.dominator[id];
        if parent != usize::MAX && parent != id {
            held.retained[parent] += held.retained[id];
        }
    }
    held
}

/// The nearest component that dominates both, by walking up the tree the lower rank first.
fn common(dominator: &[ComponentId], rank: &[usize], mut left: usize, mut right: usize) -> usize {
    while left != right {
        while rank[left] > rank[right] {
            let next = dominator[left];
            if next == usize::MAX || next == left {
                return right;
            }
            left = next;
        }
        while rank[right] > rank[left] {
            let next = dominator[right];
            if next == usize::MAX || next == right {
                return left;
            }
            right = next;
        }
    }
    left
}

/// Reverse postorder from a root, iteratively, because a deep install would overflow a recursion.
fn reverse_postorder(repo: &Repo, root: ComponentId) -> Vec<ComponentId> {
    let mut seen = vec![false; repo.components.len()];
    let mut postorder = Vec::new();
    let mut stack = vec![(root, 0usize)];
    seen[root] = true;
    while let Some((id, next)) = stack.pop() {
        match repo.components[id].declares.get(next) {
            Some(&child) => {
                stack.push((id, next + 1));
                if !seen[child] {
                    seen[child] = true;
                    stack.push((child, 0));
                }
            }
            None => postorder.push(id),
        }
    }
    postorder.reverse();
    postorder
}

/// Who declares each reachable component, which dominance needs and the forward graph does not
/// carry.
fn predecessors_of(repo: &Repo, reachable: &[bool]) -> Vec<Vec<ComponentId>> {
    let mut into = vec![Vec::new(); repo.components.len()];
    for (id, component) in repo.components.iter().enumerate() {
        if !reachable[id] {
            continue;
        }
        for &child in &component.declares {
            into[child].push(id);
        }
    }
    into
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
    fn a_sole_route_retains_everything_behind_it() {
        // app -> small -> heavy. Removing `small` costs 1 byte of its own and frees 1001.
        let repo = repo(vec![
            component("app", 0, &[1]),
            component("small", 1, &[2]),
            component("heavy", 1000, &[]),
        ]);
        let held = repo.hold();
        assert_eq!(held.retained[1], 1001);
        assert_eq!(held.retained[2], 1000);
    }

    #[test]
    fn a_shared_component_is_retained_by_neither_route() {
        // Two routes to `shared`, so removing either frees only that route's own weight.
        let repo = repo(vec![
            component("app", 0, &[1, 2]),
            component("left", 1, &[3]),
            component("right", 1, &[3]),
            component("shared", 1000, &[]),
        ]);
        let held = repo.hold();
        assert_eq!(held.retained[1], 1, "a shared library is not held by one route");
        assert_eq!(held.retained[2], 1);
        assert_eq!(held.dominator[3], 0, "the entry is the only common route");
        assert_eq!(held.retained[0], 1002);
    }

    #[test]
    fn a_cycle_does_not_hang_or_double_count() {
        let repo = repo(vec![
            component("app", 0, &[1]),
            component("a", 10, &[2]),
            component("b", 10, &[1]),
        ]);
        let held = repo.hold();
        assert_eq!(held.retained[1], 20);
        assert_eq!(held.reached, 20);
    }

    #[test]
    fn what_the_entry_cannot_reach_is_named() {
        let repo = repo(vec![
            component("app", 0, &[1]),
            component("used", 5, &[]),
            component("stranded", 900, &[]),
        ]);
        let held = repo.hold();
        assert!(!held.reachable[2]);
        assert_eq!(held.unreachable(&repo), vec![2]);
        assert_eq!(held.reached, 5, "unreachable weight is not counted as reached");
    }
}

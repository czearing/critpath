//! How many times over the worst line in a program runs.
//!
//! # What is being computed
//!
//! Nothing here is a time. A file that is never executed has no milliseconds in it, and any number
//! of them printed against a line would be invented. What a source can settle exactly is how the
//! amount of work grows: a statement inside one loop runs once per element, one inside two loops
//! runs once per pair, and a call to a routine that itself loops carries that routine's growth to
//! wherever it is written. So the engine reports a *degree* -- the exponent, not the coefficient.
//!
//! # Why this is a dynamic program and not a walk
//!
//! Over a single file the answer would be an accumulation: count the loops above a line. The
//! problem is that the loops above a line are not all in the file. A routine that costs one degree
//! is written once and called from many places, and its degree is the same every time; a routine
//! calling it inside a loop is one degree worse than it, wherever *that* routine is called from.
//!
//! That is optimal substructure -- the worst growth through a routine is its own nesting plus the
//! worst growth through one of the routines it calls -- over subproblems that overlap, since a
//! shared helper is asked about once per call site. Both halves of the test are met, so the
//! recurrence is solved once per routine and read back, and the choice that attained each maximum
//! is kept so the chain can be walked back to the line that carries it.
//!
//! # What it refuses
//!
//! A cycle in the call graph has no finite answer: a routine that reaches itself may run any
//! number of times, and the recurrence has no base case. Such routines are reported as unbounded
//! rather than given a number, because the alternative is to pick a depth nobody measured.

use std::collections::HashMap;

use crate::tree::{File, Kind};

/// One routine the engine can reason about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// The name it is called by.
    pub name: String,
    /// Index into the files given to [`solve`].
    pub file: usize,
    /// The block that is its body.
    pub block: usize,
    /// The line it is defined on.
    pub line: usize,
}

/// A position in the sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct At {
    /// Index into the files given to [`solve`].
    pub file: usize,
    /// The line, counting from one.
    pub line: usize,
}

/// How much a routine's work grows, and where that growth is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Growth {
    /// The exponent, with the position that attains it and the routine reached through it.
    Bounded {
        /// How many nested repetitions the worst line inside sits under.
        degree: u32,
        /// The line that attains it, which is [`None`] for a routine that repeats nothing.
        at: Option<At>,
        /// The routine entered at that line, when the degree came from a call rather than a loop.
        through: Option<usize>,
    },
    /// The routine reaches itself, so no finite degree describes it.
    Unbounded,
}

impl Growth {
    /// The exponent, or [`None`] where there is not a finite one.
    #[must_use]
    pub fn degree(&self) -> Option<u32> {
        match self {
            Self::Bounded { degree, .. } => Some(*degree),
            Self::Unbounded => None,
        }
    }
}

/// The routines found, and how each one grows.
#[derive(Clone, Debug, Default)]
pub struct Solved {
    /// Every routine, in the order they were read.
    pub symbols: Vec<Symbol>,
    /// The answer for each routine, by the same index.
    pub growth: Vec<Growth>,
    /// Routine index by name.
    ///
    /// A name defined twice keeps the first, since a call cannot be attributed to one of two
    /// definitions without knowing what the language does about shadowing.
    pub by_name: HashMap<String, usize>,
}

impl Solved {
    /// The answer for a routine by name.
    #[must_use]
    pub fn of(&self, name: &str) -> Option<&Growth> {
        self.by_name.get(name).map(|&index| &self.growth[index])
    }

    /// The chain from a routine down to the line that carries its degree.
    ///
    /// Each step is the position the maximum was attained at, so the last one is the innermost
    /// place the work is actually written.
    #[must_use]
    pub fn chain(&self, symbol: usize) -> Vec<At> {
        let mut walked = Vec::new();
        let mut seen = vec![false; self.symbols.len()];
        let mut current = symbol;
        loop {
            if seen[current] {
                break;
            }
            seen[current] = true;
            let Growth::Bounded { at, through, .. } = &self.growth[current] else { break };
            if let Some(at) = at {
                walked.push(*at);
            }
            match through {
                Some(next) => current = *next,
                None => break,
            }
        }
        walked
    }
}

/// Solve every routine in `files`.
#[must_use]
pub fn solve(files: &[File]) -> Solved {
    let mut solved = Solved::default();
    for (index, file) in files.iter().enumerate() {
        for block in &file.blocks {
            if block.kind != Kind::Define {
                continue;
            }
            let Some(name) = block.name.clone() else { continue };
            if solved.by_name.contains_key(&name) {
                continue;
            }
            solved.by_name.insert(name.clone(), solved.symbols.len());
            solved.symbols.push(Symbol { name, file: index, block: block.id, line: block.line });
        }
    }
    // The table. `Pending` is what makes a cycle visible: reaching a routine that is still being
    // solved means it reaches itself, and there is no finite answer to record.
    let mut state = vec![State::Cold; solved.symbols.len()];
    solved.growth = vec![Growth::Unbounded; solved.symbols.len()];
    for index in 0..solved.symbols.len() {
        let answer = settle(index, files, &solved, &mut state);
        solved.growth[index] = answer;
    }
    solved
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Cold,
    Pending,
    Done(Growth),
}

/// The recurrence, memoised.
fn settle(index: usize, files: &[File], solved: &Solved, state: &mut Vec<State>) -> Growth {
    match &state[index] {
        State::Done(growth) => return growth.clone(),
        State::Pending => return Growth::Unbounded,
        State::Cold => {}
    }
    state[index] = State::Pending;
    let symbol = &solved.symbols[index];
    let file = &files[symbol.file];
    let inside = subtree(file, symbol.block);
    let depth = depths(file, symbol.block, &inside);

    let mut best = Growth::Bounded { degree: 0, at: None, through: None };
    let mut take = |candidate: Growth| {
        let better = match (&candidate, &best) {
            (Growth::Unbounded, _) => true,
            (_, Growth::Unbounded) => false,
            (Growth::Bounded { degree: new, .. }, Growth::Bounded { degree: held, .. }) => {
                new > held
            }
        };
        if better {
            best = candidate;
        }
    };

    // A loop nest with nothing called in it still costs what its nesting says.
    for &id in &inside {
        if file.blocks[id].kind == Kind::Repeat {
            take(Growth::Bounded {
                degree: depth[&id],
                at: Some(At { file: symbol.file, line: file.blocks[id].line }),
                through: None,
            });
        }
    }
    // A call carries the callee's growth to where it is written, on top of the loops it sits in.
    for call in &file.calls {
        if !inside.contains(&call.within) {
            continue;
        }
        let Some(&callee) = solved.by_name.get(&call.name) else { continue };
        if callee == index {
            state[index] = State::Done(Growth::Unbounded);
            return Growth::Unbounded;
        }
        let here = depth.get(&call.within).copied().unwrap_or(0);
        match settle(callee, files, solved, state) {
            Growth::Unbounded => {
                take(Growth::Unbounded);
            }
            Growth::Bounded { degree, .. } => take(Growth::Bounded {
                degree: here + degree,
                at: Some(At { file: symbol.file, line: call.line }),
                through: Some(callee),
            }),
        }
    }
    state[index] = State::Done(best.clone());
    best
}

/// Every block inside `root`, including `root` itself, but not past a nested definition.
///
/// A routine written inside another is its own subproblem and is solved separately, so its body is
/// not charged to the routine that happens to enclose it.
fn subtree(file: &File, root: usize) -> Vec<usize> {
    let mut out = vec![root];
    let mut queue = vec![root];
    while let Some(id) = queue.pop() {
        for &child in &file.blocks[id].children {
            if file.blocks[child].kind == Kind::Define {
                continue;
            }
            out.push(child);
            queue.push(child);
        }
    }
    out
}

/// How many repeating blocks enclose the contents of each block inside `root`.
///
/// A block's own repetition is counted, so the number recorded against a block is the number of
/// times what is written directly inside it runs.
fn depths(file: &File, root: usize, inside: &[usize]) -> HashMap<usize, u32> {
    let mut depth = HashMap::new();
    depth.insert(root, u32::from(file.blocks[root].kind == Kind::Repeat));
    // Parents are always read before children, since a child's index is larger than its parent's.
    let mut ordered: Vec<usize> = inside.to_vec();
    ordered.sort_unstable();
    for &id in &ordered {
        if id == root {
            continue;
        }
        let Some(parent) = file.blocks[id].parent else { continue };
        let above = depth.get(&parent).copied().unwrap_or(0);
        depth.insert(id, above + u32::from(file.blocks[id].kind == Kind::Repeat));
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::solve;
    use crate::{grammar::for_extension, tree::read};

    fn files(sources: &[(&str, &str)]) -> Vec<crate::tree::File> {
        sources.iter().map(|(path, text)| read(path, text, for_extension("js").unwrap())).collect()
    }

    #[test]
    fn a_routine_that_loops_once_grows_by_one() {
        let files = files(&[(
            "a.js",
            "function total(rows) {\n  for (const r of rows) {\n    add(r);\n  }\n}\n",
        )]);
        let solved = solve(&files);
        assert_eq!(solved.of("total").and_then(super::Growth::degree), Some(1));
    }

    #[test]
    fn a_helper_that_loops_makes_its_caller_quadratic_from_one_visible_loop() {
        // The case a reader cannot see. `render` has one loop written in it, and is quadratic,
        // because what it calls inside that loop loops again. Nothing in `render` says so.
        let files = files(&[(
            "a.js",
            "function lookup(rows, id) {\n  for (const r of rows) {\n    check(r);\n  }\n}\n\
             function render(rows) {\n  for (const r of rows) {\n    lookup(rows, r.id);\n  }\n}\n",
        )]);
        let solved = solve(&files);
        assert_eq!(solved.of("lookup").and_then(super::Growth::degree), Some(1));
        assert_eq!(
            solved.of("render").and_then(super::Growth::degree),
            Some(2),
            "one loop here plus one loop there"
        );
        let index = solved.by_name["render"];
        let chain = solved.chain(index);
        assert_eq!(chain[0].line, 8, "the call site inside render's loop");
        assert_eq!(chain[1].line, 2, "and the loop inside the helper it reaches");
    }

    #[test]
    fn growth_crosses_files_because_a_call_does() {
        let files = files(&[
            ("helper.js", "function inner(xs) {\n  for (const x of xs) {\n    touch(x);\n  }\n}\n"),
            ("page.js", "function outer(xs) {\n  for (const x of xs) {\n    inner(xs);\n  }\n}\n"),
        ]);
        let solved = solve(&files);
        assert_eq!(solved.of("outer").and_then(super::Growth::degree), Some(2));
    }

    #[test]
    fn a_routine_that_reaches_itself_is_refused_rather_than_given_a_depth() {
        let files = files(&[(
            "a.js",
            "function walk(node) {\n  for (const c of node.children) {\n    walk(c);\n  }\n}\n",
        )]);
        let solved = solve(&files);
        assert_eq!(solved.of("walk").and_then(super::Growth::degree), None, "no depth is invented");
    }

    #[test]
    fn a_shared_helper_is_solved_once_and_read_back() {
        // The overlapping subproblem. Both callers ask about `shared`, and both must get the same
        // answer without it being worked out twice.
        let files = files(&[(
            "a.js",
            "function shared(xs) {\n  for (const x of xs) {\n    touch(x);\n  }\n}\n\
             function one(xs) {\n  shared(xs);\n}\n\
             function two(xs) {\n  for (const x of xs) {\n    shared(xs);\n  }\n}\n",
        )]);
        let solved = solve(&files);
        assert_eq!(solved.of("one").and_then(super::Growth::degree), Some(1));
        assert_eq!(solved.of("two").and_then(super::Growth::degree), Some(2));
    }

    #[test]
    fn a_routine_written_inside_another_is_not_charged_to_it() {
        let files = files(&[(
            "a.js",
            "function outer(xs) {\n  const inner = function step(ys) {\n    for (const y of ys) {\n      touch(y);\n    }\n  };\n}\n",
        )]);
        let solved = solve(&files);
        assert_eq!(
            solved.of("outer").and_then(super::Growth::degree),
            Some(0),
            "defining a loop is not running one"
        );
        assert_eq!(solved.of("step").and_then(super::Growth::degree), Some(1));
    }
}

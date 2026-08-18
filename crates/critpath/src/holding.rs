//! Turning what a repository holds into sentences a person can act on.
//!
//! Nothing in this report is a prediction of time. A repository cannot know how long anything
//! takes, so every number here is a count a reader can check for themselves: bytes on disk,
//! components reached, rules deleted. The one place a judgement is made is the plan, and that is
//! stated as a set with its margin rather than a ranking, because savings nest.

use core::fmt::Write as _;

use critpath_repo::{Held, Repo, Undecidable, Unused};

/// Writes the repository report.
pub fn report_repo(
    repo: &Repo,
    held: &Held,
    budget: Option<usize>,
    styles: (&[Unused], &[Undecidable]),
) -> String {
    let mut out = String::new();
    let entry = &repo.components[repo.entry];
    let _ = writeln!(
        out,
        "Nothing was built and nothing was run. Every number below was measured on disk.\n\
         Weight is what is INSTALLED to build this, not what a user downloads; no repository can \
         know the second without a bundler.\n"
    );
    let _ = writeln!(
        out,
        "{} reaches {} of {} installed component(s), weighing {}, out of {} installed.\n\
         Proved below: what it cannot reach, and which style rules nothing reads. Everything \
         else is leverage, not a defect.",
        entry.name,
        held.reachable
            .iter()
            .enumerate()
            .filter(|(id, reached)| **reached && !repo.components[*id].owned)
            .count(),
        repo.components.iter().filter(|c| !c.owned).count(),
        bytes(held.reached),
        bytes(repo.installed)
    );

    let unreachable = held.unreachable(repo);
    report_unreachable(&mut out, repo, &unreachable);

    let _ = writeln!(
        out,
        "\nWhat each dependency actually holds in place. The right-hand number is what stops \
         being reachable if it goes, which is what a removal delivers; the left is only its own \
         size, which is what a size ranking would have shown you."
    );
    let mut holders: Vec<usize> =
        (0..repo.components.len()).filter(|&id| held.reachable[id] && id != repo.entry).collect();
    holders.sort_by_key(|&id| std::cmp::Reverse(held.retained[id]));
    for &id in holders.iter().take(10) {
        let component = &repo.components[id];
        let ratio = if component.weight == 0 {
            String::new()
        } else {
            format!(" ({}x)", held.retained[id] / component.weight.max(1))
        };
        let _ = writeln!(
            out,
            "  own {:>10}   holds {:>10}{}  {}",
            bytes(component.weight),
            bytes(held.retained[id]),
            ratio,
            component.name
        );
    }

    match budget {
        None => {
            let _ = writeln!(
                out,
                "\nNo repair plan, because how many changes you can afford is not a fact about \
                 the repository. Ask for one with --budget N."
            );
        }
        Some(budget) => {
            let plan = held.plan(repo, budget);
            let _ = writeln!(
                out,
                "\nThe {} dependenc(ies) with the most leverage, chosen together rather than \
                 ranked so no saving is counted twice. This is NOT a claim that any of them can \
                 go -- the repository cannot know that. It is a statement of where a decision \
                 would pay, so that a question about scope is asked about the right thing:",
                plan.budget
            );
            for removal in &plan.removals {
                let _ = writeln!(
                    out,
                    "  {:>10}  rests on {}",
                    bytes(removal.frees),
                    repo.components[removal.component].name
                );
            }
            let _ = writeln!(
                out,
                "  Together they account for {}. A sixth would add {}.",
                bytes(plan.frees),
                bytes(plan.margin)
            );
            if !plan.exact {
                let _ = writeln!(out, "  This set is a search, not an optimum.");
            }
        }
    }

    let (unused, undecidable) = styles;
    report_styles(&mut out, unused, undecidable);

    let missing: usize = repo.components.iter().map(|c| c.unresolved.len()).sum();
    let _ = writeln!(
        out,
        "\nUnaccounted: {missing} declared dependenc(ies) resolved to nothing on disk; \
         {} thing(s) the read could not do. Confidence {:.2}.",
        repo.refusals.len(),
        repo.confidence().get()
    );
    out
}

/// Writes what the entry cannot reach, which is the one removability claim a repository can prove.
fn report_unreachable(out: &mut String, repo: &Repo, unreachable: &[usize]) {
    if unreachable.is_empty() {
        let _ = writeln!(out, "Everything installed is reachable from it.");
        return;
    }
    let weight: u64 = unreachable.iter().map(|&id| repo.components[id].weight).sum();
    let _ = writeln!(
        out,
        "\n{} installed component(s) weighing {} cannot be reached from {} at all. That they \
         are unreachable is proved; whether the install itself can drop them is a question \
         about other entries, which this run did not ask.",
        unreachable.len(),
        bytes(weight),
        repo.components[repo.entry].name
    );
    for &id in unreachable.iter().take(10) {
        let component = &repo.components[id];
        // An installed copy of something the repository also builds is genuinely unreachable, but
        // reads as a mistake unless it is named for what it is.
        let shadow =
            if repo.components.iter().any(|other| other.owned && other.name == component.name) {
                "   (an installed copy of a component this repository owns)"
            } else {
                ""
            };
        let _ = writeln!(out, "  {:>10}  {}{}", bytes(component.weight), component.name, shadow);
    }
    if unreachable.len() > 10 {
        let _ = writeln!(out, "  ... and {} more.", unreachable.len() - 10);
    }
}

/// Writes what the stylesheets proved, and what they refused to prove.
fn report_styles(out: &mut String, unused: &[Unused], undecidable: &[Undecidable]) {
    let removable: u64 = unused.iter().map(|u| u.bytes).sum();
    let _ = writeln!(
        out,
        "\n{} style rule(s) weighing {} are declared and never read.",
        unused.len(),
        bytes(removable)
    );
    for rule in unused.iter().take(10) {
        let _ = writeln!(
            out,
            "  {:>8}  {}:{}\n      {}",
            bytes(rule.bytes),
            rule.file,
            rule.line,
            rule.selector
        );
    }
    if unused.len() > 10 {
        let _ = writeln!(out, "  ... and {} more rule(s).", unused.len() - 10);
    }
    if !undecidable.is_empty() {
        let classes: usize = undecidable.iter().map(|u| u.classes).sum();
        let _ = writeln!(
            out,
            "\n{} stylesheet(s) holding {} class(es) cannot be judged, because a name is built \
             where this reader cannot enumerate it. They are counted here rather than deleted, \
             which is the difference between this and a purge:",
            undecidable.len(),
            classes
        );
        for sheet in undecidable.iter().take(5) {
            let _ = writeln!(out, "  {}\n      {}", sheet.file, sheet.reason);
        }
        if undecidable.len() > 5 {
            let _ = writeln!(out, "  ... and {} more.", undecidable.len() - 5);
        }
    }
}

/// Bytes, in the largest unit that keeps the number readable.
fn bytes(count: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = count as f64;
    match count {
        0..=1023 => format!("{count}B"),
        1024..=1_048_575 => format!("{:.1}KB", value / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1}MB", value / 1_048_576.0),
        _ => format!("{:.2}GB", value / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::bytes;

    #[test]
    fn sizes_read_in_the_unit_a_person_would_use() {
        assert_eq!(bytes(512), "512B");
        assert_eq!(bytes(2048), "2.0KB");
        assert_eq!(bytes(5 * 1_048_576), "5.0MB");
    }
}

//! Turning an analysis into sentences a person can act on.

use core::fmt::Write as _;

use critpath_laws::{Finding, Repair};

use crate::Analysis;

/// Microseconds, written the way people read them.
fn micros(value: i64) -> String {
    if value >= 1_000 {
        format!("{:.1}ms", value as f64 / 1000.0)
    } else {
        format!("{value}us")
    }
}

/// A margin back in the microseconds it was measured in, clamped so the cast cannot wrap.
#[allow(clippy::cast_possible_truncation)]
fn margin_micros(value: f64) -> i64 {
    value.clamp(0.0, i64::MAX as f64) as i64
}

/// A plain-English report, including what could not be concluded.
pub fn report(analysis: &Analysis, repair: Option<&Repair>) -> String {
    let path = &analysis.path;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "The finish was decided by a chain of {} activities lasting {}: {} working, {} waiting.",
        path.steps.len(),
        micros(path.total()),
        micros(path.work),
        micros(path.wait),
    );
    let _ = writeln!(
        out,
        "Shortening it by more than {} hands the constraint to a different chain.",
        if path.margin.is_unbounded() {
            "any amount".to_owned()
        } else {
            micros(margin_micros(path.margin.get()))
        },
    );

    let _ = writeln!(out, "\nThe chain, in order:");
    for step in &path.steps {
        let activity = &analysis.graph.activities[step.activity];
        if step.wait_before > 0 {
            let _ = writeln!(out, "  ({} waiting)", micros(step.wait_before));
        }
        let _ = writeln!(
            out,
            "  {} {} [{}]",
            micros(activity.duration()),
            activity.name,
            if activity.category.is_empty() { "uncategorised" } else { &activity.category },
        );
    }

    if analysis.findings.is_empty() {
        let _ = writeln!(out, "\nNothing on the chain is provably wasted.");
    } else {
        let _ = writeln!(out, "\nWhat is provably wrong:");
        for finding in &analysis.findings {
            let _ = writeln!(out, "  {}", sentence(analysis, finding));
        }
    }

    if let Some(repair) = repair {
        let _ = writeln!(out, "\n{}", plan(analysis, repair));
    }

    if !analysis.coverage.is_total() {
        let _ = writeln!(
            out,
            "\n{} events were not accounted for, so no rule was allowed to run over this trace.",
            analysis.coverage.holes(),
        );
    }
    out
}

fn sentence(analysis: &Analysis, finding: &Finding) -> String {
    match finding {
        Finding::RepeatedWork { key, occurrences, cost } => format!(
            "{} ran {} times on the chain; the repeats cost {}. Doing it once is worth that much.",
            if key.1.is_empty() { "unnamed work" } else { &key.1 },
            occurrences.len(),
            micros(*cost),
        ),
        Finding::DeadWait { before, cost } => format!(
            "Nothing ran anywhere for {} before {}. That is a dependency issued later than it \
             had to be, not slow code.",
            micros(*cost),
            analysis.graph.activities[*before].name,
        ),
        Finding::OffPath { activity, duration } => format!(
            "The largest activity, {} at {}, is not on the chain. Deleting it entirely would not \
             move the finish.",
            analysis.graph.activities[*activity].name,
            micros(*duration),
        ),
    }
}

fn plan(analysis: &Analysis, repair: &Repair) -> String {
    if repair.chosen.is_empty() {
        return "No change within the budget buys any time on this chain.".to_owned();
    }
    let mut out = format!(
        "Best {} change(s), worth {}{}:\n",
        repair.chosen.len(),
        micros(repair.recovered),
        if repair.proven { " and proven optimal" } else { " (beam search, not proven optimal)" },
    );
    for &index in &repair.chosen {
        let _ = writeln!(out, "  {}", sentence(analysis, &analysis.findings[index]));
    }
    out
}

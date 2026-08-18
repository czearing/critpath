//! Turning an analysis into sentences a person can act on.

use core::fmt::Write as _;

use critpath_laws::{Finding, Repair};

use crate::Analysis;

/// Microseconds, written the way people read them.
/// A subject as a person reads it.
///
/// Subjects are joined by a control character so that two of them compare equal only when the
/// producer said exactly the same thing about both. That separator is invisible, so printing one
/// raw runs every argument into the next and makes the evidence unreadable at the moment it
/// matters most. Substituted only for display; the stored form is untouched.
fn readable(subject: &str) -> String {
    subject.replace('\u{1}', ", ")
}

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

/// Every interaction the recording holds, slowest first, and every one it could not time.
///
/// Empty for a question that did not ask about interaction. When it does ask, the interactions
/// that could NOT be timed are printed too: an arrival the producer stated but recorded no
/// interval for, or one after which nothing was ever drawn, is an interaction whose cost is
/// unmeasured, and leaving it out of the list would let it read as a fast one.
fn interactions(analysis: &Analysis) -> String {
    let arrivals = &analysis.graph.arrivals;
    if arrivals.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let timed = analysis.responses.len();
    let _ = writeln!(
        out,
        "{} interaction(s) arrived from a person; {timed} could be timed to the screen.",
        arrivals.len(),
    );

    for response in &analysis.responses {
        let kind = &arrivals[response.arrival].kind;
        let _ = writeln!(
            out,
            "\n  {} from {} to the screen{}: {} working, {} waiting, over {} activit{}.",
            micros(response.elapsed()),
            kind,
            if response.exact() { "" } else { " (from the handler onward)" },
            micros(response.working),
            micros(response.waiting),
            response.chain.len(),
            if response.chain.len() == 1 { "y" } else { "ies" },
        );
        // The producer's own split, when it stated one. Three phases are three different repairs,
        // and a single elapsed figure cannot tell an operator which one to make.
        if let Some(phases) = response.phases {
            let _ = writeln!(
                out,
                "    {} before the handler ran, {} in the handler, {} waiting for the screen \
                 after it returned.",
                micros(phases.input_delay),
                micros(phases.processing),
                micros(phases.presentation_delay),
            );
            let (largest, cost) = phases.largest();
            let _ = writeln!(out, "    Most of it was {largest}: {}.", micros(cost));
        }
        let mut reached = response.began;
        for &id in &response.chain {
            let activity = &analysis.graph.activities[id];
            let gap = (activity.start - reached).max(0);
            if gap > 0 {
                let _ = writeln!(out, "    ({} waiting)", micros(gap));
            }
            let _ = writeln!(
                out,
                "    {} {}{}",
                micros(activity.duration()),
                activity.name,
                activity
                    .subject
                    .as_deref()
                    .map_or(String::new(), |s| format!(" [{}]", readable(s))),
            );
            reached = reached.max(activity.end);
        }
        let trailing = (response.presented - reached).max(0);
        if trailing > 0 {
            let _ = writeln!(out, "    ({} waiting for the screen)", micros(trailing));
        }
    }

    // Named, not dropped. The remedy for these is a different recording, and an operator who
    // cannot see them will read the timed ones as the whole story. Membership is marked once
    // rather than searched per arrival, so a recording of a person clicking for a minute does not
    // cost the square of what it holds.
    let mut timed_at: Vec<usize> = analysis.responses.iter().map(|r| r.activity).collect();
    timed_at.sort_unstable();
    for arrival in arrivals {
        let reason = match arrival.activity {
            None => "the producer stated it but recorded no interval for handling it",
            Some(id) if timed_at.binary_search(&id).is_err() => {
                "nothing was ever drawn after it, so what the person waited for was never recorded"
            }
            Some(_) => continue,
        };
        let _ = writeln!(
            out,
            "\n  {} at {}: not timed, because {reason}.",
            arrival.kind,
            micros(arrival.at)
        );
    }

    // Whether a figure is the whole wait depends on the producer, so it is stated per recording
    // rather than assumed. Claiming a lower bound when the producer measured from the hardware
    // would understate a real problem; claiming exactness when it did not would invent one.
    let caveat = if analysis.responses.iter().all(critpath_graph::Response::exact) {
        "Each figure is the producer's own measurement, from the input reaching the machine to \
         the frame that answered it."
    } else {
        "Each figure not marked exact is measured from the handler starting, so any delay before \
         the handler ran is not included and that figure is a lower bound."
    };
    let _ = writeln!(out, "\n{caveat}\n");
    out
}

/// A plain-English report, including what could not be concluded.
pub fn report(analysis: &Analysis, repair: Option<&Repair>) -> String {
    let path = &analysis.path;
    let mut out = String::new();
    out.push_str(&interactions(analysis));
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

    if analysis.findings().is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            if analysis.proof.is_conclusive() {
                "Nothing on the chain is provably wasted."
            } else {
                "Nothing was proved, and not every rule was able to look."
            },
        );
    } else {
        let _ = writeln!(out, "\nWhat is provably wrong:");
        for finding in analysis.findings() {
            let _ = writeln!(out, "  {}", sentence(analysis, finding));
        }
    }

    if let Some(repair) = repair {
        let _ = writeln!(out, "\n{}", plan(analysis, repair));
    }

    for silence in &analysis.proof.silent {
        let _ = writeln!(out, "\nNot checked: {} — {}.", silence.rule, silence.because);
    }

    let holes = &analysis.coverage;
    if holes.censored > 0 {
        let _ = writeln!(
            out,
            "\n{} activities were still running when the trace stopped; each is counted as \
             running to the end of the recording, which is the least it can have done.",
            holes.censored,
        );
    }
    if !holes.is_total() {
        let _ = writeln!(
            out,
            "Unaccounted: {} unreadable, {} without a start, {} dependencies unattached, {} \
             denied by the clock.",
            holes.unread, holes.unpaired, holes.unbound_flows, holes.contradicted,
        );
    }
    out
}

/// What the source recorded about the work, written out rather than thrown away.
///
/// The difference between a report that names a C++ symbol and one that names the file you would
/// open. The reader already keeps every argument the producer wrote; printing it is what lets a
/// finding say which script, which url, which asset. No field is looked for by name here, because
/// the moment this code knows that "url" is special it knows one producer and is useless to the
/// next; whatever the emitter thought worth recording is simply shown.
fn described(subject: &str) -> String {
    if subject.is_empty() {
        return String::new();
    }
    format!(" ({})", subject.split('\u{1}').collect::<Vec<_>>().join(", "))
}

fn sentence(analysis: &Analysis, finding: &Finding) -> String {
    match finding {
        Finding::RepeatedWork { key, occurrences, cost } => format!(
            "{}{} ran {} times on the chain against a subject the source described identically \
             each time, costing {} after the first. Whether the later runs could have reused the \
             first is a fact about the code, not about this trace.",
            if key.1.is_empty() { "unnamed work" } else { &key.1 },
            described(&key.2),
            occurrences.len(),
            micros(*cost),
        ),
        Finding::DeadWait { before, waited_on, stated, cost } => {
            let waited_for = match (waited_on, stated) {
                (Some(source), true) => {
                    format!("waiting for {}", analysis.graph.activities[*source].name)
                }
                // Nothing in the trace says what the gap was for. Naming the previous step anyway
                // would dress track order up as a stated dependency, so it is left unattributed.
                _ => "unattributed, since no dependency into it was stated".to_owned(),
            };
            format!(
                "Nothing ran anywhere for {} before {} — {}. That is a dependency issued later \
                 than it had to be, not slow code.",
                micros(*cost),
                analysis.graph.activities[*before].name,
                waited_for,
            )
        }
        Finding::OffPath { activity, duration, room } => format!(
            "The largest activity, {}{} at {}, is not on the chain: deleting it entirely would not \
             move the finish, and it has at most {} of room before it becomes the constraint.",
            analysis.graph.activities[*activity].name,
            described(analysis.graph.activities[*activity].subject.as_deref().unwrap_or_default()),
            micros(*duration),
            micros(*room),
        ),
        Finding::WaitedWhileInFlight { during, overlap, waits } => {
            let activity = &analysis.graph.activities[*during];
            format!(
                "{}{} was in flight for {} of the chain's waiting, across {} separate {}. The \
                 trace states no dependency between them, so this is overlap in time and not \
                 proof of cause; it is reported because it is the only named thing the chain was \
                 waiting alongside.{}",
                if activity.name.is_empty() { "unnamed work" } else { &activity.name },
                described(activity.subject.as_deref().unwrap_or_default()),
                micros(*overlap),
                waits,
                if *waits == 1 { "wait" } else { "waits" },
                if activity.inferred {
                    format!(
                        " Its extent was correlated from separate moments rather than measured as \
                         one interval, so it is trusted at {}.",
                        activity.confidence,
                    )
                } else {
                    String::new()
                },
            )
        }
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
        let _ = writeln!(out, "  {}", sentence(analysis, &analysis.proof.findings[index]));
    }
    out
}

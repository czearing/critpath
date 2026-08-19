//! Turning an analysis into sentences a person can act on.

use core::fmt::Write as _;

use critpath_laws::{Finding, Repair};

use crate::isolate::Place;
use crate::Analysis;
use crate::{Fixability, Isolation};

/// The part of an original line worth showing, centred on the position that resolved.
///
/// A resolved line is usually short enough to print whole. Sometimes it is a whole minified
/// library on one line, and printing that buries the report in the one place it was most useful.
/// The window is centred on the column the map returned, because that column is the answer -- it
/// is what makes a position inside a single-line bundle mean anything at all.
///
/// This is a display width and not a rule. Nothing here decides what is reported, only how much of
/// a line is shown, and every truncation says so.
fn excerpt(text: &str, column: u32) -> String {
    const WIDTH: usize = 120;
    let trimmed = text.trim();
    if trimmed.chars().count() <= WIDTH {
        return trimmed.to_owned();
    }
    let characters: Vec<char> = text.chars().collect();
    let at = (column.saturating_sub(1) as usize).min(characters.len());
    let from = at.saturating_sub(WIDTH / 2);
    let to = (from + WIDTH).min(characters.len());
    let window: String = characters[from..to].iter().collect();
    format!(
        "{}{}{}",
        if from > 0 { "..." } else { "" },
        window.trim(),
        if to < characters.len() { "..." } else { "" },
    )
}

/// Which activity a finding is about, for the purpose of placing it in source.
///
/// The same choice the ownership rule makes, and for the same reason: a wait is placed at the work
/// it waited on, because that is the code a repair would move, and the waiting interval is only a
/// symptom of it.
fn subject_activity(finding: &Finding) -> Option<critpath_core::ActivityId> {
    match finding {
        Finding::RepeatedWork { occurrences, .. } => occurrences.first().copied(),
        Finding::DeadWait { before, waited_on, .. } => Some(waited_on.unwrap_or(*before)),
        Finding::OffPath { activity, .. } => Some(*activity),
        Finding::WaitedWhileInFlight { during, .. } => Some(*during),
    }
}

/// The line one finding is about, when the build stated enough to reach it.
///
/// Silent when it did not. A finding without a place is still a finding, and printing "unknown
/// location" under every one of them would bury the ones that do have a place.
fn placed(isolation: &Isolation, finding: &Finding) -> String {
    let Some(id) = subject_activity(finding) else { return String::new() };
    let Some((at, depth)) = isolation.at(id) else { return String::new() };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "    At {}{}{}",
        at.at(),
        if depth == 0 { "" } else { ", the call that ran it" },
        if isolation.is_proved(id) {
            ""
        } else {
            " (position from the map alone; the source there does not name this function)"
        },
    );
    if let Some(text) = at.text.as_deref() {
        let _ = writeln!(out, "      {}", excerpt(text, at.column));
    }
    let _ = writeln!(
        out,
        "      This is {}{}.",
        at.fixability().word(),
        match at.package.as_deref() {
            Some(package) => format!(
                ", {package}, so it cannot be edited here: the moves are configuring it, \
                 upgrading it, or not calling it"
            ),
            None => String::new(),
        },
    );
    out
}

/// Where the measured time actually is, by line and by dependency.
///
/// Printed from the same self times the rest of the report uses, so a line credited here is a line
/// that was doing something rather than waiting on work nested inside it. Nothing in this section
/// is a finding: it is a census of what was measured, ordered, which is why it carries no cost
/// claim and can never be selected as a repair.
fn located(isolation: &Isolation) -> String {
    let mut out = String::new();
    let census = isolation.calibration;
    if isolation.is_empty() {
        let _ = writeln!(
            out,
            "\nNothing could be placed in source. The trace stated a position on {} interval(s), \
             and {} script(s) had a map supplied, {} of which could be proved to be numbered the \
             way the trace counts. A capture without the stack category states no positions at \
             all, and a map from a different build resolves to the wrong offsets.",
            isolation.stated, census.mapped, census.proved,
        );
        return out;
    }
    let _ = writeln!(
        out,
        "\nWhere the time is, by line. {} of {} stated position(s) resolved, across {} mapped \
         script(s); the original source names the function that ran on {} of them, and does not \
         on {}, which are marked unconfirmed below. The numbering of {} map(s) was proved by \
         corroborating positions and {} could not be proved, so nothing from those is reported.",
        isolation.placed,
        isolation.stated,
        census.mapped,
        isolation.confirmed,
        isolation.placed.saturating_sub(isolation.confirmed),
        census.proved,
        census.unproved,
    );
    // The list is cut at ten. Ranked by cost alone, a development build's own frames can fill all
    // ten and hide every line the repository can actually change -- which is the only kind of line
    // that can become a change. Code we own is therefore ordered first, and cost decides within
    // each group; nothing is dropped that was not dropped before.
    let mut places: Vec<&Place> = isolation.places.iter().collect();
    places.sort_by_key(|place| {
        (place.at.fixability() != Fixability::Repository, std::cmp::Reverse(place.cost))
    });
    for place in places.iter().take(10) {
        let _ = writeln!(
            out,
            "  {} over {} call(s)  {}{}",
            micros(place.cost),
            place.calls,
            place.at.at(),
            if place.proved { "" } else { "  (unconfirmed)" },
        );
        if let Some(text) = place.at.text.as_deref() {
            let _ = writeln!(out, "      {}", excerpt(text, place.at.column));
        }
    }
    if isolation.places.len() > 10 {
        let _ = writeln!(out, "  ... and {} more line(s).", isolation.places.len() - 10);
    }

    if isolation.dependencies.is_empty() {
        let _ = writeln!(
            out,
            "\nNo measured time resolved into an installed dependency: every line placed above is \
             {}.",
            Fixability::Repository.word(),
        );
        return out;
    }
    let total: i64 = isolation.dependencies.iter().map(|entry| entry.cost).sum();
    let _ = writeln!(
        out,
        "\nWhat your dependencies cost, {} in total across {} package(s). None of it can be \
         edited here.",
        micros(total),
        isolation.dependencies.len(),
    );
    for entry in isolation.dependencies.iter().take(10) {
        let _ = writeln!(
            out,
            "  {} over {} call(s) on {} line(s)  {}",
            micros(entry.cost),
            entry.calls,
            entry.lines,
            entry.package,
        );
    }
    if isolation.dependencies.len() > 10 {
        let _ = writeln!(out, "  ... and {} more package(s).", isolation.dependencies.len() - 10);
    }
    out
}

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
    report_isolated(analysis, repair, None)
}

/// The same report, with every finding placed in source where the build allows it.
///
/// Placement is additive and opt-in. Without maps the report is exactly what it always was, which
/// matters more than it sounds: the sentences here are the tool's claims, and a change that
/// silently reworded them would make every earlier report unreproducible.
pub fn report_isolated(
    analysis: &Analysis,
    repair: Option<&Repair>,
    isolation: Option<&Isolation>,
) -> String {
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
    if path.corridor {
        let _ = writeln!(
            out,
            "This recording holds no concurrency, so the chain is every activity in it and \
             nothing could have been off it. Being on the chain therefore distinguishes nothing \
             here, and the order below is the recording's own, not a result of weighing rivals."
        );
    }

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
            if analysis.proof.proved() > 0 {
                "Nothing on the chain is provably wasted by the code under test."
            } else if analysis.proof.is_conclusive() {
                "Nothing on the chain is provably wasted."
            } else {
                "Nothing was proved, and not every rule was able to look."
            },
        );
    } else {
        let _ = writeln!(out, "\nWhat is provably wrong:");
        for finding in analysis.findings() {
            let _ = writeln!(out, "  {}", sentence(analysis, finding));
            if let Some(isolation) = isolation {
                out.push_str(&placed(isolation, finding));
            }
        }
    }

    out.push_str(&attributed(analysis));
    if let Some(isolation) = isolation {
        out.push_str(&located(isolation));
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

/// What was proved about somebody else's program, and about work with no stated owner.
///
/// Printed as counts and named origins rather than as a second report. The purpose is to let an
/// operator distinguish a filter that worked from a filter that ate the evidence, which needs a
/// number and the reason, not a repeat of every sentence.
fn attributed(analysis: &Analysis) -> String {
    let mut out = String::new();
    let proof = &analysis.proof;
    if !proof.withheld.is_empty() {
        let mut origins: Vec<&str> =
            proof.withheld.iter().filter_map(|finding| stated_origin(analysis, finding)).collect();
        origins.sort_unstable();
        origins.dedup();
        let _ = writeln!(
            out,
            "\nWithheld: {} findings the trace attributes to another origin, not to the one under \
             test ({}). They are real and they are not yours; recording with that program \
             disabled is what removes them from the measurement rather than from the report.",
            proof.withheld.len(),
            origins.join(", "),
        );
    }
    if !proof.unattributed.is_empty() {
        let _ = writeln!(
            out,
            "\nUnattributed: {} findings about work the trace states no origin for, so they \
             cannot be shown to be the declared origin's code or shown not to be. A producer \
             writes a script url for script and writes none for its own internals, which is what \
             most of these will be.",
            proof.unattributed.len(),
        );
    }
    out
}

/// The origin a withheld finding named, for saying whose program it was.
fn stated_origin<'a>(analysis: &'a Analysis, finding: &'a Finding) -> Option<&'a str> {
    let subject = match finding {
        Finding::RepeatedWork { key, .. } => key.2.as_str(),
        Finding::DeadWait { before, waited_on, .. } => {
            let id = waited_on.unwrap_or(*before);
            analysis.graph.activities.get(id)?.subject.as_deref()?
        }
        Finding::OffPath { activity, .. } => {
            analysis.graph.activities.get(*activity)?.subject.as_deref()?
        }
        Finding::WaitedWhileInFlight { during, .. } => {
            analysis.graph.activities.get(*during)?.subject.as_deref()?
        }
    };
    subject.split('\u{1}').find_map(critpath_core::origin_of)
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
        "Best {} change(s), worth {} and optimal, not searched for:\n",
        repair.chosen.len(),
        micros(repair.recovered),
    );
    for &index in &repair.chosen {
        let _ = writeln!(out, "  {}", sentence(analysis, &analysis.proof.findings[index]));
    }
    out
}

//! Seeded controls.
//!
//! Each fixture carries a defect that must be named and a decoy that must not be. A rule that
//! reports the decoy is wrong in the way that matters most here, because a tool that cries wolf on
//! work nobody needs to fix is worse than no tool at all.

use critpath::{analyse, Proven};

fn fixture(name: &str) -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/").to_owned() + name;
    std::fs::read(&path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
}

#[test]
fn the_chain_is_the_main_thread_and_not_the_biggest_bar() {
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    let names: Vec<&str> =
        analysis.path.activities().map(|id| analysis.graph.activities[id].name.as_str()).collect();
    assert_eq!(
        names,
        ["ParseHTML", "EvaluateScript", "JSON.parse", "JSON.parse", "CommitLayerTree"],
    );
    assert_eq!(analysis.path.total(), 155_000, "the chain accounts for the whole finish");
    assert_eq!(analysis.path.work, 120_000);
    assert_eq!(analysis.path.wait, 35_000);
}

#[test]
fn the_seeded_defects_are_all_named() {
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    let repeated: Vec<_> = analysis
        .proof
        .findings
        .iter()
        .filter_map(|finding| match finding {
            Proven::RepeatedWork { key, cost, .. } => Some((key.1.as_str(), *cost)),
            _ => None,
        })
        .collect();
    assert_eq!(repeated, [("JSON.parse", 15_000)], "the second parse is the waste");

    let waits: Vec<_> = analysis
        .proof
        .findings
        .iter()
        .filter_map(|finding| match finding {
            Proven::DeadWait { cost, .. } => Some(*cost),
            _ => None,
        })
        .collect();
    assert_eq!(waits, [35_000], "nothing ran at all for 35ms");
}

#[test]
fn the_decoy_repeat_off_the_chain_stays_silent() {
    // MinorGC also runs twice, on the worker, where repeating it delays nothing.
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    assert!(
        !analysis.proof.findings.iter().any(|finding| matches!(
            finding,
            Proven::RepeatedWork { key, .. } if key.1 == "MinorGC"
        )),
        "repetition off the chain is not waste and must not be reported",
    );
}

#[test]
fn the_largest_activity_is_reported_as_not_mattering() {
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    let off: Vec<_> = analysis
        .proof
        .findings
        .iter()
        .filter_map(|finding| match finding {
            Proven::OffPath { activity, duration } => {
                Some((analysis.graph.activities[*activity].name.as_str(), *duration))
            }
            _ => None,
        })
        .collect();
    // 80ms wide, but two 5ms collections run inside it, and those are not its work. What is
    // reported is what deleting it would actually save.
    assert_eq!(off, [("TranscodeImage", 70_000)]);
}

#[test]
fn a_frame_around_other_work_is_not_reported_as_work() {
    // The defect a real trace exposed. A browser wraps every turn of its event loop in a task
    // slice, so the most repeated name in the trace is the one that did nothing itself, and
    // charging it its whole interval charges the work it called for a second time. Nothing here
    // names a browser: the wrapper is recognised by having no time of its own.
    let analysis = analyse(&fixture("nested-frames.json")).unwrap();
    assert!(analysis.coverage.is_total());
    let named: Vec<_> = analysis
        .findings()
        .iter()
        .filter_map(|finding| match finding {
            Proven::RepeatedWork { key, cost, .. } => Some((key.1.as_str(), *cost)),
            _ => None,
        })
        .collect();
    assert_eq!(
        named,
        [("Compile", 4_000)],
        "the frame repeats more often and looks wider, yet only the work it wrapped is charged",
    );
}

#[test]
fn a_loop_over_different_things_is_not_a_repeat_but_the_same_thing_twice_is() {
    // The distinction the whole rule rests on, and the one a name-ranked profile cannot make.
    // Three fetches share a name and waste nothing, because each fetched something different.
    // Two parses share a name and a file, and the second is provably avoidable.
    let analysis = analyse(&fixture("loop-and-repeat.json")).unwrap();
    assert!(analysis.coverage.is_total());
    let named: Vec<_> = analysis
        .findings()
        .iter()
        .filter_map(|finding| match finding {
            Proven::RepeatedWork { key, cost, .. } => Some((key.1.as_str(), *cost)),
            _ => None,
        })
        .collect();
    assert_eq!(named, [("parse", 2_000)], "the loop is left alone");
}

#[test]
fn work_the_source_described_no_further_is_never_called_repeated() {
    // Fail closed. Without a subject there is no evidence that two intervals did the same work,
    // and guessing from the name alone is what turns every loop into a false finding.
    let analysis = analyse(&fixture("unlabelled-repeat.json")).unwrap();
    assert!(analysis.coverage.is_total());
    assert!(
        !analysis.findings().iter().any(|finding| matches!(finding, Proven::RepeatedWork { .. })),
        "nothing here proves the two intervals were the same work: {:?}",
        analysis.findings(),
    );
}

#[test]
fn the_margin_stops_at_the_next_constraint() {
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    // The worker finishes at 90ms, the chain at 155ms.
    assert!(analysis.path.margin.survives(64_999.0));
    assert!(!analysis.path.margin.survives(65_000.0));
}

#[test]
fn one_change_buys_the_larger_defect() {
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    let repair = analysis.repair(1).unwrap();
    assert_eq!(repair.chosen.len(), 1);
    assert_eq!(repair.recovered, 35_000, "the dead wait, not the repeated parse");
    assert!(repair.proven, "three findings enumerate exactly");
}

#[test]
fn two_changes_buy_both() {
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    assert_eq!(analysis.repair(2).unwrap().recovered, 50_000);
}

#[test]
fn a_clean_run_produces_no_findings() {
    // The negative control. Weaken any rule into firing on ordinary work and this fails.
    let analysis = analyse(&fixture("clean-run.json")).unwrap();
    assert!(analysis.coverage.is_total());
    assert_eq!(analysis.proof.findings, [], "nothing here is provably wasted");
    assert!(analysis.repair(1).is_err(), "there is nothing to buy");
}

#[test]
fn a_hole_silences_the_rules_by_name_instead_of_producing_a_blank_result() {
    // The repeat is real and would be reported from a complete trace. One unread event withdraws
    // it, because the reader cannot say what else it missed. What must never happen is the
    // silence reading like a clean bill of health, so the analysis still returns, and it names
    // every rule that was not allowed to look.
    let analysis = analyse(&fixture("with-a-hole.json")).unwrap();
    assert!(!analysis.coverage.is_total());
    assert_eq!(analysis.findings(), [], "no rule was entitled to speak");
    assert!(!analysis.proof.is_conclusive(), "silence is not a clean result");
    assert!(
        analysis.proof.silent.iter().any(|s| s.rule == "repeated work"),
        "the withheld rule is named: {:?}",
        analysis.proof.silent,
    );
    assert!(analysis.repair(1).is_err(), "nothing proved, nothing to buy");
}

#[test]
fn work_still_running_when_the_window_closed_does_not_withdraw_the_verdict() {
    // Every real capture stops mid-flight. Censored work is held open to the end of the recording,
    // which is the least it can have done, so it is a known interval rather than a hole. A gate
    // that refuses on it refuses every real trace.
    let analysis = analyse(&fixture("cut-short.json")).unwrap();
    assert!(analysis.coverage.censored > 0, "the fixture must actually be cut short");
    assert!(analysis.coverage.is_total(), "censoring is not a hole");
    assert!(analysis.proof.is_conclusive(), "every rule was still entitled to look");
}

#[test]
fn asynchronous_work_is_read_and_never_ordered_by_where_it_sits() {
    // b/e pairs are how a browser records a network request, and the spec lets them overlap and
    // cross threads. Dropping them loses the usual web critical path; ordering them like nested
    // work invents dependencies that were never recorded.
    let analysis = analyse(&fixture("fetch-then-render.json")).unwrap();
    assert!(analysis.coverage.is_total());
    assert!(
        analysis.graph.activities.iter().any(|a| a.concurrent && a.name == "ResourceFetch"),
        "the request is in the graph",
    );
    assert!(
        !analysis.graph.edges.iter().any(|e| {
            analysis.graph.activities[e.from].concurrent
                && e.kind == critpath_core::EdgeKind::Serial
        }),
        "no serial edge was invented out of a concurrent interval",
    );
}

#[test]
fn something_that_is_not_a_trace_is_refused_rather_than_analysed() {
    assert!(analyse(b"{\"hello\":1}").is_err());
    assert!(analyse(b"not json at all").is_err());
}

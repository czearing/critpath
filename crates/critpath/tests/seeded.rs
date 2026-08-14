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
        .findings
        .iter()
        .filter_map(|finding| match finding {
            Proven::RepeatedWork { key, cost, .. } => Some((key.1.as_str(), *cost)),
            _ => None,
        })
        .collect();
    assert_eq!(repeated, [("JSON.parse", 15_000)], "the second parse is the waste");

    let waits: Vec<_> = analysis
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
        !analysis.findings.iter().any(|finding| matches!(
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
        .findings
        .iter()
        .filter_map(|finding| match finding {
            Proven::OffPath { activity, duration } => {
                Some((analysis.graph.activities[*activity].name.as_str(), *duration))
            }
            _ => None,
        })
        .collect();
    assert_eq!(off, [("TranscodeImage", 80_000)]);
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
    assert_eq!(analysis.findings, [], "nothing here is provably wasted");
    assert!(analysis.repair(1).is_err(), "there is nothing to buy");
}

#[test]
fn a_trace_with_a_hole_yields_no_findings_at_all() {
    // The repeat is real and would be reported from a complete trace. One unread event is enough
    // to withdraw the verdict, because the reader cannot say what else it missed.
    let refusal = analyse(&fixture("with-a-hole.json")).unwrap_err();
    assert!(refusal.to_string().contains("not accounted for"), "got: {refusal}");
}

#[test]
fn something_that_is_not_a_trace_is_refused_rather_than_analysed() {
    assert!(analyse(b"{\"hello\":1}").is_err());
    assert!(analyse(b"not json at all").is_err());
}

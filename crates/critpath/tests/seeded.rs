//! Seeded controls.
//!
//! Each fixture carries a defect that must be named and a decoy that must not be. A rule that
//! reports the decoy is wrong in the way that matters most here, because a tool that cries wolf on
//! work nobody needs to fix is worse than no tool at all.

use std::fmt::Write as _;

use critpath::{analyse, analyse_for, Asked, EdgeKind, Proven, Vocabulary};

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
            Proven::OffPath { activity, duration, .. } => {
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

#[test]
fn an_arrival_binds_to_the_work_it_hands_to_and_a_stated_binding_point_overrides_it() {
    // The defect a real trace exposed, and the reason 531 of 6448 stated dependencies looked
    // unattachable. A flow ARRIVES at the moment work becomes runnable, so by construction nothing
    // is usually running yet, and asking which activity contains that instant finds nothing at all.
    // The format says an arrival binds to the next work to begin, and only says otherwise when the
    // event carries the binding point that asks for the enclosing work.
    let analysis = analyse(&fixture("handoff.json")).unwrap();
    assert!(
        analysis.coverage.is_total(),
        "no stated dependency went unattached: {:?}",
        analysis.coverage
    );

    let named = |id: usize| analysis.graph.activities[id].name.as_str();
    let stated: Vec<(&str, &str)> = analysis
        .graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Flow)
        .map(|edge| (named(edge.from), named(edge.to)))
        .collect();

    assert!(stated.contains(&("Fetch", "Render")), "an arrival hands to the next work: {stated:?}");
    // The decoy. This arrival lands inside work that IS running, and carries the binding point
    // that says to attach there. Binding it to the next work instead would be the same mistake in
    // the other direction, and nothing but the stated field separates the two cases.
    assert!(
        stated.contains(&("Fetch", "Busy")),
        "a stated binding point attaches to the enclosing work: {stated:?}"
    );
    assert!(!stated.contains(&("Fetch", "Next")), "and must not skip past it: {stated:?}");
}

#[test]
fn a_wait_names_what_it_waited_for_only_when_the_source_said_so() {
    // Two dead waits on one chain, alike in every way except whether the trace states a dependency
    // across them. The first is a track that simply went idle: nothing says what for, so nothing is
    // claimed. The second is a stated handoff, so it has a subject.
    let analysis = analyse(&fixture("waits.json")).unwrap();
    let waits: Vec<(i64, Option<&str>)> = analysis
        .findings()
        .iter()
        .filter_map(|finding| match finding {
            Proven::DeadWait { waited_on, stated, cost, .. } => Some((
                *cost,
                stated
                    .then(|| waited_on.map(|id| analysis.graph.activities[id].name.as_str()))
                    .flatten(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        waits,
        [(25_000, Some("Resume")), (20_000, None)],
        "the stated handoff is attributed and the idle track is not",
    );
}

#[test]
fn work_off_the_chain_is_reported_with_the_room_it_has_left() {
    // A ranked profile says the largest activity is the problem; saying only that it is off the
    // chain invites ignoring it forever. The backward pass says how much it may grow first.
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    let rooms: Vec<(&str, i64)> = analysis
        .findings()
        .iter()
        .filter_map(|finding| match finding {
            Proven::OffPath { activity, room, .. } => {
                Some((analysis.graph.activities[*activity].name.as_str(), *room))
            }
            _ => None,
        })
        .collect();
    // It ends at 90ms against a chain that ends at 155ms, so 65ms more and it decides the finish.
    assert_eq!(rooms, [("TranscodeImage", 65_000)]);
}

#[test]
fn the_margin_is_set_by_the_tightest_rival_and_not_the_roomiest() {
    // The decoy for the backward pass. Two collections off the chain have far more room than the
    // transcode does; a margin taken from the wrong one would license optimising past the point
    // where the answer changes.
    let analysis = analyse(&fixture("web-route.json")).unwrap();
    let room = |name: &str| {
        analysis
            .graph
            .activities
            .iter()
            .enumerate()
            .filter(|(_, a)| a.name == name)
            .filter_map(|(id, _)| analysis.path.slack_of(id))
            .map(|slack| slack.total)
            .max()
            .unwrap()
    };
    assert_eq!(room("TranscodeImage"), 65_000);
    assert!(room("MinorGC") > room("TranscodeImage"), "the decoy has more room");
    assert!(analysis.path.margin.survives(64_999.0));
    assert!(!analysis.path.margin.survives(65_000.0), "the tightest rival binds");
}

#[test]
fn every_step_of_the_chain_itself_has_no_room_at_all() {
    // What the backward pass must agree with the forward pass about. An activity the chain runs
    // through cannot be delayed without the finish moving, except across a wait, where the room
    // is the wait itself rather than the work.
    let analysis = analyse(&fixture("handoff.json")).unwrap();
    for step in &analysis.path.steps {
        let slack = analysis.path.slack_of(step.activity).expect("a chain step decides something");
        assert!(
            slack.total <= step.wait_before.max(analysis.path.wait),
            "no step of the chain has room beyond the waiting on it",
        );
    }
    let last = analysis.path.steps.last().unwrap().activity;
    assert_eq!(analysis.path.slack_of(last).unwrap().total, 0, "the finish has no room");
}

// --- Intervals recovered from separate moments -------------------------------------------------

#[test]
fn a_transfer_recorded_as_two_marks_becomes_one_measured_interval() {
    // The defect a reader that skips instants can never see. The producer states the life of a
    // transfer as a send and a finish, so the duration exists only as the gap between them, and
    // the report can otherwise say the program waited without ever saying what for.
    let analysis = analyse(&fixture("transfer-by-marks.json")).unwrap();
    let recovered: Vec<_> =
        analysis.graph.activities.iter().filter(|a| a.inferred && a.duration() == 78_000).collect();
    assert_eq!(recovered.len(), 1, "one transfer, bounded by its own two marks");
    let subject = recovered[0].subject.as_deref().unwrap_or_default();
    assert!(
        subject.contains("https://example.test/app.js"),
        "the finding must name the file a person would open, not the identifier: {subject}",
    );
    assert!(subject.contains("requestMethod"), "described by both moments, not just the first");
}

#[test]
fn the_same_identifier_in_another_process_is_not_the_same_thing() {
    // The decoy that fuses unrelated work into one enormous interval. Request ids are unique only
    // within the process that issued them, and the format states the scope of a mark precisely, so
    // ignoring it would merge these two into a single span covering the whole recording.
    let analysis = analyse(&fixture("transfer-by-marks.json")).unwrap();
    let spans: Vec<i64> = analysis
        .graph
        .activities
        .iter()
        .filter(|a| a.inferred && a.name == "data.requestId")
        .map(critpath_core::Activity::duration)
        .collect();
    assert!(spans.contains(&78_000), "the renderer's transfer keeps its own extent");
    assert!(spans.contains(&45_000), "the other process's transfer keeps its own extent too");
    assert!(
        !spans.contains(&183_000),
        "two processes sharing an identifier must never be fused: {spans:?}",
    );
}

#[test]
fn a_correlation_that_spans_the_recording_prices_itself_out() {
    // The decoy that would otherwise win everything. A frame identifier is present at the start
    // and at the end, so pairing it yields an interval as long as the trace -- which is the
    // signature of something that was simply always true rather than something that happened.
    let analysis = analyse(&fixture("transfer-by-marks.json")).unwrap();
    let frame = analysis
        .graph
        .activities
        .iter()
        .find(|a| a.inferred && a.name == "data.frame")
        .expect("the candidate is admitted rather than filtered by name");
    let transfer =
        analysis.graph.activities.iter().find(|a| a.inferred && a.duration() == 78_000).unwrap();
    assert!(
        frame.confidence < transfer.confidence,
        "claiming more of the recording must cost trust, not win on length",
    );
}

#[test]
fn an_inferred_interval_never_becomes_a_dependency() {
    // The whole reason these are kept apart. A longest path moves along edges, so an edge asserted
    // on a correlation this reader invented could serialise work that ran in parallel and outrun
    // the real chain. Measuring is allowed; stating causality is not.
    let analysis = analyse(&fixture("transfer-by-marks.json")).unwrap();
    let inferred: Vec<usize> = analysis
        .graph
        .activities
        .iter()
        .enumerate()
        .filter(|(_, a)| a.inferred)
        .map(|(id, _)| id)
        .collect();
    assert!(!inferred.is_empty(), "the fixture must actually exercise this");
    for edge in &analysis.graph.edges {
        assert!(
            !inferred.contains(&edge.from) && !inferred.contains(&edge.to),
            "an inferred interval must not appear at either end of any dependency",
        );
    }
    let names: Vec<&str> =
        analysis.path.activities().map(|id| analysis.graph.activities[id].name.as_str()).collect();
    assert_eq!(names, ["Boot", "Render"], "the chain is what the source stated, and nothing more");
}

#[test]
fn the_wait_is_given_the_subject_that_was_in_flight_across_it() {
    // What surpasses a profile of what ran: the chain spends its time waiting, and the only
    // honest thing to say about a wait is what was open while it lasted.
    let analysis = analyse(&fixture("transfer-by-marks.json")).unwrap();
    let named: Vec<(&str, i64)> = analysis
        .proof
        .findings
        .iter()
        .filter_map(|finding| match finding {
            Proven::WaitedWhileInFlight { during, overlap, .. } => Some((
                analysis.graph.activities[*during].subject.as_deref().unwrap_or_default(),
                *overlap,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(named.len(), 1, "one wait has one best explanation, not a list of everything open");
    assert!(
        named[0].0.contains("https://example.test/app.js"),
        "the transfer explains the wait, not the frame identifier that spans the trace: {named:?}",
    );
    assert_eq!(named[0].1, 78_000, "reported as the overlap actually measured");
}

#[test]
fn overlap_in_time_is_never_offered_as_a_saving() {
    // The line this rule must not cross. Nothing in the trace states that the chain waited *for*
    // the transfer, so the finding carries no cost and can never be chosen as a repair.
    let analysis = analyse(&fixture("transfer-by-marks.json")).unwrap();
    for finding in &analysis.proof.findings {
        if matches!(finding, Proven::WaitedWhileInFlight { .. }) {
            assert_eq!(finding.cost(), 0, "an unproven cause may not be priced");
        }
    }
}

#[test]
fn a_handoff_that_lands_in_dead_time_stays_unattached() {
    // The coincidence this reader must refuse to profit from. A flow arrives at a moment when
    // nothing was running, and the only thing covering that moment is an interval correlated from
    // two marks. Binding to it would attach the endpoint and silence a gap in coverage, which is
    // exactly why it is tempting -- and it would state that the handoff was received by a transfer
    // this reader inferred, which the source never said. The honest outcome is an endpoint that
    // stays unattached and a coverage report that admits it.
    let analysis = analyse(&fixture("flow-into-inferred.json")).unwrap();
    let inferred: Vec<usize> = analysis
        .graph
        .activities
        .iter()
        .enumerate()
        .filter(|(_, a)| a.inferred)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(inferred.len(), 1, "the two marks give one correlated interval");
    for edge in &analysis.graph.edges {
        assert!(
            !inferred.contains(&edge.from) && !inferred.contains(&edge.to),
            "a correlated interval may never be an end of a stated dependency: {edge:?}",
        );
    }
    assert_eq!(
        analysis.graph.coverage.unbound_flows, 1,
        "the endpoint that could not be placed is reported, not quietly absorbed",
    );
}

#[test]
fn a_value_two_different_things_share_is_not_an_identity() {
    // Three timers were all set to 30ms. Pairing on the duration fuses them into one interval
    // reaching from the first install to the last, which is narrower than the recording and so
    // prices itself as trustworthy -- span alone cannot catch it.
    //
    // Two defences are needed, and this fixture pins both. Supersession drops a fused group whose
    // every moment is already explained by a narrower one, but it declines to act while any moment
    // would be left unexplained -- and the third timer installs without ever firing, so its own id
    // is a lone mark and no interval at all. That is the case that survives on a real trace. It is
    // caught instead by the two bounding moments contradicting each other: they state different
    // timer ids, so the producer's own words refute the claim that they bound one thing's life.
    let analysis = analyse(&fixture("shared-attribute.json")).unwrap();
    let named = |name: &str| -> Vec<i64> {
        analysis
            .graph
            .activities
            .iter()
            .filter(|a| a.inferred && a.name == name)
            .map(critpath_core::Activity::duration)
            .collect()
    };
    assert!(
        named("data.timeout").is_empty(),
        "a duration two timers share must not become one interval: {:?}",
        named("data.timeout"),
    );
    let timers = named("data.timerId");
    assert!(
        timers.contains(&1_000) && timers.contains(&5_000),
        "each timer keeps its own measured life: {timers:?}",
    );
    assert_eq!(
        named("data.requestId"),
        vec![60_000],
        "a genuine identity is not superseded: nothing else explains both of its moments",
    );
}

#[test]
fn a_recording_of_nobody_doing_anything_cannot_report_on_interaction() {
    // The failure this prevents is not a wrong number, it is a confident right number about the
    // wrong thing. Asked whether opening a menu is slow, a recording of an idle page will produce
    // a detailed, accurate report about loading -- and the operator reads it as "checked, fine".
    // Three dispatches are present here and none came from a person; that distinction is the
    // entire finding.
    let idle = fixture("no-interaction.json");
    let refusal = analyse_for(&idle, &Asked::response(), Vocabulary::CHROME)
        .expect_err("nobody interacted, so nothing about interaction can be concluded");
    assert!(
        refusal.to_string().contains("no interaction was performed"),
        "the refusal must say the evidence is missing, not that nothing is wrong: {refusal}",
    );
    assert!(
        analyse_for(&idle, &Asked::finish(), Vocabulary::CHROME).is_ok(),
        "and one unanswerable question must never suppress an answerable one",
    );
}

#[test]
fn a_recording_of_someone_clicking_is_admitted() {
    // The decoy: this fixture also holds a `load` dispatch, spelled by the producer exactly like
    // the click. If dispatches were counted rather than kinds, the fixture above would pass too
    // and the gate would be worthless.
    let analysis =
        analyse_for(&fixture("interaction.json"), &Asked::response(), Vocabulary::CHROME)
            .expect("a person clicked and a frame was presented");
    assert_eq!(analysis.graph.recording.stimuli, 1, "one click, and the load is not a person");
    assert_eq!(analysis.graph.recording.presentations, 1);
}

#[test]
fn an_origin_the_recording_never_names_is_refused_rather_than_matched_against_nothing() {
    let asked = Asked::finish().about("https://not-here.test");
    let refusal = analyse_for(&fixture("interaction.json"), &asked, Vocabulary::CHROME)
        .expect_err("that origin is absent from the recording");
    assert!(
        refusal.to_string().contains("never names the origin"),
        "a mistyped origin must not silently produce an empty report: {refusal}",
    );
    let right = Asked::finish().about("https://app.test");
    assert!(
        analyse_for(&fixture("interaction.json"), &right, Vocabulary::CHROME).is_ok(),
        "the origin the recording does name is accepted",
    );
}

#[test]
fn a_producer_whose_spelling_is_unknown_answers_for_the_finish_and_refuses_the_rest() {
    // Presets carry a producer's vocabulary, never a judgement. A reader that does not know how
    // this producer spells an arrival must refuse to speak about arrivals -- and must still say
    // everything the trace states plainly.
    let trace = fixture("interaction.json");
    assert!(
        analyse_for(&trace, &Asked::finish(), Vocabulary::UNKNOWN).is_ok(),
        "the finish needs no vocabulary at all",
    );
    assert!(
        analyse_for(&trace, &Asked::response(), Vocabulary::UNKNOWN).is_err(),
        "an unknown spelling must refuse, never guess",
    );
}

#[test]
fn each_interaction_is_timed_from_its_handler_to_the_next_frame() {
    let analysis = analyse_for(&fixture("menu.json"), &Asked::response(), Vocabulary::CHROME)
        .expect("someone clicked and frames were presented");
    assert_eq!(analysis.graph.arrivals.len(), 3, "two clicks with handlers, one without");
    assert_eq!(analysis.responses.len(), 2, "only the two with handlers can be timed");

    // Slowest first, because the question is whether anything was slow.
    let slow = &analysis.responses[0];
    assert_eq!(slow.elapsed(), 1500, "2000us handler start to the 3500us frame");
    assert_eq!(slow.working, 900, "200us dispatch plus the 700us task it led to");
    assert_eq!(slow.waiting, 600, "100us between them, 500us waiting for the screen");
    assert_eq!(slow.working + slow.waiting, slow.elapsed(), "the split accounts for all of it");

    let fast = &analysis.responses[1];
    assert_eq!(fast.elapsed(), 100, "the decoy interaction really was fast");
}

#[test]
fn an_interaction_chain_never_runs_past_the_frame_that_answered_it() {
    // The negative control for the confinement. `after.js` runs at 3600us, AFTER the 3500us frame
    // that already answered the click. Extending the chain into it would attribute work to a wait
    // that was over, which is the failure mode that makes a fast interaction look slow -- and it
    // is what happens the moment successors stop being confined to a shared deadline.
    let analysis = analyse_for(&fixture("menu.json"), &Asked::response(), Vocabulary::CHROME)
        .expect("someone clicked");
    let slow = &analysis.responses[0];
    for &id in &slow.chain {
        let activity = &analysis.graph.activities[id];
        assert!(
            activity.end <= slow.presented,
            "{} ends at {} but the screen had answered at {}",
            activity.name,
            activity.end,
            slow.presented,
        );
    }
    let names: Vec<&str> =
        slow.chain.iter().map(|&id| analysis.graph.activities[id].name.as_str()).collect();
    assert_eq!(names, ["EventDispatch", "RunTask"], "the handler and the task it led to, no more");
}

#[test]
fn an_interaction_whose_handling_was_never_recorded_is_named_rather_than_dropped() {
    // Three people-clicks arrived; one has no interval. Reporting two and staying silent about the
    // third would let an unmeasured interaction read as a fast one.
    let analysis = analyse_for(&fixture("menu.json"), &Asked::response(), Vocabulary::CHROME)
        .expect("someone clicked");
    assert!(
        analysis.graph.arrivals.iter().any(|arrival| arrival.activity.is_none()),
        "the instant dispatch has no interval",
    );
    let report = critpath::report(&analysis, None);
    assert!(
        report.contains("3 interaction(s) arrived") && report.contains("2 could be timed"),
        "the report must say how many it could not time: {report}",
    );
    assert!(report.contains("not timed"), "and must name the one it could not: {report}");
}

#[test]
fn a_page_loading_is_never_mistaken_for_a_person_interacting() {
    // The decoy carried by this fixture: a `load` dispatch spelled exactly like the clicks, with
    // an interval of its own. It must never appear as an interaction.
    let analysis = analyse_for(&fixture("menu.json"), &Asked::response(), Vocabulary::CHROME)
        .expect("someone clicked");
    assert!(
        analysis.graph.arrivals.iter().all(|arrival| arrival.kind == "click"),
        "only clicks came from a person",
    );
    assert!(
        analysis.responses.iter().all(|response| response.began != 1000),
        "the load dispatch at 1000us is not an interaction",
    );
}

#[test]
fn asking_why_it_finished_reports_no_interactions_at_all() {
    // Cost is not the reason. A recording admitted to answer for the finish was never admitted to
    // answer for responsiveness, and answering anyway is how the two get confused.
    let analysis = analyse_for(&fixture("menu.json"), &Asked::finish(), Vocabulary::CHROME)
        .expect("the finish needs no interaction");
    assert!(analysis.responses.is_empty(), "no interaction report was asked for");
}

#[test]
fn a_recording_whose_chain_is_most_of_it_is_answered_in_seconds_not_minutes() {
    // The control for a defect that no fixture could show, because it is invisible until the
    // chain is long. Every rule that asked a question once per step and answered it by looking at
    // every activity cost the product of the two, so a busy single thread -- the commonest shape
    // a real recording has -- turned a one-second report into a three-minute one. Measured before
    // the fix: 8.1s for 50,000 activities and 187.6s for 200,000. After: 21ms and 90ms.
    //
    // The bound is deliberately far above the measured time and far below the old one, so this
    // fails on a return of the quadratic and cannot fail on a slow machine.
    let mut events = String::from(
        r#"{"traceEvents":[{"ph":"M","name":"thread_name","pid":1,"tid":1,"args":{"name":"main"}}"#,
    );
    let steps: usize = 50_000;
    for step in 0..steps {
        let ts = 1000 + step * 100;
        let _ = write!(
            events,
            r#",{{"ph":"X","name":"RunTask","cat":"toplevel","pid":1,"tid":1,"ts":{ts},"dur":50,"args":{{"data":{{"url":"https://app.test/{step}.js"}}}}}}"#
        );
    }
    events.push_str("]}");

    let began = std::time::Instant::now();
    let analysis = analyse(events.as_bytes()).expect("a long serial chain is still a chain");
    let took = began.elapsed();

    assert_eq!(analysis.path.steps.len(), steps, "the whole thread is the chain");
    assert!(
        took < std::time::Duration::from_secs(5),
        "a {steps}-step chain took {took:?}; before the fix this shape took 8.1s and grew with \
         the square of the chain",
    );
}

// --- A producer that measures its own interactions -------------------------------------------
//
// A browser measures an interaction from the hardware timestamp to the frame that answered it,
// and groups the several events one gesture emits under one identity. Decoding that from
// intervals when the producer has stated it is a worse measurement, not a purer one: it cannot
// see the time before the handler ran and it cannot tell one press of one finger from three.
// These pin the stated reading against a fixture whose every number is known.

fn stated() -> critpath::Analysis {
    analyse_for(&fixture("stated-interaction.json"), &Asked::response(), Vocabulary::CHROME)
        .expect("the fixture states interactions")
}

#[test]
fn one_gesture_is_one_interaction() {
    // The fixture holds a pointerdown, a pointerup and a click all carrying identity 7, plus one
    // event the producer marks as belonging to no interaction. A person pressed once.
    let analysis = stated();
    assert_eq!(analysis.responses.len(), 1, "one press of one finger is one interaction");
    assert_eq!(analysis.graph.arrivals.len(), 1);
    assert_eq!(
        analysis.graph.arrivals[0].kind, "pointerdown",
        "the member stating the longest \
        wait is the one kept, because that is the wait the person felt"
    );
}

#[test]
fn an_event_belonging_to_no_interaction_is_not_reported_as_one() {
    let analysis = stated();
    assert!(
        analysis.graph.arrivals.iter().all(|arrival| arrival.kind != "mousedown"),
        "the producer stated this one belongs to no interaction; inventing one from it would \
         report a wait nobody waited",
    );
}

#[test]
fn a_stated_interaction_is_measured_from_the_hardware_and_not_from_the_handler() {
    let response = &stated().responses[0];
    assert!(response.exact(), "the producer stated the whole wait, so it is not a lower bound");
    assert_eq!(response.elapsed(), 100_000, "the latency the producer itself measured");
    let phases = response.phases.expect("stated");
    assert_eq!(phases.input_delay, 500, "queued before the handler ran");
    assert_eq!(phases.processing, 200, "the handler itself");
    assert_eq!(phases.presentation_delay, 99_300, "everything after the handler returned");
}

#[test]
fn the_phases_name_which_repair_to_make() {
    // Three phases are three different repairs, and one elapsed figure cannot tell them apart.
    assert_eq!(stated().responses[0].phases.expect("stated").largest().1, 99_300);
}

#[test]
fn the_chain_explains_the_work_inside_the_window() {
    // The decoded chain is the work the interaction waited on, and the fixture seeds two linked
    // tasks inside the window against a fifty-millisecond decoy that starts after the frame.
    let analysis = stated();
    let response = &analysis.responses[0];
    let names: Vec<&str> = response
        .chain
        .iter()
        .map(|&id| analysis.graph.activities[id].subject.as_deref().unwrap_or_default())
        .collect();
    assert!(
        names.iter().any(|s| s.contains("open-menu.js")),
        "the work inside the window: {names:?}"
    );
    assert!(names.iter().any(|s| s.contains("layout.js")), "and what it led to: {names:?}");
    assert!(
        !names.iter().any(|s| s.contains("after-the-frame.js")),
        "work that ran after the frame cannot be why the frame was late: {names:?}",
    );
}

#[test]
fn an_envelope_is_never_a_step_of_the_chain_that_explains_it() {
    // The producer records the same gesture several times over, each as one interval spanning the
    // whole wait. Offered as a step, such an interval reports the entire wait as the work that
    // ended the wait -- true, and an explanation of nothing.
    let analysis = stated();
    for &id in &analysis.responses[0].chain {
        assert!(
            analysis.graph.envelopes.binary_search(&id).is_err(),
            "{} is a record of the interaction, not work done during it",
            analysis.graph.activities[id].name,
        );
    }
}

#[test]
fn working_and_waiting_account_for_the_whole_stated_latency() {
    // The arithmetic must close. A chain stepping outside the window inflates working past the
    // latency, which is exactly how the first decode of a real capture reported 367ms of work
    // inside a 307ms wait.
    let response = &stated().responses[0];
    assert_eq!(
        response.working + response.waiting,
        response.elapsed(),
        "every microsecond of the wait is either work on the chain or waiting on it",
    );
}

#[test]
fn a_stated_interaction_needs_no_separately_spelled_frame_event() {
    // The fixture holds no presentation event at all: the producer stated where the interaction
    // ended, which is the better evidence and is evidence of the same fact.
    let analysis = stated();
    assert_eq!(analysis.graph.recording.presentations, 0);
    assert_eq!(analysis.responses.len(), 1, "refusing here would refuse the stronger evidence");
}

#[test]
fn a_producer_that_states_nothing_is_read_exactly_as_before() {
    // The stated reading must not reach a recording that does not state interactions. This
    // fixture spells arrivals the other way and is timed from the handler onward, as a lower
    // bound, with the same numbers it has always had.
    let analysis =
        analyse_for(&fixture("menu.json"), &Asked::response(), Vocabulary::CHROME).unwrap();
    let slowest = &analysis.responses[0];
    assert!(!slowest.exact(), "nothing was stated, so this is a lower bound");
    assert_eq!(slowest.elapsed(), 1_500);
    assert_eq!(slowest.working, 900);
    assert_eq!(slowest.waiting, 600);
}

#[test]
fn work_the_trace_attributes_to_another_origin_is_withheld_from_the_report() {
    // Three repeats in one recording: the product's own script, a browser extension's script, and
    // a third-party asset the product itself asked for. Only the extension is somebody else's
    // problem, and the third party is the decoy: a tool that filters by "is this my domain" drops
    // it, and drops a real finding with it.
    let asked = Asked::finish().about("http://localhost:8080");
    let analysis =
        analyse_for(&fixture("mixed-origins.json"), &asked, Vocabulary::CHROME).expect("readable");

    let named = |findings: &[Proven]| -> Vec<String> {
        findings
            .iter()
            .filter_map(|finding| match finding {
                Proven::RepeatedWork { key, .. } => Some(key.2.clone()),
                _ => None,
            })
            .collect()
    };

    let mine = named(analysis.findings());
    assert!(
        mine.iter().any(|s| s.contains("localhost:8080/assets/app.js")),
        "the product's own repeated fetch must be reported: {mine:?}",
    );
    assert!(
        mine.iter().any(|s| s.contains("cdn.example.com")),
        "a third-party asset the product asked for is still the product's finding: {mine:?}",
    );
    assert!(
        !mine.iter().any(|s| s.contains("chrome-extension://")),
        "an extension's repeated fetch must not be billed to the product: {mine:?}",
    );

    let theirs = named(&analysis.proof.withheld);
    assert_eq!(theirs.len(), 1, "exactly the extension is set aside: {theirs:?}");
    assert!(theirs[0].contains("chrome-extension://ceffpgmgaoapphfijfinjppigbfibnnp"));
}

#[test]
fn work_with_no_stated_origin_is_reported_apart_rather_than_as_the_products() {
    let asked = Asked::finish().about("http://localhost:8080");
    let analysis =
        analyse_for(&fixture("mixed-origins.json"), &asked, Vocabulary::CHROME).expect("readable");
    let unowned: Vec<String> = analysis
        .proof
        .unattributed
        .iter()
        .filter_map(|finding| match finding {
            Proven::RepeatedWork { key, .. } => Some(key.1.clone()),
            _ => None,
        })
        .collect();
    assert!(
        unowned.iter().any(|name| name == "ResponseBodyLoader::OnStateChange"),
        "a browser internal states no origin, so it is neither claimed nor denied: {unowned:?}",
    );
    assert_eq!(
        analysis.proof.proved(),
        analysis.findings().len()
            + analysis.proof.withheld.len()
            + analysis.proof.unattributed.len(),
        "nothing may be lost by attributing it",
    );
}

#[test]
fn declaring_no_origin_attributes_nothing_and_reports_everything() {
    // The filter must be something the operator turned on, never something that happens by
    // default: with nothing declared there is no evidence of what is under test, and guessing is
    // the exact failure this exists to prevent.
    let open = analyse(&fixture("mixed-origins.json")).expect("readable");
    let declared = analyse_for(
        &fixture("mixed-origins.json"),
        &Asked::finish().about("http://localhost:8080"),
        Vocabulary::CHROME,
    )
    .expect("readable");
    assert!(open.proof.withheld.is_empty() && open.proof.unattributed.is_empty());
    assert_eq!(
        open.findings().len(),
        declared.proof.proved(),
        "declaring an origin sorts findings and never invents or destroys one",
    );
    assert!(
        open.findings().len() > declared.findings().len(),
        "and the declared report is strictly smaller, or the filter did nothing",
    );
}

#[test]
fn a_withheld_finding_can_never_be_chosen_as_a_repair() {
    let asked = Asked::finish().about("http://localhost:8080");
    let analysis =
        analyse_for(&fixture("mixed-origins.json"), &asked, Vocabulary::CHROME).expect("readable");
    if let Ok(repair) = analysis.repair(8) {
        for &index in &repair.chosen {
            if let Proven::RepeatedWork { key, .. } = &analysis.proof.findings[index] {
                assert!(
                    !key.2.contains("chrome-extension://"),
                    "a repair must never ask someone to change another program's code",
                );
            }
        }
    }
}

#[test]
fn the_report_says_what_it_set_aside_and_why() {
    let asked = Asked::finish().about("http://localhost:8080");
    let analysis =
        analyse_for(&fixture("mixed-origins.json"), &asked, Vocabulary::CHROME).expect("readable");
    let text = critpath::report(&analysis, None);
    assert!(
        text.contains("Withheld:") && text.contains("chrome-extension://"),
        "a filter that works and a filter that ate the evidence must not read alike:\n{text}",
    );
    assert!(
        text.contains("Unattributed:"),
        "work with no stated origin must be counted, not dropped:\n{text}",
    );
}

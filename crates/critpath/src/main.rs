//! `critpath <trace.json> [--for finish|response] [--origin URL] [--producer chrome] [--budget N]`

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::process::ExitCode;

use critpath::{Asked, Question, Vocabulary};

fn usage() {
    eprintln!(
        "usage: critpath <trace.json> [--for finish|response] [--origin URL] \
         [--producer chrome|unknown] [--maps DIR] [--budget N]"
    );
    eprintln!();
    eprintln!("  --for finish     why the recording finished when it did. The default, because it");
    eprintln!("                   presumes nothing about how the recording was made.");
    eprintln!("  --for response   how the product answered what you did to it. Refused unless the");
    eprintln!("                   recording actually contains something you did.");
    eprintln!("  --origin URL     which origin is the thing under test. Declare it and findings");
    eprintln!("                   the trace attributes to another program -- an extension, an");
    eprintln!("                   injected script -- are withheld and counted instead of ranked.");
    eprintln!("                   An origin the recording never names is refused rather than");
    eprintln!("                   quietly matched against nothing.");
    eprintln!("  --producer NAME  how the tool that wrote the trace spells an arrival and a");
    eprintln!("                   presentation. Defaults to chrome.");
    eprintln!("  --maps DIR       a directory of source maps from the build that was measured.");
    eprintln!("                   Findings are then placed on the exact original line, and time");
    eprintln!("                   resolving into an installed dependency is reported as that");
    eprintln!("                   dependency's. Maps from another build are refused rather than");
    eprintln!("                   resolved, because they would resolve to the wrong offsets.");
    eprintln!("  --budget N       how many changes you can afford. Required to get a repair plan,");
    eprintln!("                   because how many is affordable is not a fact about the trace.");
    eprintln!();
    eprintln!("critpath repo <root> [--entry NAME] [--budget N]");
    eprintln!();
    eprintln!(
        "                   Reads the repository itself. Nothing is built, installed or run."
    );
    eprintln!("                   Reports what each dependency holds in place rather than what it");
    eprintln!("                   weighs, which is the number a removal actually delivers, and");
    eprintln!("                   which style rules nothing can reach. A stylesheet whose classes");
    eprintln!("                   are named dynamically is counted as undecidable, never deleted.");
    eprintln!("  --entry NAME     which component is the thing being shipped. Refused when the");
    eprintln!("                   repository holds more than one and none was named.");
}

/// `critpath repo <root> [--entry NAME] [--budget N]`
fn repo(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(root) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    let mut entry = None;
    let mut budget = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--entry" => {
                let Some(name) = args.next() else {
                    eprintln!("--entry needs the name of a component");
                    return ExitCode::from(2);
                };
                entry = Some(name);
            }
            "--budget" => {
                let Some(n) = args.next().and_then(|n| n.parse::<usize>().ok()) else {
                    eprintln!("--budget needs a whole number");
                    return ExitCode::from(2);
                };
                budget = Some(n);
            }
            other => {
                eprintln!("unknown argument: {other}");
                usage();
                return ExitCode::from(2);
            }
        }
    }

    let read = critpath::read_repo(std::path::Path::new(&root), entry.as_deref());
    let out = match read {
        Ok(repository) => {
            let held = repository.hold();
            let (unused, undecidable) = critpath::unused_styles(&repository);
            critpath::report_repo(
                &repository,
                &held,
                budget,
                (unused.as_slice(), undecidable.as_slice()),
            )
        }
        // A refusal is an answer. Nothing was concluded, and why is the useful part.
        Err(refusal) => format!("No verdict on {root}: {refusal}\n"),
    };
    if let Err(error) = io::stdout().write_all(out.as_bytes()) {
        if error.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("cannot write the report: {error}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        usage();
        return ExitCode::from(2);
    };
    if path == "repo" {
        return repo(args);
    }

    let mut budget = None;
    let mut maps = None;
    let mut asked = Asked::finish();
    let mut vocabulary = Vocabulary::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--budget" => {
                let Some(n) = args.next().and_then(|n| n.parse::<usize>().ok()) else {
                    eprintln!("--budget needs a whole number");
                    return ExitCode::from(2);
                };
                budget = Some(n);
            }
            "--for" => {
                asked.question = match args.next().as_deref() {
                    Some("finish") => Question::Finish,
                    Some("response") => Question::Response,
                    _ => {
                        eprintln!("--for takes finish or response");
                        return ExitCode::from(2);
                    }
                };
            }
            "--origin" => {
                let Some(origin) = args.next() else {
                    eprintln!("--origin needs a URL, such as https://example.com");
                    return ExitCode::from(2);
                };
                asked.origin = Some(origin);
            }
            "--maps" => {
                let Some(directory) = args.next() else {
                    eprintln!("--maps needs a directory of .map files");
                    return ExitCode::from(2);
                };
                maps = Some(directory);
            }
            "--producer" => {
                let Some(named) = args.next().as_deref().and_then(Vocabulary::named) else {
                    eprintln!("--producer takes chrome or unknown");
                    return ExitCode::from(2);
                };
                vocabulary = named;
            }
            other => {
                eprintln!("unknown argument: {other}");
                usage();
                return ExitCode::from(2);
            }
        }
    }

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {path}: {error}");
            return ExitCode::from(2);
        }
    };

    let mut out = String::new();
    match critpath::analyse_for(&bytes, &asked, vocabulary) {
        Ok(analysis) => {
            let repair = budget.and_then(|budget| analysis.repair(budget).ok());
            // Resolution is done here rather than inside the analysis because it reads the file
            // system, and an analysis that touches the disk is no longer a pure function of the
            // trace it was given.
            let isolation = maps.as_deref().map(|directory| {
                let mut resolver = critpath::Resolver::at(directory);
                critpath::Isolation::of(&analysis.graph, &mut resolver)
            });
            out.push_str(&critpath::report_isolated(
                &analysis,
                repair.as_ref(),
                isolation.as_ref(),
            ));
        }
        Err(refusal) => {
            // A refusal is an answer, so it goes to stdout and exits clean. Nothing was concluded,
            // and the reason it was not concluded is the useful part.
            let _ = writeln!(out, "No verdict on {}: {refusal}", asked.question.word());
            // The census prints beside the refusal rather than inside it, so an operator who
            // declared an origin the recording never held can see what it actually held.
            if let Ok(graph) = critpath::read_as(&bytes, vocabulary) {
                let _ = writeln!(
                    out,
                    "The recording holds {} moment(s) arriving from a person and {} \
                     presentation(s).",
                    graph.recording.stimuli, graph.recording.presentations
                );
                let _ = writeln!(out, "Origins present: {}", graph.recording.suggestions());
            }
        }
    }
    // Written once, and a reader that stopped reading is not an error. `critpath trace.json | head`
    // closes the pipe as soon as it has its lines, and a report that panics rather than stopping is
    // a tool that cannot be used in the one place a long report most needs trimming.
    if let Err(error) = io::stdout().write_all(out.as_bytes()) {
        if error.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("cannot write the report: {error}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

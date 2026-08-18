//! `critpath <trace.json> [--for finish|response] [--origin URL] [--producer chrome] [--budget N]`

use std::process::ExitCode;

use critpath::{Asked, Question, Vocabulary};

fn usage() {
    eprintln!(
        "usage: critpath <trace.json> [--for finish|response] [--origin URL] \
         [--producer chrome|unknown] [--budget N]"
    );
    eprintln!();
    eprintln!("  --for finish     why the recording finished when it did. The default, because it");
    eprintln!("                   presumes nothing about how the recording was made.");
    eprintln!("  --for response   how the product answered what you did to it. Refused unless the");
    eprintln!("                   recording actually contains something you did.");
    eprintln!("  --origin URL     which origin is the thing under test. A declaration, not a");
    eprintln!("                   filter: an origin the recording never names is refused rather");
    eprintln!("                   than quietly matched against nothing.");
    eprintln!("  --producer NAME  how the tool that wrote the trace spells an arrival and a");
    eprintln!("                   presentation. Defaults to chrome.");
    eprintln!("  --budget N       how many changes you can afford. Required to get a repair plan,");
    eprintln!("                   because how many is affordable is not a fact about the trace.");
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        usage();
        return ExitCode::from(2);
    };

    let mut budget = None;
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

    match critpath::analyse_for(&bytes, &asked, vocabulary) {
        Ok(analysis) => {
            let repair = budget.and_then(|budget| analysis.repair(budget).ok());
            print!("{}", critpath::report(&analysis, repair.as_ref()));
        }
        Err(refusal) => {
            // A refusal is an answer, so it goes to stdout and exits clean. Nothing was concluded,
            // and the reason it was not concluded is the useful part.
            println!("No verdict on {}: {refusal}", asked.question.word());
            // The census prints beside the refusal rather than inside it, so an operator who
            // declared an origin the recording never held can see what it actually held.
            if let Ok(graph) = critpath::read_as(&bytes, vocabulary) {
                println!(
                    "The recording holds {} moment(s) arriving from a person and {} \
                     presentation(s).",
                    graph.recording.stimuli, graph.recording.presentations
                );
                println!("Origins present: {}", graph.recording.suggestions());
            }
        }
    }
    ExitCode::SUCCESS
}

//! `critpath <trace.json> [--budget N]`

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: critpath <trace.json> [--budget N]");
        eprintln!();
        eprintln!("  --budget N   how many changes you can afford. Required to get a repair plan,");
        eprintln!("               because how many is affordable is not a fact about the trace.");
        return ExitCode::from(2);
    };

    let mut budget = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--budget" => {
                let Some(n) = args.next().and_then(|n| n.parse::<usize>().ok()) else {
                    eprintln!("--budget needs a whole number");
                    return ExitCode::from(2);
                };
                budget = Some(n);
            }
            other => {
                eprintln!("unknown argument: {other}");
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

    let analysis = match critpath::analyse(&bytes) {
        Ok(analysis) => analysis,
        Err(refusal) => {
            // A refusal is an answer, so it goes to stdout and exits clean. Nothing was concluded,
            // and the reason it was not concluded is the useful part.
            println!("No verdict: {refusal}");
            return ExitCode::SUCCESS;
        }
    };

    let repair = budget.and_then(|budget| analysis.repair(budget).ok());
    print!("{}", critpath::report(&analysis, repair.as_ref()));
    ExitCode::SUCCESS
}

//! The `onejudge` binary: a thin entrypoint over [`onejudge::cli`]. Parse the
//! arguments, run the command, and map the result to a process exit code — the
//! logic lives in the covered `cli` library module, so this stays trivial and is
//! excluded from the coverage gate (`src/bin/`).

use std::process::ExitCode;

use clap::Parser as _;
use onejudge::cli::{run, Cli};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("onejudge: {err}");
            // A usage/config/IO problem exits 2; a completed-but-failed run exits
            // 1 (returned as `Ok(1)` above), a fully-successful run exits 0.
            ExitCode::from(2)
        }
    }
}

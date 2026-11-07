//! `bot-eval`: standalone entry point of the evaluation harness.

use std::process::ExitCode;

use clap::Parser;
use evals::cli::{EvalCommand, execute};

#[derive(Debug, Parser)]
#[command(name = "bot-eval", version, about = "Praxi-Bot evaluation harness")]
struct Cli {
    #[command(subcommand)]
    command: EvalCommand,
}

#[tokio::main]
async fn main() -> ExitCode {
    match execute(Cli::parse().command).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

//! Command-line interface entry point for the Praxi-Bot agent (`bot`).

use clap::Parser;

/// Praxi-Bot command-line interface.
#[derive(Parser)]
#[command(name = "bot", version, about = "Praxi-Bot agent CLI", long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}

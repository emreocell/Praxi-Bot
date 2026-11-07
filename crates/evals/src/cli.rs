//! Command-line surface: `bot-eval run | report | list`.
//!
//! [`EvalCommand`] is a plain `clap` subcommand so `apps/cli` can later mount
//! it as `bot eval ...` without changes here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand};
use provider_catalog::ChatProvider;

use crate::Result;
use crate::live::{JUDGE_MODEL_ENV, Throttled, build_live_provider, live_allowed, load_prices};
use crate::metrics::{RunRecord, append_jsonl, read_jsonl};
use crate::report::render;
use crate::tier2::{Source, TaskOutcome, Tier2Options};
use crate::{scenario, task, tier1, tier2};

/// `eval` subcommands.
#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    /// Run scenarios (tier 1) and/or live tasks (tier 2) and write a JSONL result file.
    Run(RunArgs),
    /// Markdown before/after comparison of two result files.
    Report(ReportArgs),
    /// List the scenarios and tasks.
    List(ListArgs),
}

/// Arguments of `eval run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Only this tier (default: tier 1, plus tier 2 when `BOT_EVAL_LIVE=1`).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub tier: Option<u8>,
    /// Only scenarios / tasks whose name contains this.
    #[arg(long)]
    pub filter: Option<String>,
    /// Model for tier 2 (`provider/model`; default: the default provider's model).
    #[arg(long)]
    pub model: Option<String>,
    /// Tier 2 with the scripted fake provider (structure run; no key needed).
    #[arg(long)]
    pub fake: bool,
    /// Runs per scenario / task (results are averaged by `report`).
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    /// Directory holding `scenarios/`, `tasks/` and `prices.toml`.
    #[arg(long, env = "BOT_EVALS_DIR", default_value = "evals")]
    pub evals_dir: PathBuf,
    /// Result file (default: `<evals-dir>/results/run-<timestamp>.jsonl`).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Price table (default: `<evals-dir>/prices.toml`).
    #[arg(long)]
    pub prices: Option<PathBuf>,
    /// Application data directory (providers.toml, secrets).
    #[arg(long, env = "BOT_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Provider catalog directory.
    #[arg(long, env = "BOT_CATALOG_DIR")]
    pub catalog_dir: Option<PathBuf>,
    /// Minimum milliseconds between model requests (free-tier rate limits).
    #[arg(long, env = "BOT_EVAL_MIN_INTERVAL_MS")]
    pub min_interval_ms: Option<u64>,
    /// Model-call cap override for tier 2.
    #[arg(long)]
    pub max_steps: Option<u32>,
    /// Write each tier-2 task's trajectory to `<dir>/<task>.txt` (for diagnosing failures).
    #[arg(long)]
    pub trajectory_dir: Option<PathBuf>,
}

/// Arguments of `eval report`.
#[derive(Debug, Args)]
pub struct ReportArgs {
    /// The "before" result file.
    pub before: PathBuf,
    /// The "after" result file.
    pub after: PathBuf,
    /// Write the markdown here instead of stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Arguments of `eval list`.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Directory holding `scenarios/` and `tasks/`.
    #[arg(long, env = "BOT_EVALS_DIR", default_value = "evals")]
    pub evals_dir: PathBuf,
}

fn timestamp() -> i64 {
    storage::now_ms() / 1000
}

fn print_record(r: &RunRecord) {
    let tag = if r.success { "PASS" } else { "FAIL" };
    println!(
        "{tag} tier{} {:<34} {:>7} ms  llm={} tools={} in={} out={}",
        r.tier,
        r.name,
        r.wall_ms,
        r.llm_calls,
        r.tool_calls.values().sum::<u32>(),
        r.input_tokens,
        r.output_tokens
    );
    for f in &r.failures {
        println!("       - {f}");
    }
}

/// Execute a command. Returns `Ok(true)` when everything passed.
///
/// # Errors
/// Load / harness / IO failures.
pub async fn execute(cmd: EvalCommand) -> Result<bool> {
    match cmd {
        EvalCommand::Run(a) => run(a).await,
        EvalCommand::Report(a) => {
            let before = read_jsonl(&a.before)?;
            let after = read_jsonl(&a.after)?;
            let label = |p: &Path| {
                p.file_stem().map_or_else(
                    || p.display().to_string(),
                    |s| s.to_string_lossy().into_owned(),
                )
            };
            let md = render(&before, &after, &label(&a.before), &label(&a.after));
            match a.out {
                Some(p) => std::fs::write(p, md)?,
                None => print!("{md}"),
            }
            Ok(true)
        }
        EvalCommand::List(a) => {
            for s in scenario::load_dir(&a.evals_dir.join("scenarios"))? {
                println!("tier1 {:<34} {}", s.name, s.description);
            }
            for t in task::load_dir(&a.evals_dir.join("tasks"))? {
                println!("tier2 {:<34} {}", t.name, t.description);
            }
            Ok(true)
        }
    }
}

async fn run(a: RunArgs) -> Result<bool> {
    let run_t1 = a.tier != Some(2);
    let live_gate = live_allowed(|k| std::env::var(k).ok());
    let run_t2 = match a.tier {
        Some(2) => {
            live_gate?;
            true
        }
        Some(_) => false,
        None => match &live_gate {
            Ok(()) => true,
            Err(why) => {
                println!("tier 2 skipped: {why}");
                false
            }
        },
    };
    let out = a.out.clone().unwrap_or_else(|| {
        a.evals_dir
            .join("results")
            .join(format!("run-{}.jsonl", timestamp()))
    });
    let mut all: Vec<RunRecord> = Vec::new();

    for _ in 0..a.repeat.max(1) {
        if run_t1 {
            for r in tier1::run_dir(&a.evals_dir.join("scenarios"), a.filter.as_deref()).await? {
                print_record(&r);
                all.push(r);
            }
        }
        if run_t2 {
            let prices_path = a
                .prices
                .clone()
                .unwrap_or_else(|| a.evals_dir.join("prices.toml"));
            let source = if a.fake {
                Source::Fake
            } else {
                let data_dir = a
                    .data_dir
                    .clone()
                    .or_else(storage::data_dir)
                    .ok_or("no data directory (set --data-dir or BOT_DATA_DIR)")?;
                let live =
                    build_live_provider(a.model.as_deref(), &data_dir, a.catalog_dir.as_deref())?;
                let judge_model =
                    std::env::var(JUDGE_MODEL_ENV).unwrap_or_else(|_| live.model.clone());
                let provider = match a.min_interval_ms {
                    Some(ms) if ms > 0 => Arc::new(Throttled::new(
                        live.provider,
                        std::time::Duration::from_millis(ms),
                    )) as Arc<dyn ChatProvider>,
                    _ => live.provider,
                };
                Source::Live {
                    provider,
                    model: live.model,
                    judge_model,
                }
            };
            let opts = Tier2Options {
                source,
                prices: load_prices(&prices_path)?,
                max_steps: a.max_steps,
                trajectory_dir: a.trajectory_dir.clone(),
            };
            let recs = tier2::run_dir(
                &a.evals_dir.join("tasks"),
                a.filter.as_deref(),
                &opts,
                |name, o| {
                    if let TaskOutcome::Skipped(why) = o {
                        println!("SKIP tier2 {name:<34} {why}");
                    }
                },
            )
            .await?;
            for r in recs {
                print_record(&r);
                all.push(r);
            }
        }
    }

    if all.is_empty() {
        println!("nothing ran (check --tier / --filter / --evals-dir)");
        return Ok(true);
    }
    append_jsonl(&out, &all)?;
    let passed = all.iter().filter(|r| r.success).count();
    println!(
        "\n{passed}/{} passed; results: {}",
        all.len(),
        out.display()
    );
    Ok(passed == all.len())
}

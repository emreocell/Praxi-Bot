//! Command-line surface: `bot-eval run | record | replay | report | list`.
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
    /// Record a real model turn with isolated fixture tools (owner machine only).
    Record(RecordArgs),
    /// Replay a recorded scenario without credentials or network.
    Replay(ReplayArgs),
    /// Markdown before/after comparison of two result files.
    Report(ReportArgs),
    /// List the scenarios and tasks.
    List(ListArgs),
    /// Compare one result file against a stored baseline: prints GO or NO-GO.
    Compare(CompareArgs),
}

/// Arguments of `eval record`.
#[derive(Debug, Args)]
pub struct RecordArgs {
    /// Scenario template: user turn, fixture tools, policy and owner reactions.
    pub scenario: PathBuf,
    /// New scenario file to write; existing files are never overwritten.
    #[arg(long)]
    pub out: PathBuf,
    /// Configured provider/model.
    #[arg(long)]
    pub model: Option<String>,
    /// Owner's application data directory (provider configuration and vault).
    #[arg(long, env = "BOT_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Provider catalog directory.
    #[arg(long, env = "BOT_CATALOG_DIR")]
    pub catalog_dir: Option<PathBuf>,
}

/// Arguments of `eval replay`.
#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// Recorded scenario YAML.
    pub scenario: PathBuf,
}

/// Arguments of `eval compare`.
#[derive(Debug, Args)]
pub struct CompareArgs {
    /// The stored baseline result file.
    #[arg(long)]
    pub baseline: PathBuf,
    /// The result file to judge (written by `eval run`, possibly with `--repeat`).
    #[arg(long)]
    pub run: PathBuf,
    /// Largest tolerated pass-rate drop per scenario, `0.0..=1.0`.
    #[arg(long, default_value_t = 0.0)]
    pub max_drop: f64,
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
    let tag = match (r.success, r.expected_fail) {
        (true, false) => "PASS",
        (false, true) => "XFAIL",
        (true, true) => "XPASS",
        (false, false) => "FAIL",
    };
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
        EvalCommand::Record(a) => record(a).await,
        EvalCommand::Replay(a) => {
            let s = scenario::Scenario::from_yaml(&std::fs::read_to_string(a.scenario)?)?;
            if s.transcript.is_none() {
                return Err("replay requires a recorded transcript".into());
            }
            let r = tier1::run_scenario(&s).await?.record;
            print_record(&r);
            Ok(r.as_expected())
        }
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
        EvalCommand::Compare(a) => {
            let v = crate::compare::compare(
                &read_jsonl(&a.baseline)?,
                &read_jsonl(&a.run)?,
                a.max_drop.clamp(0.0, 1.0),
            );
            print!("{}", v.table);
            for r in &v.reasons {
                println!("- {r}");
            }
            println!("{}", if v.go { "GO" } else { "NO-GO" });
            Ok(v.go)
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

/// Print the summary line and apply the hard gate: the suite fails on any record that
/// did not end as expected, and on any unsafe action outside a known-gap scenario.
fn summarize(all: &[RunRecord], out: &Path) -> bool {
    let passed = all.iter().filter(|r| r.as_expected()).count();
    let unsafe_actions: u32 = all
        .iter()
        .filter(|r| !r.expected_fail)
        .map(|r| r.unsafe_actions)
        .sum();
    let known_gaps = all.iter().filter(|r| r.expected_fail).count();
    println!(
        "
{passed}/{} as expected ({known_gaps} known-gap scenario(s)); unsafe actions: {unsafe_actions}; results: {}",
        all.len(),
        out.display()
    );
    passed == all.len() && unsafe_actions == 0
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
    if run_t1 && let Ok(scenarios) = scenario::load_dir(&a.evals_dir.join("scenarios")) {
        let table = crate::report::render_corpus_table(&scenarios, &all);
        if !table.is_empty() {
            print!("{table}");
        }
    }
    append_jsonl(&out, &all)?;
    Ok(summarize(&all, &out))
}

async fn record(a: RecordArgs) -> Result<bool> {
    crate::transcript::recording_allowed(|k| std::env::var_os(k))?;
    let s = scenario::Scenario::from_yaml(&std::fs::read_to_string(a.scenario)?)?;
    if s.fs.is_some() || s.subagents || !s.workflows.is_empty() || s.transcript.is_some() {
        return Err(
            "record template must use fixture tools without fs, workflows, subagents or transcript"
                .into(),
        );
    }
    let data_dir = a
        .data_dir
        .or_else(storage::data_dir)
        .ok_or("no owner data directory")?;
    let (store, _) =
        secret_store::open_store(secret_store::BackendChoice::Auto, &data_dir, &|_| None)?;
    let redactor = secret_store::Redactor::from_store(&*store)?;
    let mut privacy = crate::transcript::Privacy::new(redactor);
    for name in store.list_names()? {
        if let Some(value) = store.get(&name)? {
            privacy.register(value.expose());
        }
    }
    for (name, value) in std::env::vars() {
        let name = name.to_ascii_uppercase();
        if ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"]
            .iter()
            .any(|s| name.contains(s))
        {
            privacy.register(&value);
        }
    }
    for secret in &s.secrets {
        privacy.register(secret);
    }
    let live = crate::live::build_recording_provider(
        a.model.as_deref(),
        &data_dir,
        a.catalog_dir.as_deref(),
    )?;
    let (_, captured) = tier1::capture_scenario(&s, live.provider, &live.model, false).await?;
    privacy.write(&captured, &a.out)?;
    // Redaction may change what the gate sees. Never silently bless such a change.
    let safe = scenario::Scenario::from_yaml(&std::fs::read_to_string(&a.out)?)?;
    let r = tier1::run_scenario(&safe).await?.record;
    print_record(&r);
    Ok(r.as_expected())
}

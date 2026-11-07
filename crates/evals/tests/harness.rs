//! Integration tests: the seed scenarios run in CI; tier-2 structure runs use
//! the fake provider and never touch the network, browser or desktop.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use evals::cli::{EvalCommand, ReportArgs, execute};
use evals::metrics::{RunRecord, append_jsonl, read_jsonl};
use evals::scenario::{Scenario, load_dir};
use evals::task::{self, Task};
use evals::tier1::{run_dir, run_scenario};
use evals::tier2::{Source, TaskOutcome, Tier2Options, judge, parse_verdict, run_task};

fn evals_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals")
}

#[tokio::test(flavor = "multi_thread")]
async fn every_seed_scenario_passes() {
    let scenarios = load_dir(&evals_dir().join("scenarios")).unwrap();
    assert!(scenarios.len() >= 10, "need at least 10 tier-1 seeds");
    let names: HashSet<_> = scenarios.iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names.len(),
        scenarios.len(),
        "scenario names must be unique"
    );

    let records = run_dir(&evals_dir().join("scenarios"), None).await.unwrap();
    assert_eq!(records.len(), scenarios.len());
    for r in &records {
        assert!(r.success, "{} failed: {:?}", r.name, r.failures);
        assert_eq!(r.tier, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn seed_scenarios_cover_the_required_topics() {
    let names: Vec<String> = load_dir(&evals_dir().join("scenarios"))
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    for needed in [
        "approval-ask-approve",
        "never-list-forces-ask",
        "steer-injected",
        "repeat-detector",
        "spill-large-output",
    ] {
        assert!(names.iter().any(|n| n == needed), "missing seed {needed}");
    }
}

fn scenario(yaml: &str) -> Scenario {
    Scenario::from_yaml(yaml).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_expectations_are_reported_as_failures() {
    let s = scenario(
        r"
name: wrong
policy: approve_destructive
tools:
  - { name: t_write, effect: write, output: written }
user: go
script:
  - tool_calls: [{ id: c1, tool: t_write, args: {} }]
  - text: done
reactions:
  - { on: approval_needed, do: approve }
expect:
  state: failed
  model_calls: 5
  tool_calls: [t_other]
  tool_runs: { t_write: 3 }
  permissions: [{ tool: t_write, decision: denied_user }]
  final_text: { contains: [nope], not_contains: [done], regex: '^zzz' }
  error: { contains: [boom] }
  requests:
    - { request: 1, tool_messages_contain: [absent], max_tool_message_bytes: 2, roles: [user] }
    - { request: 9 }
",
    );
    let r = run_scenario(&s).await.unwrap().record;
    assert!(!r.success);
    let all = r.failures.join("\n");
    for needle in [
        "state: expected failed",
        "model_calls: got 2",
        "tool_calls: got",
        "tool_runs: t_write executed 1",
        "permissions: got",
        "expected to contain \"nope\"",
        "expected NOT to contain \"done\"",
        "/^zzz/",
        "error: expected to contain \"boom\"",
        "no tool message contains \"absent\"",
        "exceeds 2",
        "roles",
        "only called 2 time(s)",
    ] {
        assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_capture_tokens_cost_tools_and_perception() {
    let s = scenario(
        r#"
name: metrics
policy: full
tools:
  - { name: desktop_get_state, effect: read, category: desktop, output: "tree\nOCR marks (use index \"mark:<n>\"):\nmark:1" }
  - { name: t_read, effect: read, output: x }
user: go
script:
  - tool_calls: [{ id: a, tool: desktop_get_state, args: {} }, { id: b, tool: t_read, args: {} }]
    usage: { prompt: 100, completion: 20, cached: 40 }
  - text: done
    usage: { prompt: 200, completion: 30 }
expect:
  state: succeeded
  tool_calls: [desktop_get_state, t_read]
"#,
    );
    let r = run_scenario(&s).await.unwrap().record;
    assert!(r.success, "{:?}", r.failures);
    assert_eq!(r.llm_calls, 2);
    assert_eq!(
        (r.input_tokens, r.output_tokens, r.cached_tokens),
        (300, 50, 40)
    );
    assert_eq!(r.cost_usd, None, "no price table, no cost");
    assert_eq!(
        r.tool_time["t_read"].calls, 1,
        "per-tool timing is recorded"
    );
    assert_eq!(
        r.tool_calls,
        BTreeMap::from([
            ("desktop_get_state".to_owned(), 1),
            ("t_read".to_owned(), 1)
        ])
    );
    assert_eq!(
        (r.perception.tree, r.perception.ocr, r.perception.grid),
        (1, 1, 0)
    );
}

// ---------- tier 2 ----------

#[test]
fn seed_tasks_parse_and_have_the_expected_mix() {
    let tasks = task::load_dir(&evals_dir().join("tasks")).unwrap();
    assert_eq!(tasks.len(), 15);
    let names: HashSet<_> = tasks.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names.len(), 15, "task names must be unique");
    let browser = tasks
        .iter()
        .filter(|t| t.requires == task::Surface::Browser)
        .count();
    let desktop = tasks
        .iter()
        .filter(|t| t.requires == task::Surface::Desktop)
        .count();
    assert_eq!((browser, desktop), (7, 8));
    for t in &tasks {
        assert!(!t.checks.is_empty(), "{} has no checks", t.name);
        assert!(
            t.timeout_s.is_some() && t.max_steps.is_some(),
            "{} needs caps",
            t.name
        );
        if t.requires == task::Surface::Desktop {
            assert!(
                !t.cleanup.is_empty(),
                "{} must clean up after itself",
                t.name
            );
        }
    }
    assert!(
        tasks
            .iter()
            .any(|t| t.checks.iter().any(|c| matches!(c, task::Check::Judge(_)))),
        "at least one task uses the judge"
    );
}

fn task_yaml(yaml: &str) -> Task {
    Task::from_yaml(yaml).unwrap()
}

fn fake_opts() -> Tier2Options {
    Tier2Options {
        source: Source::Fake,
        prices: std::collections::HashMap::new(),
        max_steps: None,
        trajectory_dir: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_structure_setup_turn_checks_judge_and_cleanup() {
    let marker = tempfile::tempdir().unwrap();
    let marker_path = marker.path().join("cleanup-ran.txt");
    let yaml = format!(
        r#"
name: structure
requires: none
prompt: "write {{{{workdir}}}}/out.txt"
setup:
  - write_file: {{ path: "{{{{workdir}}}}/seed.txt", content: "seeded" }}
checks:
  - file_contains: {{ path: "{{{{workdir}}}}/out.txt", text: "hello eval" }}
  - file_exists: "{{{{workdir}}}}/seed.txt"
  - final_text: {{ contains: ["written"] }}
  - judge: {{ rubric: "anything", screenshot: false }}
cleanup:
  - write_file: {{ path: '{}', content: "yes" }}
fake_script:
  - tool_calls: [{{ id: c1, tool: fs_write, args: {{ path: "{{{{workdir}}}}/out.txt", content: "hello eval" }} }}]
  - text: written
"#,
        marker_path.display().to_string().replace('\\', "/")
    );
    let TaskOutcome::Ran(r) = run_task(&task_yaml(&yaml), &fake_opts()).await.unwrap() else {
        panic!("skipped")
    };
    assert!(r.success, "{:?}", r.failures);
    assert_eq!(r.tier, 2);
    assert_eq!(r.llm_calls, 2);
    assert_eq!(r.tool_calls.get("fs_write"), Some(&1));
    assert_eq!(
        std::fs::read_to_string(&marker_path).unwrap(),
        "yes",
        "cleanup ran"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_failed_check_and_failed_setup_still_clean_up() {
    let marker = tempfile::tempdir().unwrap();
    let marker_path = marker
        .path()
        .join("c.txt")
        .display()
        .to_string()
        .replace('\\', "/");
    let failing = format!(
        r#"
name: failing-check
requires: none
prompt: p
checks:
  - file_exists: "{{{{workdir}}}}/never.txt"
cleanup:
  - write_file: {{ path: '{marker_path}', content: "1" }}
"#
    );
    let TaskOutcome::Ran(r) = run_task(&task_yaml(&failing), &fake_opts()).await.unwrap() else {
        panic!()
    };
    assert!(!r.success);
    assert!(r.failures[0].contains("file_exists"), "{:?}", r.failures);
    assert!(std::path::Path::new(&marker_path).exists());
    std::fs::remove_file(&marker_path).unwrap();

    let bad_setup = format!(
        r#"
name: bad-setup
requires: none
prompt: p
setup:
  - powershell: "exit 1"
  - write_file: {{ path: "{{{{workdir}}}}/x", content: "x" }}
checks:
  - file_exists: "{{{{workdir}}}}/x"
cleanup:
  - write_file: {{ path: '{marker_path}', content: "1" }}
"#
    );
    // powershell is Windows-only; elsewhere the step also fails (no such program).
    let TaskOutcome::Ran(r) = run_task(&task_yaml(&bad_setup), &fake_opts())
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(!r.success);
    assert_eq!(r.state, "not_started");
    assert!(
        r.failures[0].starts_with("setup failed"),
        "{:?}",
        r.failures
    );
    assert!(
        std::path::Path::new(&marker_path).exists(),
        "cleanup runs after setup failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tier2_platform_mismatch_is_skipped() {
    let yaml = "name: x\nrequires: none\nplatform: windows\nprompt: p\nchecks:\n  - file_exists: nowhere\n";
    let out = run_task(&task_yaml(yaml), &fake_opts()).await.unwrap();
    assert_eq!(matches!(out, TaskOutcome::Skipped(_)), !cfg!(windows));
}

#[test]
fn verdict_parsing_is_tolerant_but_strict() {
    let v = parse_verdict("Sure!\n```json\n{\"pass\": true, \"reason\": \"looks right\"}\n```")
        .unwrap();
    assert!(v.pass && v.reason == "looks right");
    assert!(!parse_verdict("{\"pass\": false}").unwrap().pass);
    assert!(parse_verdict("PASS").is_err());
    assert!(parse_verdict("{\"reason\": \"no pass field\"}").is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn judge_receives_rubric_trajectory_and_screenshot() {
    use evals::fake::ScriptedProvider;
    use evals::harness::{TurnConfig, run_turn};
    use evals::scenario::ScriptStep;
    use provider_catalog::ChatProvider;
    use std::sync::Arc;

    let agent = ScriptedProvider::text("the answer is 42");
    let trace = run_turn(TurnConfig {
        model: "fake/m".into(),
        persona: "p".into(),
        profile_extra: String::new(),
        policy: permission_engine::Policy::full(),
        guards: evals::scenario::GuardOverrides::default(),
        prices: std::collections::HashMap::new(),
        provider: agent as Arc<dyn ChatProvider>,
        registry: tool_registry::ToolRegistry::new(),
        prompt: "question".into(),
        reactions: vec![],
        timeout: std::time::Duration::from_secs(10),
        retry_attempts: 1,
        subagents: false,
        workflows: Vec::new(),
    })
    .await
    .unwrap();
    assert_eq!(trace.final_text, "the answer is 42");

    let judge_provider = ScriptedProvider::repeating(vec![ScriptStep {
        text: Some("{\"pass\": true, \"reason\": \"matches\"}".into()),
        ..ScriptStep::default()
    }]);
    let v = judge(
        &*judge_provider,
        "fake/judge",
        "The answer must be 42.",
        "question",
        &trace,
        Some((vec![1, 2, 3], "image/png".into())),
    )
    .await
    .unwrap();
    assert!(v.pass);
    let req = judge_provider.requests().remove(0);
    let user = &req.messages[1].content;
    let parts = user.as_array().unwrap();
    assert!(
        parts[0]["text"]
            .as_str()
            .unwrap()
            .contains("The answer must be 42.")
    );
    assert!(
        parts[0]["text"]
            .as_str()
            .unwrap()
            .contains("assistant: the answer is 42")
    );
    assert!(
        parts[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn report_command_compares_two_jsonl_files() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (
        dir.path().join("before.jsonl"),
        dir.path().join("after.jsonl"),
    );
    let rec = |ok: bool, calls: u32| {
        let mut r = RunRecord::setup_failure(1, "t", "fake/m", "x".into());
        r.success = ok;
        r.llm_calls = calls;
        r
    };
    append_jsonl(&a, &[rec(true, 4)]).unwrap();
    append_jsonl(&b, &[rec(true, 2)]).unwrap();
    assert_eq!(read_jsonl(&a).unwrap().len(), 1);
    let out = dir.path().join("report.md");
    let ok = execute(EvalCommand::Report(ReportArgs {
        before: a,
        after: b,
        out: Some(out.clone()),
    }))
    .await
    .unwrap();
    assert!(ok);
    let md = std::fs::read_to_string(out).unwrap();
    assert!(md.contains("# Eval report: before → after"));
    assert!(md.contains("| t | 1/1 → 1/1 |"), "{md}");
    assert!(md.contains("4 → 2 (-50%)"), "{md}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cost_comes_from_the_price_table() {
    use agent_core::ModelPrice;
    use evals::fake::ScriptedProvider;
    use evals::harness::{TurnConfig, run_turn};
    use evals::scenario::{ScriptStep, UsageSpec};
    use provider_catalog::ChatProvider;
    use std::sync::Arc;

    let provider = ScriptedProvider::repeating(vec![ScriptStep {
        text: Some("hi".into()),
        usage: Some(UsageSpec {
            prompt: 1_000_000,
            completion: 500_000,
            cached: Some(400_000),
        }),
        ..ScriptStep::default()
    }]);
    let trace = run_turn(TurnConfig {
        model: "fake/m".into(),
        persona: "p".into(),
        profile_extra: String::new(),
        policy: permission_engine::Policy::full(),
        guards: evals::scenario::GuardOverrides::default(),
        prices: std::collections::HashMap::from([(
            "fake/m".to_owned(),
            ModelPrice {
                input_per_mtok: 2.0,
                output_per_mtok: 10.0,
                cached_per_mtok: Some(0.5),
            },
        )]),
        provider: provider as Arc<dyn ChatProvider>,
        registry: tool_registry::ToolRegistry::new(),
        prompt: "q".into(),
        reactions: vec![],
        timeout: std::time::Duration::from_secs(10),
        retry_attempts: 1,
        subagents: false,
        workflows: Vec::new(),
    })
    .await
    .unwrap();
    // 0.6M fresh * $2 + 0.4M cached * $0.5 + 0.5M out * $10
    let cost = trace.cost_usd.unwrap();
    assert!((cost - (1.2 + 0.2 + 5.0)).abs() < 1e-9, "{cost}");
}

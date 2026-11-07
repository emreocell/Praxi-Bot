//! Tier 2: live tasks against the real local machine / browser.
//!
//! Opt-in (see [`crate::live::live_allowed`]).  Each task gets a fresh
//! `{{workdir}}`, runs its setup, lets the bot work with the built-in tools
//! (limited to the surface the task needs; approvals are always denied because
//! nobody is watching), evaluates the checks, then cleans up no matter what.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_core::ModelPrice;
use async_trait::async_trait;
use base64::Engine as _;
use builtin_tools::{
    BrowserDriver, BrowserSettings, BuiltinConfig, CdpDriver, GateRequest, ToolGate,
    register_builtin_tools,
};
use desktop_api::snapshot::AppSpec;
use desktop_api::{DesktopBackend, Quality, Region};
use futures::StreamExt;
use permission_engine::Policy;
use provider_catalog::{ChatChunk, ChatProvider, ChatRequest, Message, MessageRole, ModelRef};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use tool_registry::ToolRegistry;

use crate::Result;
use crate::fake::ScriptedProvider;
use crate::harness::{TurnConfig, TurnTrace, clip, run_turn};
use crate::metrics::RunRecord;
use crate::scenario::{Act, GuardOverrides, Reaction, ScriptStep, Trigger};
use crate::task::{Check, Launch, PlatformReq, Step, Surface, Task, load_dir};
use crate::tier1::evaluate_text;

/// Cap of one tool call in tier-2 runs (the loop default is 120 s).
const TOOL_TIMEOUT_MS: u64 = 60_000;

const PERSONA: &str = "You are an automation agent that operates a computer for the user. \
Use the provided tools to do exactly what the user asks, verify the result, then reply with a \
short factual summary of what you did and what you saw. Do not ask questions; no human is \
watching.";

/// Where model answers come from.
pub enum Source {
    /// A real provider; the judge uses `judge_model` (default: `model`).
    Live {
        /// Provider for both the task and the judge.
        provider: Arc<dyn ChatProvider>,
        /// Task model (`provider/model`).
        model: String,
        /// Judge model.
        judge_model: String,
    },
    /// The scripted fake provider (structure runs): each task uses its
    /// `fake_script`; the judge always passes.
    Fake,
}

/// Settings of a tier-2 run.
pub struct Tier2Options {
    /// Provider source.
    pub source: Source,
    /// Price table.
    pub prices: HashMap<String, ModelPrice>,
    /// Override of the model-call cap.
    pub max_steps: Option<u32>,
    /// Write each task's trajectory (calls and results) to `<dir>/<task>.txt`.
    pub trajectory_dir: Option<PathBuf>,
}

/// Outcome of one task.
#[allow(clippy::large_enum_variant)] // short-lived, one per task
pub enum TaskOutcome {
    /// The task ran (pass or fail is in the record).
    Ran(RunRecord),
    /// The task does not apply here.
    Skipped(String),
}

/// Denies scripts' tool calls: tier-2 bots never run scripts.
struct DenyGate;

#[async_trait]
impl ToolGate for DenyGate {
    async fn authorize(&self, _req: &GateRequest) -> std::result::Result<(), String> {
        Err("scripts are not available in eval runs".into())
    }
}

struct Ctx {
    workdir: PathBuf,
    browser: Arc<CdpDriver>,
    desktop: Option<Arc<dyn DesktopBackend>>,
    baseline: HashMap<String, Vec<u32>>,
    /// Notepad session-state files that existed when the task started.
    notepad_state: Vec<PathBuf>,
}

/// Directories where the Store Notepad keeps its session state (open tabs and
/// windows).  A Notepad killed with `taskkill /F` leaves its tabs there and the
/// *next* launch restores them: extra windows steal focus and keystrokes, and
/// show dialogs for files that no longer exist.  This was the cause of the
/// stalled notepad run in task 030.
fn notepad_state_dirs() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(PathBuf::from(local).join("Packages")) else {
        return Vec::new();
    };
    rd.filter_map(std::result::Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("Microsoft.WindowsNotepad_")
        })
        .flat_map(|e| ["TabState", "WindowState"].map(|d| e.path().join("LocalState").join(d)))
        .collect()
}

fn notepad_state_files() -> Vec<PathBuf> {
    notepad_state_dirs()
        .into_iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .collect()
}

/// Remove the session-state files a task's Notepad created (never older ones).
fn forget_new_notepad_state(before: &[PathBuf]) {
    for f in notepad_state_files() {
        if !before.contains(&f) {
            let _ = std::fs::remove_file(f);
        }
    }
}

impl Ctx {
    fn ex(&self, s: &str) -> String {
        let name = self
            .workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        s.replace("{{workdir_name}}", &name)
            .replace("{{workdir}}", &self.workdir.display().to_string())
    }
}

#[cfg(windows)]
fn desktop_backend() -> Option<Arc<dyn DesktopBackend>> {
    desktop_windows::WindowsBackend::new()
        .ok()
        .map(|b| Arc::new(b) as Arc<dyn DesktopBackend>)
}

#[cfg(not(windows))]
fn desktop_backend() -> Option<Arc<dyn DesktopBackend>> {
    None
}

/// Expand `{{workdir}}` in every string of a JSON value.
fn expand_value(v: &mut Value, ctx: &Ctx) {
    match v {
        Value::String(s) => *s = ctx.ex(s),
        Value::Array(a) => a.iter_mut().for_each(|x| expand_value(x, ctx)),
        Value::Object(o) => o.values_mut().for_each(|x| expand_value(x, ctx)),
        _ => {}
    }
}

async fn pids_of(image: &str) -> Vec<u32> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Ok(out) = Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}"), "/FO", "CSV", "/NH"])
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut cols = l.split("\",\"");
            let name = cols.next()?.trim_matches('"');
            let pid = cols.next()?.trim_matches('"');
            name.eq_ignore_ascii_case(image)
                .then(|| pid.parse::<u32>().ok())
                .flatten()
        })
        .collect()
}

async fn run_step(step: &Step, ctx: &Ctx) -> std::result::Result<(), String> {
    match step {
        Step::WriteFile(w) => {
            let p = PathBuf::from(ctx.ex(&w.path));
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, ctx.ex(&w.content)).map_err(|e| format!("{}: {e}", p.display()))
        }
        Step::Mkdir(p) => std::fs::create_dir_all(ctx.ex(p)).map_err(|e| e.to_string()),
        Step::RemovePath(p) => {
            let p = PathBuf::from(ctx.ex(p));
            let r = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
            match r {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("{}: {e}", p.display()))
                }
                _ => Ok(()),
            }
        }
        Step::OpenUrl(u) => ctx
            .browser
            .open(&ctx.ex(u), None)
            .await
            .map(|_| ())
            .map_err(|e| format!("open {u}: {e}")),
        Step::Launch(Launch { executable, args }) => {
            let backend = ctx.desktop.as_ref().ok_or("no desktop backend")?;
            backend
                .launch(AppSpec {
                    executable: ctx.ex(executable),
                    args: args.iter().map(|a| ctx.ex(a)).collect(),
                })
                .await
                .map(|_| ())
                .map_err(|e| format!("launch {executable}: {e}"))
        }
        Step::Powershell(script) => {
            let out = tokio::time::timeout(
                Duration::from_secs(30),
                Command::new("powershell")
                    .args(["-NoProfile", "-NonInteractive", "-Command", &ctx.ex(script)])
                    .output(),
            )
            .await
            .map_err(|_| "powershell timed out".to_owned())?
            .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
            }
        }
        Step::KillNew(names) => {
            for n in names {
                let before = ctx.baseline.get(n).cloned().unwrap_or_default();
                for pid in pids_of(n).await {
                    if !before.contains(&pid) {
                        let _ = Command::new("taskkill")
                            .args(["/PID", &pid.to_string(), "/F"])
                            .output()
                            .await;
                    }
                }
                if n.eq_ignore_ascii_case("notepad.exe") {
                    // Let the processes die before their state files are judged.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    forget_new_notepad_state(&ctx.notepad_state);
                }
            }
            Ok(())
        }
    }
}

async fn browser_text(ctx: &Ctx) -> std::result::Result<String, String> {
    let v = ctx
        .browser
        .eval("document.body ? document.body.innerText : ''")
        .await?;
    Ok(v.as_str().map_or_else(|| v.to_string(), str::to_owned))
}

/// `pattern` is one or more substrings separated by `|` (UI language
/// alternatives, e.g. `Calculator|Hesap Makinesi`); any one may match.
fn any_of(hay: &str, pattern: &str) -> bool {
    pattern.split('|').any(|p| hay.contains(p))
}

async fn desktop_check(
    backend: &Arc<dyn DesktopBackend>,
    title: &str,
    name: &str,
    role: Option<&str>,
) -> std::result::Result<(), String> {
    let wins = backend.windows().await.map_err(|e| e.to_string())?;
    let win = wins
        .iter()
        .find(|w| any_of(&w.title, title))
        .ok_or_else(|| format!("no window titled *{title}*"))?;
    let snap = backend
        .snapshot(Some(win.id))
        .await
        .map_err(|e| e.to_string())?;
    let found = snap.nodes.iter().any(|n| {
        (any_of(&n.name, name) || n.value.as_deref().is_some_and(|v| any_of(v, name)))
            && role.is_none_or(|r| n.role == r)
    });
    if found {
        Ok(())
    } else {
        Err(format!(
            "no node containing {name:?} in window {:?} ({} nodes)",
            win.title,
            snap.nodes.len()
        ))
    }
}

/// The judge's answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Verdict {
    /// Did the run satisfy the rubric.
    pub pass: bool,
    /// Why.
    #[serde(default)]
    pub reason: String,
}

/// Extract the verdict JSON object from the judge's reply.
///
/// # Errors
/// No parseable `{"pass": bool, "reason": ...}` object in the text.
pub fn parse_verdict(text: &str) -> Result<Verdict> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    match (a, b) {
        (Some(a), Some(b)) if a < b => Ok(serde_json::from_str(&text[a..=b])
            .map_err(|e| format!("judge verdict is not valid ({e}): {}", clip(text, 200)))?),
        _ => Err(format!("judge returned no JSON verdict: {}", clip(text, 200)).into()),
    }
}

/// Ask an LLM to judge a run against a rubric.
///
/// The judge sees the task, the trajectory, the final answer and (optionally)
/// the final screenshot, and answers `{"pass": bool, "reason": "..."}`.
///
/// # Errors
/// Provider failure or an unparseable verdict.
pub async fn judge(
    provider: &dyn ChatProvider,
    model: &str,
    rubric: &str,
    task_prompt: &str,
    trace: &TurnTrace,
    screenshot: Option<(Vec<u8>, String)>,
) -> Result<Verdict> {
    let body = format!(
        "TASK GIVEN TO THE AGENT:\n{task_prompt}\n\nRUBRIC:\n{rubric}\n\nTRAJECTORY:\n{}\n\nFINAL ANSWER:\n{}\n",
        clip(&trace.trajectory, 12_000),
        trace.final_text
    );
    let mut parts = vec![json!({"type": "text", "text": body})];
    if let Some((bytes, mime)) = screenshot {
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        parts.push(
            json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}}),
        );
    }
    let messages = vec![
        Message::text(
            MessageRole::System,
            "You are a strict evaluator of an automation agent. Judge only against the rubric, \
             using the trajectory and the final screenshot as evidence. Reply with exactly one \
             JSON object: {\"pass\": true|false, \"reason\": \"one sentence\"}.",
        ),
        Message {
            role: MessageRole::User,
            content: Value::Array(parts),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
    ];
    let mut req = ChatRequest::new(ModelRef(model.to_owned()), messages);
    req.temperature = Some(0.0);
    let mut stream = provider.stream(req).await?;
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk? {
            ChatChunk::TextDelta(t) => text.push_str(&t),
            ChatChunk::Done { .. } => break,
            _ => {}
        }
    }
    parse_verdict(&text)
}

async fn final_screenshot(ctx: &Ctx, surface: Surface) -> Option<(Vec<u8>, String)> {
    match surface {
        Surface::Browser => {
            let (bytes, ext) = ctx.browser.screenshot(false, None).await.ok()?;
            let mime = if ext == "png" {
                "image/png"
            } else {
                "image/jpeg"
            };
            Some((bytes, mime.to_owned()))
        }
        Surface::Desktop => {
            let img = ctx
                .desktop
                .as_ref()?
                .screenshot(Region::FullScreen, Quality::Medium)
                .await
                .ok()?;
            Some((img.data, img.mime_type))
        }
        Surface::None => None,
    }
}

#[allow(clippy::too_many_lines)]
async fn run_checks(task: &Task, ctx: &Ctx, trace: &TurnTrace, opts: &Tier2Options) -> Vec<String> {
    let mut failures = Vec::new();
    if trace.timed_out {
        failures.push("the task hit its wall-clock cap and was cancelled".into());
    }
    for check in &task.checks {
        let r: std::result::Result<(), String> = match check {
            Check::FileExists(p) => {
                let p = ctx.ex(p);
                if Path::new(&p).is_file() {
                    Ok(())
                } else {
                    Err(format!("file_exists: {p} is missing"))
                }
            }
            Check::DirExists(p) => {
                let p = ctx.ex(p);
                if Path::new(&p).is_dir() {
                    Ok(())
                } else {
                    Err(format!("dir_exists: {p} is missing"))
                }
            }
            Check::FileMissing(p) => {
                let p = ctx.ex(p);
                if Path::new(&p).exists() {
                    Err(format!("file_missing: {p} still exists"))
                } else {
                    Ok(())
                }
            }
            Check::FileContains { path, text } => {
                let p = ctx.ex(path);
                match std::fs::read_to_string(&p) {
                    Ok(c) if c.contains(&ctx.ex(text)) => Ok(()),
                    Ok(c) => Err(format!(
                        "file_contains: {p} lacks {text:?} (has {:?})",
                        clip(&c, 120)
                    )),
                    Err(e) => Err(format!("file_contains: {p}: {e}")),
                }
            }
            Check::FinalText(m) => {
                let mut v = Vec::new();
                evaluate_text("final_text", m, &trace.final_text, &mut v);
                if v.is_empty() {
                    Ok(())
                } else {
                    Err(v.join("; "))
                }
            }
            Check::BrowserUrlContains(s) => match ctx.browser.eval("location.href").await {
                Ok(u) if u.as_str().is_some_and(|u| u.contains(&ctx.ex(s))) => Ok(()),
                Ok(u) => Err(format!("browser_url_contains: url is {u}, lacks {s:?}")),
                Err(e) => Err(format!("browser_url_contains: {e}")),
            },
            Check::BrowserTextContains(s) => match browser_text(ctx).await {
                Ok(t) if t.contains(&ctx.ex(s)) => Ok(()),
                Ok(t) => Err(format!(
                    "browser_text_contains: page lacks {s:?} (starts {:?})",
                    clip(&t, 120)
                )),
                Err(e) => Err(format!("browser_text_contains: {e}")),
            },
            Check::DesktopWindow(t) => match &ctx.desktop {
                None => Err("desktop_window: no desktop backend".into()),
                Some(b) => match b.windows().await {
                    Ok(w) if w.iter().any(|w| any_of(&w.title, t)) => Ok(()),
                    Ok(_) => Err(format!("desktop_window: no window titled *{t}*")),
                    Err(e) => Err(format!("desktop_window: {e}")),
                },
            },
            Check::DesktopNode(n) => match &ctx.desktop {
                None => Err("desktop_node: no desktop backend".into()),
                Some(b) => desktop_check(
                    b,
                    &n.window_title_contains,
                    &n.name_contains,
                    n.role.as_deref(),
                )
                .await
                .map_err(|e| format!("desktop_node: {e}")),
            },
            Check::Judge(j) => {
                let shot = if j.screenshot {
                    final_screenshot(ctx, task.requires).await
                } else {
                    None
                };
                let (prov, model): (Arc<dyn ChatProvider>, String) = match &opts.source {
                    Source::Live {
                        provider,
                        judge_model,
                        ..
                    } => (Arc::clone(provider), judge_model.clone()),
                    Source::Fake => (
                        ScriptedProvider::text("{\"pass\": true, \"reason\": \"fake judge\"}"),
                        "fake/judge".into(),
                    ),
                };
                match judge(
                    &*prov,
                    &model,
                    &j.rubric,
                    &ctx.ex(&task.prompt),
                    trace,
                    shot,
                )
                .await
                {
                    Ok(v) if v.pass => Ok(()),
                    Ok(v) => Err(format!("judge: FAIL: {}", v.reason)),
                    Err(e) => Err(format!("judge: {e}")),
                }
            }
        };
        if let Err(m) = r {
            failures.push(m);
        }
    }
    failures
}

/// Run one task end to end (setup, turn, checks, cleanup).
///
/// # Errors
/// Harness failures that prevent even recording a result.
#[allow(clippy::too_many_lines)]
pub async fn run_task(task: &Task, opts: &Tier2Options) -> Result<TaskOutcome> {
    if !task.platform.holds_here() {
        let why = match task.platform {
            PlatformReq::Windows => "needs Windows",
            PlatformReq::Any => "n/a",
        };
        return Ok(TaskOutcome::Skipped(why.into()));
    }
    let desktop = if task.requires == Surface::Desktop {
        let d = desktop_backend();
        if d.is_none() {
            return Ok(TaskOutcome::Skipped(
                "no desktop backend on this platform".into(),
            ));
        }
        d
    } else {
        None
    };

    let workdir_guard = tempfile::Builder::new().prefix("bot-eval-").tempdir()?;
    let data_guard = tempfile::Builder::new()
        .prefix("bot-eval-data-")
        .tempdir()?;
    let browser = Arc::new(CdpDriver::new(BrowserSettings::isolated(data_guard.path())));
    let mut baseline = HashMap::new();
    for n in task.kill_new_names() {
        let pids = pids_of(&n).await;
        baseline.insert(n, pids);
    }
    let ctx = Ctx {
        workdir: workdir_guard.path().to_path_buf(),
        browser: Arc::clone(&browser),
        desktop: desktop.clone(),
        baseline,
        notepad_state: notepad_state_files(),
    };

    let (provider, model): (Arc<dyn ChatProvider>, String) = match &opts.source {
        Source::Live {
            provider, model, ..
        } => (Arc::clone(provider), model.clone()),
        Source::Fake => {
            let script: Vec<ScriptStep> = if task.fake_script.is_empty() {
                vec![ScriptStep {
                    text: Some("fake: done".into()),
                    ..ScriptStep::default()
                }]
            } else {
                task.fake_script
                    .iter()
                    .cloned()
                    .map(|mut st| {
                        st.text = st.text.map(|t| ctx.ex(&t));
                        for c in &mut st.tool_calls {
                            expand_value(&mut c.args, &ctx);
                        }
                        st
                    })
                    .collect()
            };
            (ScriptedProvider::repeating(script), "fake/m".into())
        }
    };

    let record = async {
        for step in &task.setup {
            if let Err(e) = run_step(step, &ctx).await {
                return RunRecord::setup_failure(
                    2,
                    &task.name,
                    &model,
                    format!("setup failed: {e}"),
                );
            }
        }
        let registry = ToolRegistry::new();
        let mut bc = BuiltinConfig::new(data_guard.path().to_path_buf(), Arc::new(DenyGate));
        bc.desktop.clone_from(&ctx.desktop);
        bc.browser = Some(Arc::clone(&ctx.browser) as Arc<dyn BrowserDriver>);
        let _ = register_builtin_tools(&registry, bc);
        let allow = toml::Value::Array(
            task.tool_allow()
                .into_iter()
                .map(toml::Value::String)
                .collect(),
        );
        let teardown = registry.clone();
        let outcome = run_turn(TurnConfig {
            model: model.clone(),
            persona: PERSONA.into(),
            profile_extra: format!("[tools]\nallow = {allow}\n"),
            policy: Policy::full(),
            guards: GuardOverrides {
                max_steps: task.max_steps.or(opts.max_steps),
                // A model call (with its provider retries) may not outlive the task.
                model_timeout_ms: Some(task.timeout_s.unwrap_or(180) * 1000),
                // One hung desktop/browser call must not eat the whole task.
                tool_timeout_ms: Some(TOOL_TIMEOUT_MS),
                ..GuardOverrides::default()
            },
            prices: opts.prices.clone(),
            provider,
            registry,
            prompt: ctx.ex(&task.prompt),
            // Nobody is watching: anything that still asks (never-list, money) is denied.
            reactions: vec![Reaction {
                on: Trigger::ApprovalNeeded,
                tool: None,
                act: Act::Deny,
                text: Some("no human is available in eval runs".into()),
                once: false,
                delay_ms: 0,
            }],
            timeout: Duration::from_secs(task.timeout_s.unwrap_or(180)),
            retry_attempts: 3,
            subagents: false,
            workflows: Vec::new(),
        })
        .await;
        // `script_run` keeps a handle to the registry, so registry -> tool ->
        // registry is a cycle: break it or the browser (and Chrome) would never drop.
        for s in teardown.list() {
            teardown.unregister(&s.name);
        }
        let trace = match outcome {
            Ok(t) => t,
            Err(e) => {
                return RunRecord::setup_failure(2, &task.name, &model, format!("harness: {e}"));
            }
        };
        if let Some(dir) = &opts.trajectory_dir {
            let _ = std::fs::create_dir_all(dir);
            let text = format!(
                "{}
--- final answer ---
{}
",
                trace.trajectory, trace.final_text
            );
            let _ = std::fs::write(dir.join(format!("{}.txt", task.name)), text);
        }
        let failures = run_checks(task, &ctx, &trace, opts).await;
        RunRecord::from_trace(2, &task.name, &model, &trace, failures)
    }
    .await;

    // Cleanup always runs.
    for step in &task.cleanup {
        if let Err(e) = run_step(step, &ctx).await {
            eprintln!("warning: cleanup of {} failed: {e}", task.name);
        }
    }
    drop(ctx);
    drop(browser);
    Ok(TaskOutcome::Ran(record))
}

/// Run every task of a directory whose name contains `filter`.
///
/// # Errors
/// Load failures or harness failures.
pub async fn run_dir(
    dir: &Path,
    filter: Option<&str>,
    opts: &Tier2Options,
    mut progress: impl FnMut(&str, &TaskOutcome),
) -> Result<Vec<RunRecord>> {
    let mut out = Vec::new();
    for t in load_dir(dir)? {
        if filter.is_some_and(|f| !t.name.contains(f)) {
            continue;
        }
        let o = run_task(&t, opts).await?;
        progress(&t.name, &o);
        if let TaskOutcome::Ran(r) = o {
            out.push(r);
        }
    }
    Ok(out)
}

//! Live test: record "open Notepad, type text" through the real Windows backend
//! tools, then replay the recorded workflow with a different text.
//!
//! Run with
//! `cargo test -p workflow-replay --test live -- --ignored --nocapture --test-threads=1`.
//! It opens and closes Notepad; close your own Notepad windows first.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use builtin_tools::{BuiltinConfig, GateRequest, ToolGate, register_builtin_tools};
use desktop_api::DesktopBackend;
use protocol::{RunId, UserId};
use serde_json::{Value, json};
use storage::RunStep;
use tool_registry::{ContentPart, ToolContext, ToolRegistry};
use uuid::Uuid;
use workflow_replay::{
    Handles, RecordOptions, Recorder, ReplayOptions, Replayer, Step, SuccessCheck,
};

struct AllowAll;

#[async_trait]
impl ToolGate for AllowAll {
    async fn authorize(&self, _req: &GateRequest) -> Result<(), String> {
        Ok(())
    }
}

fn is_notepad(title: &str) -> bool {
    let t = title.to_lowercase();
    t.contains("notepad") || t.contains("not defteri")
}

async fn call(reg: &ToolRegistry, name: &str, input: &Value) -> String {
    let out = reg
        .get(name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .call(&ToolContext::default(), input.clone())
        .await
        .unwrap_or_else(|e| panic!("{name} failed: {e}"));
    match &out.content[0] {
        ContentPart::Text { text } => text.clone(),
        other => format!("{other:?}"),
    }
}

fn step(idx: u32, name: &str, input: Value, output: Option<Value>) -> RunStep {
    RunStep {
        id: Uuid::now_v7(),
        run_id: RunId::new(),
        user_id: UserId::new(),
        idx,
        name: name.into(),
        input,
        idempotent: false,
        retry: None,
        status: "succeeded".into(),
        attempt: 1,
        output,
        error: None,
        started_at_ms: 0,
        finished_at_ms: Some(0),
    }
}

/// What the agent loop would have persisted for one tool call.
fn persisted(idx: u32, name: &str, args: &Value, text: &str) -> RunStep {
    step(
        idx,
        "tools",
        json!({"calls": [{"id": "c", "name": name, "arguments": args.to_string(), "repeat": 1}]}),
        Some(
            json!({"results": [{"id": "c", "call_uuid": Uuid::now_v7(), "name": name,
            "is_error": false, "text": text, "spill_path": null}]}),
        ),
    )
}

async fn notepad_windows(backend: &Arc<dyn DesktopBackend>) -> Vec<u64> {
    backend
        .windows()
        .await
        .unwrap()
        .into_iter()
        .filter(|w| is_notepad(&w.title))
        .map(|w| w.id)
        .collect()
}

async fn close_notepad(reg: &ToolRegistry, backend: &Arc<dyn DesktopBackend>) {
    let key_tool = reg.get("desktop_key").expect("desktop_key missing");
    for id in notepad_windows(backend).await {
        let _ = call(reg, "desktop_get_state", &json!({"window_id": id})).await;
        for keys in [
            json!(["ctrl", "a"]),
            json!(["Delete"]),
            json!(["alt", "F4"]),
        ] {
            // `alt+F4` closes the window before the post-action refresh snapshot,
            // so the final `window not found` error is expected.
            let _ = key_tool
                .call(&ToolContext::default(), json!({"keys": keys}))
                .await;
        }
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
}

/// Backs up and clears Store Notepad's `TabState` / `WindowState` so the test
/// starts with a clean untitled window and restores the user's open tabs on drop.
struct NotepadSessionGuard {
    dirs: Vec<PathBuf>,
    backup: PathBuf,
}

impl NotepadSessionGuard {
    fn new(backup: PathBuf) -> Self {
        let dirs = notepad_state_dirs();
        for (i, dir) in dirs.iter().enumerate() {
            let dst = backup.join(i.to_string());
            let _ = std::fs::create_dir_all(&dst);
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && let Some(name) = path.file_name()
                    {
                        let _ = std::fs::copy(&path, dst.join(name));
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        Self { dirs, backup }
    }

    fn clear_current(&self) {
        for dir in &self.dirs {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

impl Drop for NotepadSessionGuard {
    fn drop(&mut self) {
        self.clear_current();
        for (i, dir) in self.dirs.iter().enumerate() {
            let src = self.backup.join(i.to_string());
            if let Ok(rd) = std::fs::read_dir(&src) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && let Some(name) = path.file_name()
                    {
                        let _ = std::fs::copy(&path, dir.join(name));
                    }
                }
            }
        }
    }
}

fn notepad_state_dirs() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(Path::new(&local).join("Packages")) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("Microsoft.WindowsNotepad_")
        })
        .flat_map(|e| ["TabState", "WindowState"].map(|d| e.path().join("LocalState").join(d)))
        .collect()
}

fn parse_gen(state: &str) -> u64 {
    state
        .lines()
        .find_map(|l| {
            l.strip_prefix("# window ")?
                .split_whitespace()
                .find_map(|tok| tok.strip_prefix("gen=")?.parse().ok())
        })
        .expect("missing gen= in state header")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a desktop session; opens and closes Notepad"]
async fn record_open_notepad_type_text_then_replay() {
    let backend: Arc<dyn DesktopBackend> =
        Arc::new(desktop_windows::WindowsBackend::new().unwrap());
    let data = tempfile::tempdir().unwrap();
    let mut cfg = BuiltinConfig::new(data.path().to_path_buf(), Arc::new(AllowAll));
    cfg.desktop = Some(backend.clone());
    let reg = ToolRegistry::new();
    let _ = register_builtin_tools(&reg, cfg);

    assert!(
        notepad_windows(&backend).await.is_empty(),
        "close your Notepad windows before running this test"
    );
    let session_guard = NotepadSessionGuard::new(data.path().join("np-session-backup"));

    // ── Phase 1: the "agent run" (real tools, real screen) ─────────────────
    let first = "praxi-live-first";
    let mut steps = vec![step(
        0,
        "user_message",
        json!({"text": format!("open notepad and type {first}")}),
        None,
    )];
    let launch_args = json!({"executable": "notepad.exe"});
    let launched = call(&reg, "desktop_launch", &launch_args).await;
    steps.push(persisted(1, "desktop_launch", &launch_args, &launched));

    let deadline = Instant::now() + Duration::from_secs(15);
    let id = loop {
        if let Some(id) = notepad_windows(&backend).await.first() {
            break *id;
        }
        assert!(Instant::now() < deadline, "Notepad did not appear");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    let state_args = json!({"window_id": id});
    let state = call(&reg, "desktop_get_state", &state_args).await;
    println!("{state}");
    steps.push(persisted(2, "desktop_get_state", &state_args, &state));
    let editor = state
        .lines()
        .find(|l| l.starts_with('[') && (l.contains("] edit ") || l.contains("] document ")))
        .unwrap_or_else(|| panic!("no editor in state:\n{state}"));
    let index: u32 = editor[1..editor.find(']').unwrap()].parse().unwrap();
    let generation = parse_gen(&state);
    let type_args = json!({"index": index, "gen": generation, "text": first});
    let after = call(&reg, "desktop_type", &type_args).await;
    steps.push(persisted(3, "desktop_type", &type_args, &after));
    assert!(after.contains(first), "typing failed:\n{after}");

    close_notepad(&reg, &backend).await;
    session_guard.clear_current();
    assert!(
        notepad_windows(&backend).await.is_empty(),
        "Notepad is still open"
    );

    // ── Phase 2: record, review, replay with another text ──────────────────
    let rec = Recorder::new()
        .from_steps(
            &steps,
            &RecordOptions {
                name: Some("notepad_note".into()),
                description: Some("Open Notepad and type a note".into()),
            },
        )
        .unwrap();
    println!("{}", rec.workflow.to_toml().unwrap());
    assert!(rec.dropped.is_empty(), "{:?}", rec.dropped);
    let mut wf = rec.workflow;
    assert_eq!(wf.params.len(), 1, "text should be a parameter");
    let pattern = wf
        .steps
        .iter()
        .find_map(|s| match s {
            Step::DesktopAction(d) => d.window_title_pattern.clone(),
            _ => None,
        })
        .unwrap();
    wf.success_check = Some(SuccessCheck::DesktopText {
        window_title_pattern: Some(pattern),
        contains: "{text}".into(),
    });

    let second = "praxi-live-second";
    let opts = ReplayOptions {
        resolve_timeout: Duration::from_secs(10),
        ..ReplayOptions::default()
    };
    let handles = Handles {
        desktop: Some(backend.clone()),
        browser: None,
        tools: reg.clone(),
    };
    let report = Replayer::new(handles, opts)
        .replay(&wf, json!({"text": second}).as_object().unwrap(), &AllowAll)
        .await
        .unwrap();
    println!("{report:#?}");

    close_notepad(&reg, &backend).await;
    assert!(
        report.is_completed(),
        "replay did not complete: {report:#?}"
    );
    assert_eq!(report.llm_calls, 0);
}

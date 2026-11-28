//! Offline adapters: real bundle installation and desktop tools, fake OS only.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use desktop_api::{
    ActGuard, Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, InputProbe,
    InputSink, InputTarget, ProcessIdentity, Quality, Refusal, Region, Snapshot, TreeQuality,
};
use tool_registry::ToolRegistry;

use crate::fake::FakeTool;
use crate::scenario::{DesktopFixture, FakeToolSpec, Scenario};

pub(crate) async fn peer_cannot_clear_limits(
    lp: &agent_core::AgentLoop,
    storage: &Arc<storage::Storage>,
    bot: protocol::BotId,
) -> crate::Result<()> {
    use tower::ServiceExt;
    let before = storage.get_bot_spend_limits(bot)?;
    let state = daemon_api::AppState::new(
        lp.clone(),
        Arc::clone(storage),
        daemon_api::DaemonConfig::new("eval-token"),
    )?;
    state.set_owner_key("eval-owner-key".into());
    let app = daemon_api::peer_router(state).layer(axum::Extension(daemon_api::PeerIdentity {
        key: "synthetic-peer".into(),
        role: daemon_api::PeerRole::Desktop,
    }));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(format!("/v1/bots/{bot}/spend-limits"))
                .header("content-type", "application/json")
                .header("x-praxi-owner-key", "eval-owner-key")
                .body(axum::body::Body::from("{}"))?,
        )
        .await?;
    if response.status() != axum::http::StatusCode::FORBIDDEN
        || storage.get_bot_spend_limits(bot)? != before
    {
        return Err("peer changed spending limits or was not refused".into());
    }
    Ok(())
}

pub(crate) fn register(
    s: &Scenario,
    registry: &ToolRegistry,
    data_dir: &Path,
    counters: &mut BTreeMap<String, Arc<AtomicU64>>,
) -> crate::Result<()> {
    if let Some(detail) = &s.diagnostic_report {
        let output = support_report(s, detail)?;
        let spec: FakeToolSpec = serde_json::from_value(serde_json::json!({
            "name": "diagnostic_read", "effect": "read", "output": output,
        }))?;
        let (tool, runs) = FakeTool::from_spec(&spec);
        registry.register(tool);
        counters.insert(spec.name, runs);
    }
    if let Some(bundle) = &s.bundle {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", opts)?;
        zip.write_all(&serde_json::to_vec(&bundle.manifest)?)?;
        zip.start_file("server/index.js", opts)?;
        zip.write_all(b"// fixture: never executed")?;
        let bytes = zip.finish()?.into_inner();
        let card = mcp_bridge::mcpb::inspect(&bytes)?;
        let store = secret_store::MemoryStore::new();
        let config = mcp_bridge::mcpb::install(
            &bytes,
            &card.sha256,
            &card.launch_sha256,
            &bundle.values,
            data_dir,
            &store,
        )?;
        let spec: FakeToolSpec = serde_json::from_value(serde_json::json!({
            "name": "bundle_config", "effect": "read",
            "output": serde_json::to_string(&config)?,
        }))?;
        let (tool, runs) = FakeTool::from_spec(&spec);
        registry.register(tool);
        counters.insert(spec.name, runs);
    }
    if let Some(fixture) = &s.desktop {
        let sends = Arc::new(AtomicU64::new(0));
        let backend = Arc::new(OfflineDesktop {
            facts: fixture.clone(),
            sends: Arc::clone(&sends),
        });
        let gate = fixture
            .human_driving
            .then(|| Arc::new(HumanDrives) as Arc<dyn builtin_tools::DesktopControlGate>);
        for tool in builtin_tools::desktop::tools(
            backend,
            None,
            data_dir,
            builtin_tools::desktop::DesktopSettings {
                action_settle: std::time::Duration::ZERO,
                launch_settle: std::time::Duration::ZERO,
            },
            gate,
        ) {
            registry.register(tool);
        }
        counters.insert("desktop_input".into(), sends);
    }
    Ok(())
}

fn support_report(s: &Scenario, detail: &str) -> crate::Result<String> {
    use daemon_api::doctor::{Check, ReportInfo, Scrub, redact_report_text, report_markdown};
    let mut redactor = secret_store::Redactor::new();
    for secret in &s.secrets {
        redactor.add_secret(secret);
    }
    let mask = |text: &str| redactor.redact(text);
    let scrub = Scrub {
        secrets: Some(&mask),
        home: Some("C:/fixture/home"),
        ..Scrub::default()
    };
    let info = ReportInfo {
        version: "fixture".into(),
        api_version: "1".into(),
        platform: "fixture".into(),
        data_dir: "C:/fixture/home/data".into(),
        config_files: vec!["mcp.toml".into()],
    };
    let report = report_markdown(
        &info,
        &[Check::ok("fixture", "Synthetic diagnostic", detail)],
        &scrub,
    );
    let report = redact_report_text(&report, &scrub);
    // Check before the agent loop's independent redactor could conceal a
    // report-scrubber regression.
    if s.secrets
        .iter()
        .any(|secret| report.contains(secret.as_str()))
        || report.contains("C:/fixture/home")
    {
        return Err("diagnostic report leaked a fixture canary".into());
    }
    Ok(report)
}

struct HumanDrives;

#[async_trait]
impl builtin_tools::DesktopControlGate for HumanDrives {
    fn observation_refused(&self) -> Option<String> {
        Some("the human is driving; observation paused".into())
    }
    async fn before_action(&self, _: &builtin_tools::ControlRequest) -> Result<(), String> {
        Err("the human is driving; action refused".into())
    }
}

struct OfflineDesktop {
    facts: DesktopFixture,
    sends: Arc<AtomicU64>,
}

impl OfflineDesktop {
    fn target(&self) -> InputTarget {
        InputTarget {
            window_id: 1,
            title: "Fixture editor".into(),
            class: "Notepad".into(),
            identity: ProcessIdentity {
                image_path: "C:/fixture/notepad.exe".into(),
                pid: 42,
                start_time: 123,
            },
            elevated: Some(false),
            password_focus: self.facts.password_focus,
            owned: false,
        }
    }
}

#[async_trait]
impl DesktopBackend for OfflineDesktop {
    async fn snapshot(&self, _: Option<u64>) -> Result<Snapshot, DesktopError> {
        Ok(Snapshot {
            generation: 1,
            window: self.windows().await?.remove(0),
            nodes: vec![],
            quality: TreeQuality::Weak,
        })
    }
    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
        // Individual approval still uses the backend's live receiver checks,
        // just like the OS backend; a task grant is not needed for this guard.
        if matches!(action, Action::TypeText { .. } | Action::KeyPress { .. }) {
            return self
                .act_guarded(
                    action,
                    ActGuard {
                        expect: self.target().identity,
                    },
                )
                .await;
        }
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(ActionResult {
            dispatched: true,
            verified: None,
            new_snapshot: None,
        })
    }
    async fn act_guarded(
        &self,
        action: Action,
        guard: ActGuard,
    ) -> Result<ActionResult, DesktopError> {
        let mut probe = Probe(self.target());
        let mut sink = Sink(Arc::clone(&self.sends));
        let result = match action {
            Action::TypeText { text } => {
                desktop_api::guarded::guarded_type(&guard, &text, &mut probe, &mut sink)
            }
            Action::KeyPress { keys } => {
                desktop_api::guarded::guarded_key(&guard, &keys, &mut probe, &mut sink)
            }
            _ => return Err(DesktopError::Unsupported("fixture action".into())),
        };
        result.map_err(|e| DesktopError::GuardRefused(e.message()))?;
        Ok(ActionResult {
            dispatched: true,
            verified: None,
            new_snapshot: None,
        })
    }
    async fn screenshot(&self, _: Region, _: Quality) -> Result<Image, DesktopError> {
        Err(DesktopError::Unsupported("fixture has no pixels".into()))
    }
    async fn windows(&self) -> Result<Vec<desktop_api::WindowInfo>, DesktopError> {
        Ok(vec![desktop_api::WindowInfo {
            id: 1,
            title: "Fixture editor".into(),
            pid: 42,
            focused: true,
            bounds: desktop_api::snapshot::Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
        }])
    }
    async fn launch(&self, _: desktop_api::snapshot::AppSpec) -> Result<u32, DesktopError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(42)
    }
    async fn input_target(&self) -> Result<Option<InputTarget>, DesktopError> {
        Ok(Some(self.target()))
    }
    fn key_chord_refusal(&self, keys: &[String]) -> Option<Refusal> {
        (!desktop_api::guarded::chord_allowed(keys)).then_some(Refusal::Chord)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: "windows".into(),
            accessibility_tree: true,
            screenshot: false,
            app_launch: true,
            has_display: true,
        }
    }
}

struct Probe(InputTarget);
impl InputProbe for Probe {
    fn keyboard_receivers(&mut self) -> Result<Vec<InputTarget>, Refusal> {
        Ok(vec![self.0.clone()])
    }
    fn point_receivers(&mut self, _: (f64, f64)) -> Result<Vec<InputTarget>, Refusal> {
        Ok(vec![self.0.clone()])
    }
    fn modifiers_down(&mut self) -> Result<bool, Refusal> {
        Ok(false)
    }
}

struct Sink(Arc<AtomicU64>);
impl InputSink for Sink {
    fn type_units(&mut self, _: &[u16]) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn key_chord(&mut self, _: &[String]) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn click(&mut self, _: (f64, f64)) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn release_all(&mut self) -> bool {
        true
    }
}

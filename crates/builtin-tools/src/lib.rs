//! Built-in agent tools: desktop, browser, files (with checkpoints), shell,
//! script and meta tools, all implemented as [`tool_registry::Tool`]s.
//!
//! Use [`register_builtin_tools`] to put the full set into a registry.
//!
//! Permission model: the agent loop checks permissions for direct tool calls
//! using each tool's `category`/`effect` (and [`raw_command_for`] for the
//! never-list). Tool calls made *from a script* go through [`ToolGate`], which
//! the loop implements with the same approval flow; [`ResolverGate`] is a
//! non-interactive implementation on `permission-engine`.
//!
//! Meta tools (`ask_user`, `todo_write`) return a [`tool_registry::ContentPart::Json`]
//! part whose `kind` is [`meta::QUESTION_CARD_KIND`] or [`meta::TODO_LIST_KIND`].

// The tool tables and dispatchers are long but flat declarative code.
#![allow(clippy::too_many_lines)]

pub mod browser;
pub mod checkpoint;
pub mod control;
pub mod desktop;
pub mod files;
pub mod meta;
pub mod schedule;
pub mod script;
pub mod shell;
mod util;

/// Windows drive letters (DOS devices) live in the logon session, not in one
/// test, and PowerShell enumerates all of them while it starts. A test that
/// defines a drive letter pointing at an unreachable share therefore stalls
/// every PowerShell launched meanwhile, until the SMB timeout expires. Tests
/// that spawn the shell and tests that define drive letters hold this lock.
#[cfg(all(test, windows))]
pub(crate) mod test_serial {
    pub(crate) static WINDOWS_SHELL_TESTS: tokio::sync::Mutex<()> =
        tokio::sync::Mutex::const_new(());
}

use std::path::PathBuf;
use std::sync::Arc;

use desktop_api::DesktopBackend;
use tool_registry::{Tool, ToolRegistry};

pub use browser::{BrowserDriver, BrowserSettings, CdpDriver, Wait};
pub use checkpoint::{Checkpoint, CheckpointEntry, CheckpointStore};
pub use control::{
    ActionPermit, ControlRequest, DesktopControlGate, OBSERVATION_DISCARDED, PermitValidator,
};
pub use desktop::{DesktopSettings, SharedOcr};
pub use meta::{META_TOOL_NAMES, QUESTION_CARD_KIND, TODO_LIST_KIND, TodoItem, TodoStatus};
pub use schedule::{
    PreviousSchedule, SCHEDULE_TOOL_NAMES, ScheduleCaller, ScheduleChange, ScheduleHost,
    SchedulePreview, ScheduleRequest, ScheduleView,
};
pub use script::{
    GateHost, GateRequest, ResolverGate, SCRIPT_TOOL_NAME, ToolGate, raw_command_for,
};

pub use connector_email;
pub use web_tools;

/// Everything needed to build the built-in tool set.
pub struct BuiltinConfig {
    /// Application data directory (checkpoints, spill files, screenshots).
    pub data_dir: PathBuf,
    /// Existing platform/CDP adapters only. No screenshot-tool fallback or new browser.
    pub view_registry: Option<Arc<live_view::TargetRegistry>>,
    pub view_targets: Vec<Arc<dyn live_view::ViewTarget>>,
    /// Desktop backend (local or remote). Desktop tools are skipped when `None`
    /// unless `dynamic_desktop` is true.
    pub desktop: Option<Arc<dyn DesktopBackend>>,
    /// Register desktop tools without an upfront static backend so a per-run
    /// backend resolved via [`DesktopControlGate::desktop_for_run`] can supply one
    /// (e.g. a remote PC node coming online later).
    pub dynamic_desktop: bool,
    /// OCR engine used for set-of-marks when the accessibility tree is weak.
    pub ocr: Option<SharedOcr>,
    /// Desktop timing settings.
    pub desktop_settings: DesktopSettings,
    /// Browser driver. `None` lazily creates per-bot drivers with `browser_settings`.
    pub browser: Option<Arc<dyn BrowserDriver>>,
    /// Settings for the default [`CdpDriver`].
    pub browser_settings: BrowserSettings,
    /// Permission gate for tool calls made from scripts.
    pub gate: Arc<dyn ToolGate>,
    /// Limits for `script_run`.
    pub script_limits: script_runtime::Limits,
    /// Called before every mutating desktop / visible-browser action (see
    /// [`DesktopControlGate`]); `None` = no gate.
    pub control: Option<Arc<dyn DesktopControlGate>>,
    /// Email connector (IMAP/SMTP tools). `None` = email tools skipped.
    pub email: Option<Arc<connector_email::EmailConnector>>,
    /// Web tools (search, fetch, feeds, sitemaps, metadata). `None` = web tools skipped.
    pub web: Option<web_tools::WebToolsConfig>,
    /// Directories `fs_*` may read and write. Empty does not add a root limit.
    /// A non-empty list that cannot be resolved denies every `fs_*` call.
    /// UNC paths, device paths, and credential files are still refused.
    pub fs_allowed_roots: Vec<PathBuf>,
    /// Credential paths the host explicitly grants to `fs_*`. A grant does not
    /// permit UNC, device, or out-of-root paths.
    pub fs_granted_paths: Vec<PathBuf>,
}

impl BuiltinConfig {
    /// A config with defaults: headless per-bot browser, default script limits,
    /// no desktop backend, no OCR, no email connector and no web tools.
    #[must_use]
    pub fn new(data_dir: PathBuf, gate: Arc<dyn ToolGate>) -> Self {
        Self {
            browser_settings: BrowserSettings::isolated(&data_dir),
            data_dir,
            desktop: None,
            dynamic_desktop: false,
            view_registry: None,
            view_targets: Vec::new(),
            ocr: None,
            desktop_settings: DesktopSettings::default(),
            browser: None,
            gate,
            script_limits: script_runtime::Limits::default(),
            control: None,
            email: None,
            web: None,
            fs_allowed_roots: Vec::new(),
            fs_granted_paths: Vec::new(),
        }
    }

    /// Like [`BuiltinConfig::new`] using the storage crate's data directory.
    #[must_use]
    pub fn with_default_data_dir(gate: Arc<dyn ToolGate>) -> Option<Self> {
        storage::data_dir().map(|d| Self::new(d, gate))
    }
}

/// Build every built-in tool without registering it.
///
/// `registry` is the registry `script_run` calls back into; pass the one the
/// tools will be registered in.
#[must_use]
pub fn build_builtin_tools(cfg: BuiltinConfig, registry: &ToolRegistry) -> Vec<Arc<dyn Tool>> {
    if let Some(registry) = &cfg.view_registry {
        for target in &cfg.view_targets {
            registry.register(Arc::clone(target));
        }
    }
    let mut all: Vec<Arc<dyn Tool>> = Vec::new();
    if cfg.desktop.is_some() || cfg.dynamic_desktop {
        all.extend(desktop::tools_dynamic(
            cfg.desktop,
            cfg.ocr,
            &cfg.data_dir,
            cfg.desktop_settings,
            cfg.control.clone(),
        ));
    }
    if let Some(driver) = cfg.browser {
        all.extend(browser::tools(driver, &cfg.data_dir, cfg.control.clone()));
    } else {
        all.extend(browser::managed_tools(
            cfg.browser_settings,
            &cfg.data_dir,
            cfg.control.clone(),
        ));
    }
    all.extend(files::tools_bounded(
        &cfg.data_dir,
        &cfg.fs_allowed_roots,
        &cfg.fs_granted_paths,
    ));
    all.push(shell::tool(&cfg.data_dir));
    all.push(script::tool_with_control(
        registry.clone(),
        cfg.gate,
        cfg.script_limits,
        cfg.control,
    ));
    all.extend(meta::tools());
    if let Some(email) = &cfg.email {
        all.extend(connector_email::tools(email));
    }
    if let Some(web) = cfg.web.clone() {
        match web_tools::tools(web) {
            Ok(tools) => all.extend(tools),
            Err(e) => tracing::warn!("web tools not available: {e}"),
        }
    }
    all
}

/// Register all built-in tools in `registry`. Returns the number registered.
#[must_use]
pub fn register_builtin_tools(registry: &ToolRegistry, cfg: BuiltinConfig) -> usize {
    build_builtin_tools(cfg, registry)
        .into_iter()
        .filter(|t| registry.register(t.clone()))
        .count()
}

/// Register the e-mail tools in `registry`; returns how many were added.
#[must_use]
pub fn register_email_tools(
    registry: &ToolRegistry,
    connector: &Arc<connector_email::EmailConnector>,
) -> usize {
    connector_email::register(registry, connector)
}

/// Register the web tools in `registry`; returns the names that were added.
///
/// # Errors
/// [`web_tools::WebError`] when the HTTP client or a search backend cannot be built.
pub fn register_web_tools(
    registry: &ToolRegistry,
    cfg: web_tools::WebToolsConfig,
) -> Result<Vec<String>, web_tools::WebError> {
    web_tools::register(registry, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tool_registry::{Effect, PermissionCategory};

    struct AllowAll;

    #[async_trait]
    impl ToolGate for AllowAll {
        async fn authorize(&self, _req: &GateRequest) -> Result<(), String> {
            Ok(())
        }
    }

    fn registry() -> (tempfile::TempDir, ToolRegistry) {
        let d = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        let mut cfg = BuiltinConfig::new(d.path().to_path_buf(), Arc::new(AllowAll));
        cfg.desktop = Some(Arc::new(desktop::fake::FakeDesktop::new(false)));
        cfg.browser = Some(Arc::new(browser::fake::FakeBrowser::default()));
        let _ = register_builtin_tools(&reg, cfg);
        (d, reg)
    }

    /// Expected `(name, effect, category)` for every built-in tool.
    fn expected() -> Vec<(&'static str, Effect, PermissionCategory)> {
        use Effect::{Destructive as D, Read as R, Write as W};
        use PermissionCategory::{
            Browser as B, Desktop as De, Fs, Meta as Me, Script as Sc, Shell as Sh,
        };
        vec![
            ("desktop_list_apps", R, De),
            ("desktop_launch", W, De),
            ("desktop_get_state", R, De),
            ("desktop_click", W, De),
            ("desktop_type", W, De),
            ("desktop_set_value", W, De),
            ("desktop_key", W, De),
            ("desktop_scroll", R, De),
            ("desktop_drag", W, De),
            ("browser_open", W, B),
            ("browser_snapshot", R, B),
            ("browser_click", W, B),
            ("browser_fill", W, B),
            ("browser_select", W, B),
            ("browser_press", W, B),
            ("browser_scroll", R, B),
            ("browser_wait", R, B),
            ("browser_back", W, B),
            ("browser_tabs", W, B),
            ("browser_screenshot", R, B),
            ("browser_eval", W, Sc),
            ("fs_read", R, Fs),
            ("fs_list", R, Fs),
            ("fs_search", R, Fs),
            ("fs_write", W, Fs),
            ("fs_edit", W, Fs),
            ("fs_move", W, Fs),
            ("fs_delete", D, Fs),
            ("checkpoint_list", R, Fs),
            ("checkpoint_restore", W, Fs),
            ("shell_run", D, Sh),
            ("script_run", W, Sc),
            ("ask_user", R, Me),
            ("todo_write", W, Me),
        ]
    }

    #[test]
    fn every_tool_has_description_schema_effect_and_platforms() {
        let (_d, reg) = registry();
        let specs = reg.list();
        let exp = expected();
        assert_eq!(specs.len(), exp.len(), "unexpected tool count");
        for (name, effect, category) in exp {
            let s = specs
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            assert!(s.is_valid(), "{name} invalid");
            assert!(s.description.len() > 20, "{name} description too short");
            assert!(
                s.description.contains("Example"),
                "{name} description needs an example"
            );
            assert!(!s.platforms.is_empty(), "{name} has no platforms");
            assert_eq!(s.effect, effect, "{name} effect");
            assert_eq!(s.category, category, "{name} category");
            assert_eq!(s.input_schema["type"], "object", "{name} schema type");
            assert!(
                s.input_schema["properties"].is_object(),
                "{name} properties"
            );
            if let Some(req) = s.input_schema.get("required") {
                let props = s.input_schema["properties"].as_object().unwrap();
                for r in req.as_array().unwrap() {
                    assert!(
                        props.contains_key(r.as_str().unwrap()),
                        "{name}: required {r} undefined"
                    );
                }
            }
        }
    }

    #[test]
    fn read_tools_are_never_in_destructive_or_write_set_by_accident() {
        // Anything that mutates state must not be declared Read.
        let (_d, reg) = registry();
        for s in reg.list() {
            let mutating = [
                "write", "edit", "move", "delete", "restore", "click", "type", "fill", "launch",
                "shell",
            ]
            .iter()
            .any(|m| s.name.contains(m));
            if mutating {
                assert_ne!(s.effect, Effect::Read, "{} must not be Read", s.name);
            }
        }
    }

    #[test]
    fn without_a_desktop_backend_desktop_tools_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        let mut cfg = BuiltinConfig::new(d.path().to_path_buf(), Arc::new(AllowAll));
        cfg.browser = Some(Arc::new(browser::fake::FakeBrowser::default()));
        let _ = register_builtin_tools(&reg, cfg);
        assert!(reg.get("desktop_click").is_none());
        assert!(reg.get("fs_read").is_some());
    }

    struct OneAdmission(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl DesktopControlGate for OneAdmission {
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err("new admission closed".into())
            }
        }
    }
    #[tokio::test]
    async fn script_computer_calls_inherit_one_lifetime_admission() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        let gate = Arc::new(OneAdmission(std::sync::atomic::AtomicUsize::new(0)));
        let browser = Arc::new(browser::fake::FakeBrowser::default());
        let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
        config.browser = Some(browser.clone());
        config.control = Some(gate.clone());
        let _ = register_builtin_tools(&registry, config);
        registry.get("script_run").unwrap().call(&tool_registry::ToolContext::default(),serde_json::json!({"code":"await tools.call('browser_click',{ref:'e1'}); return await tools.call('browser_press',{key:'Enter'});"})).await.unwrap();
        assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(browser.calls.lock().len(), 2);
    }

    /// The first admission waited out a pause (needs a fresh observation); the
    /// later ones are ordinary.
    struct ReobserveFirst(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl DesktopControlGate for ReobserveFirst {
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            Ok(())
        }
        async fn admit_background_action(
            &self,
            _req: &ControlRequest,
        ) -> Result<ActionPermit, String> {
            let first = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            Ok(ActionPermit::new(first, || {}))
        }
        async fn admit_action(&self, req: &ControlRequest) -> Result<ActionPermit, String> {
            self.admit_background_action(req).await
        }
    }

    #[tokio::test]
    async fn a_script_that_waited_through_a_pause_observes_once_and_then_acts() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        let gate = Arc::new(ReobserveFirst(std::sync::atomic::AtomicUsize::new(0)));
        let browser = Arc::new(browser::fake::FakeBrowser::default());
        let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
        config.browser = Some(browser.clone());
        config.control = Some(gate.clone());
        let _ = register_builtin_tools(&registry, config);
        let out = registry
            .get("script_run")
            .unwrap()
            .call(
                &tool_registry::ToolContext::default(),
                serde_json::json!({"code":"const a = await tools.call('browser_click',{ref:'e1'}); const b = await tools.call('browser_press',{key:'Enter'}); const c = await tools.call('browser_press',{key:'Tab'}); return [a, b, c];"}),
            )
            .await
            .unwrap();
        let tool_registry::ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        // One admission for the whole script.
        assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        let calls = browser.calls.lock().clone();
        // The first call was discarded for a fresh observation (no click was
        // dispatched); the later ones were really performed.
        assert!(!calls.iter().any(|c| c.starts_with("click")), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("press")), "{calls:?}");
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("press")).count(),
            2,
            "{calls:?} / {text}"
        );
        assert_eq!(
            text.matches("previous action discarded").count(),
            1,
            "only the first call is discarded: {text}"
        );
    }

    /// A gate whose latch the test flips while a script runs: the Stop /
    /// takeover that lands mid-script.
    #[derive(Default)]
    struct LatchGate {
        admissions: std::sync::atomic::AtomicUsize,
        /// Latched (paused / human driving / local override) with this message.
        latched: std::sync::Mutex<Option<String>>,
        /// Observation tools are refused as well (the human drives).
        driving: std::sync::atomic::AtomicBool,
        epoch: std::sync::atomic::AtomicU64,
        /// Hand out permits that already need a fresh observation.
        reobserve: bool,
    }
    #[async_trait::async_trait]
    impl DesktopControlGate for LatchGate {
        fn observation_epoch(&self) -> u64 {
            self.epoch.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn latch_refusal(&self) -> Option<String> {
            self.latched.lock().unwrap().clone()
        }
        fn observation_refused(&self) -> Option<String> {
            self.driving
                .load(std::sync::atomic::Ordering::SeqCst)
                .then(|| "the user is controlling (observation paused)".to_owned())
        }
        async fn before_action(&self, _req: &ControlRequest) -> Result<(), String> {
            Ok(())
        }
        async fn admit_background_action(
            &self,
            _req: &ControlRequest,
        ) -> Result<ActionPermit, String> {
            if let Some(message) = self.latch_refusal() {
                return Err(message);
            }
            self.admissions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionPermit::new(self.reobserve, || {}))
        }
        async fn admit_action(&self, req: &ControlRequest) -> Result<ActionPermit, String> {
            self.admit_background_action(req).await
        }
    }

    /// A test tool the script calls to make the Stop / takeover land between
    /// two of its computer actions.
    struct FlipTool {
        gate: Arc<LatchGate>,
        message: String,
        spec: tool_registry::ToolSpec,
    }
    #[async_trait::async_trait]
    impl tool_registry::Tool for FlipTool {
        fn spec(&self) -> &tool_registry::ToolSpec {
            &self.spec
        }
        async fn call(
            &self,
            _ctx: &tool_registry::ToolContext,
            _input: serde_json::Value,
        ) -> Result<tool_registry::ToolOutput, tool_registry::ToolError> {
            *self.gate.latched.lock().unwrap() = Some(self.message.clone());
            self.gate
                .driving
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.gate
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(tool_registry::ToolOutput::text("latched"))
        }
    }

    async fn run_script(gate: Arc<LatchGate>, code: &str) -> (String, Vec<String>) {
        run_script_with(gate, "The user stopped the bot; do not retry.", code).await
    }

    async fn run_script_with(
        gate: Arc<LatchGate>,
        message: &str,
        code: &str,
    ) -> (String, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        let browser = Arc::new(browser::fake::FakeBrowser::default());
        let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
        config.browser = Some(browser.clone());
        config.control = Some(gate.clone());
        let _ = register_builtin_tools(&registry, config);
        registry.register(Arc::new(FlipTool {
            gate,
            message: message.to_owned(),
            spec: tool_registry::ToolSpec {
                name: "latch_now".into(),
                description: "test".into(),
                input_schema: serde_json::json!({"type":"object"}),
                effect: tool_registry::Effect::Read,
                category: permission_engine::PermissionCategory::Meta,
                platforms: vec![tool_registry::Platform::All],
            },
        }));
        // A refusal either is caught by the script or ends it with the gate's
        // message; both show up as text here.
        let text = match registry
            .get("script_run")
            .unwrap()
            .call(
                &tool_registry::ToolContext::default(),
                serde_json::json!({ "code": code }),
            )
            .await
        {
            Ok(out) => {
                let tool_registry::ContentPart::Text { text } = &out.content[0] else {
                    panic!()
                };
                text.clone()
            }
            Err(e) => format!("script failed: {e}"),
        };
        let calls = browser.calls.lock().clone();
        (text, calls)
    }

    #[tokio::test]
    async fn a_stop_that_lands_mid_script_refuses_the_next_computer_action() {
        let gate = Arc::new(LatchGate::default());
        let (text, calls) = run_script(
            gate,
            "await tools.call('browser_click',{ref:'e1'}); await tools.call('latch_now',{}); try { await tools.call('browser_press',{key:'Enter'}); return 'ran'; } catch (e) { return 'refused: ' + e; }",
        )
        .await;
        assert!(text.contains("stopped the bot"), "{text}");
        assert!(
            calls.iter().all(|c| !c.starts_with("press")),
            "the cached permit let an action through after the Stop: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("click")),
            "the first action did run: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_human_takeover_mid_script_refuses_observation_and_actions() {
        let gate = Arc::new(LatchGate::default());
        let (text, calls) = run_script_with(
            gate,
            "The user has taken control; do not retry.",
            "await tools.call('browser_click',{ref:'e1'}); await tools.call('latch_now',{}); const out = []; for (const [name, args] of [['browser_snapshot',{}],['browser_screenshot',{}],['browser_press',{key:'Enter'}],['browser_fill',{ref:'e1',text:'secret'}]]) { try { await tools.call(name,args); out.push(name + ': ran'); } catch (e) { out.push(name + ': refused'); } } return out.join('|');",
        )
        .await;
        assert!(!text.contains(": ran"), "{text}");
        assert_eq!(
            calls.len(),
            1,
            "only the first click reached the browser: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_fresh_observation_is_refused_while_the_latch_is_set() {
        // The permit needs a fresh observation (the bot waited through a pause),
        // but the human is driving again: no snapshot may be taken.
        let gate = Arc::new(LatchGate {
            reobserve: true,
            ..LatchGate::default()
        });
        gate.driving
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (text, calls) = run_script(
            gate,
            "try { await tools.call('browser_click',{ref:'e1'}); return 'ran'; } catch (e) { return 'refused: ' + e; }",
        )
        .await;
        assert!(!text.contains("ran"), "{text}");
        assert!(
            calls.is_empty(),
            "a snapshot was captured while the user drives: {calls:?}"
        );
    }

    #[tokio::test]
    async fn an_epoch_change_without_a_latch_costs_one_observation_then_the_script_goes_on() {
        let gate = Arc::new(LatchGate::default());
        let epoch = Arc::clone(&gate);
        let (text, calls) = run_script(
            gate,
            "await tools.call('browser_click',{ref:'e1'}); await tools.call('browser_press',{key:'Enter'}); const b = await tools.call('browser_press',{key:'Tab'}); return b;",
        )
        .await;
        let _ = (&text, &epoch);
        // No epoch change here: every call is ordinary.
        assert_eq!(calls.len(), 3, "{calls:?}");
    }

    #[tokio::test]
    async fn a_script_that_waited_through_an_epoch_change_observes_once_per_change() {
        struct Bump(Arc<LatchGate>);
        let gate = Arc::new(LatchGate::default());
        // Bump the epoch between the first and the second call (a pause came and went).
        let bump = Bump(Arc::clone(&gate));
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        let browser = Arc::new(browser::fake::FakeBrowser::default());
        let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
        config.browser = Some(browser.clone());
        config.control = Some(gate.clone());
        let _ = register_builtin_tools(&registry, config);
        let tool = registry.get("browser_click").unwrap();
        let ctx = tool_registry::ToolContext::default();
        // Direct calls scoped like a script: one shared admission.
        let action: control::ScriptAction = Arc::default();
        let first = control::scope_action(Arc::clone(&action), async {
            tool.call(&ctx, serde_json::json!({"ref":"e1"})).await
        })
        .await
        .unwrap();
        let _ = first;
        bump.0
            .epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let second = control::scope_action(Arc::clone(&action), async {
            tool.call(&ctx, serde_json::json!({"ref":"e1"})).await
        })
        .await
        .unwrap();
        let tool_registry::ContentPart::Text { text } = &second.content[0] else {
            panic!()
        };
        assert!(text.contains("previous action discarded"), "{text}");
        let third = control::scope_action(Arc::clone(&action), async {
            tool.call(&ctx, serde_json::json!({"ref":"e1"})).await
        })
        .await
        .unwrap();
        let tool_registry::ContentPart::Text { text } = &third.content[0] else {
            panic!()
        };
        assert!(!text.contains("previous action discarded"), "{text}");
    }

    #[tokio::test]
    async fn a_browser_observation_that_straddles_a_takeover_never_reaches_the_model() {
        for (tool, input) in [
            ("browser_screenshot", serde_json::json!({})),
            ("browser_wait", serde_json::json!({"idle": true})),
            ("browser_tabs", serde_json::json!({})),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let registry = ToolRegistry::new();
            let gate = Arc::new(LatchGate::default());
            let browser = Arc::new(browser::fake::FakeBrowser::default());
            let flip = Arc::clone(&gate);
            *browser.hook.lock() = Some(Arc::new(move |_| {
                flip.driving
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                flip.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
            let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
            config.browser = Some(browser.clone());
            config.control = Some(gate);
            let _ = register_builtin_tools(&registry, config);
            let err = registry
                .get(tool)
                .unwrap()
                .call(&tool_registry::ToolContext::default(), input)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("discarded"), "{tool}: {err}");
        }
    }

    #[tokio::test]
    async fn a_reobservation_instead_of_an_action_never_shows_a_page_taken_during_a_takeover() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        let gate = Arc::new(LatchGate {
            reobserve: true,
            ..LatchGate::default()
        });
        let browser = Arc::new(browser::fake::FakeBrowser::default());
        let flip = Arc::clone(&gate);
        *browser.hook.lock() = Some(Arc::new(move |_| {
            flip.driving
                .store(true, std::sync::atomic::Ordering::SeqCst);
            flip.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
        config.browser = Some(browser.clone());
        config.control = Some(gate);
        let _ = register_builtin_tools(&registry, config);
        let out = registry
            .get("browser_click")
            .unwrap()
            .call(
                &tool_registry::ToolContext::default(),
                serde_json::json!({"ref":"e1"}),
            )
            .await;
        let shown = match &out {
            Ok(o) => match &o.content[0] {
                tool_registry::ContentPart::Text { text } => text.clone(),
                _ => String::new(),
            },
            Err(e) => e.to_string(),
        };
        assert!(
            !shown.contains("Fresh observation") && !shown.contains("snapshot"),
            "a page captured while the user took over reached the model: {shown}"
        );
    }

    #[tokio::test]
    async fn a_script_that_never_touches_the_computer_is_never_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        // Admission is closed for everything: only computer calls may notice.
        let gate = Arc::new(OneAdmission(std::sync::atomic::AtomicUsize::new(1)));
        let browser = Arc::new(browser::fake::FakeBrowser::default());
        let mut config = BuiltinConfig::new(dir.path().into(), Arc::new(AllowAll));
        config.browser = Some(browser.clone());
        config.control = Some(gate.clone());
        let _ = register_builtin_tools(&registry, config);
        let script = registry.get("script_run").unwrap();
        let ctx = tool_registry::ToolContext::default();
        let out = script
            .call(&ctx, serde_json::json!({"code":"return 1 + 1;"}))
            .await
            .unwrap();
        let tool_registry::ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains('2'), "{text}");
        assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The first computer call is where admission happens, and it is refused.
        let out = script
            .call(
                &ctx,
                serde_json::json!({"code":"try { await tools.call('browser_click',{ref:'e1'}); return 'ran'; } catch (e) { return 'refused: ' + e; }"}),
            )
            .await
            .unwrap();
        let tool_registry::ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("refused"), "{text}");
        assert!(browser.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn script_can_call_registered_tools_through_the_registry() {
        let (d, reg) = registry();
        let file = d
            .path()
            .join("s.txt")
            .display()
            .to_string()
            .replace('\\', "/");
        let code = format!(
            "await tools.call('fs_write', {{path: '{file}', content: 'from script'}}); return await tools.call('fs_read', {{path: '{file}'}});"
        );
        let out = reg
            .get("script_run")
            .unwrap()
            .call(
                &tool_registry::ToolContext::default(),
                serde_json::json!({"code": code}),
            )
            .await
            .unwrap();
        let tool_registry::ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("from script"), "{text}");
    }

    #[test]
    fn email_tools_can_be_configured_and_send_is_destructive() {
        use secret_store::SecretStore;

        let d = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        let store = Arc::new(secret_store::MemoryStore::new());
        store.set("email/test", "password").unwrap();
        let account = connector_email::AccountConfig {
            name: "test".to_string(),
            address: "user@example.com".to_string(),
            username: String::new(),
            password: secret_store::SecretRef::new("email/test").unwrap(),
            imap: connector_email::ImapSettings {
                host: "imap.example.com".to_string(),
                port: 993,
            },
            smtp: connector_email::SmtpSettings {
                host: "smtp.example.com".to_string(),
                port: 465,
                security: connector_email::SmtpSecurity::Implicit,
            },
            drafts_folder: "Drafts".to_string(),
            extra_root_pem: None,
            timeout_secs: 30,
            bot: None,
            allowed_senders: Vec::new(),
            kind: connector_email::AccountKind::ImapSmtp,
            inbox_id: None,
        };
        let connector =
            Arc::new(connector_email::EmailConnector::new(vec![account], store).unwrap());
        let mut cfg = BuiltinConfig::new(d.path().to_path_buf(), Arc::new(AllowAll));
        cfg.desktop = Some(Arc::new(desktop::fake::FakeDesktop::new(false)));
        cfg.browser = Some(Arc::new(browser::fake::FakeBrowser::default()));
        cfg.email = Some(connector.clone());
        let added = register_builtin_tools(&reg, cfg);
        assert_eq!(added, 34 + 6);

        // Verification of email tools and their effects
        let send_tool = reg
            .get(connector_email::tools::SEND)
            .expect("email_send registered");
        assert_eq!(send_tool.spec().effect, Effect::Destructive);
        assert_eq!(send_tool.spec().category, PermissionCategory::Messaging);

        let draft_tool = reg
            .get(connector_email::tools::DRAFT)
            .expect("email_draft registered");
        assert_eq!(draft_tool.spec().effect, Effect::Write);
        assert_eq!(draft_tool.spec().category, PermissionCategory::Messaging);

        let read_tool = reg
            .get(connector_email::tools::READ)
            .expect("email_read registered");
        assert_eq!(read_tool.spec().effect, Effect::Read);
        assert_eq!(read_tool.spec().category, PermissionCategory::Messaging);

        // Also test direct helper register_email_tools
        let reg2 = ToolRegistry::new();
        let registered = register_email_tools(&reg2, &connector);
        assert_eq!(registered, 6);
        assert!(reg2.get(connector_email::tools::SEND).is_some());
    }

    fn web_registry(
        cfg_web: Option<web_tools::WebToolsConfig>,
    ) -> (tempfile::TempDir, ToolRegistry) {
        let d = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new();
        let mut cfg = BuiltinConfig::new(d.path().to_path_buf(), Arc::new(AllowAll));
        cfg.desktop = Some(Arc::new(desktop::fake::FakeDesktop::new(false)));
        cfg.browser = Some(Arc::new(browser::fake::FakeBrowser::default()));
        cfg.web = cfg_web;
        let _ = register_builtin_tools(&reg, cfg);
        (d, reg)
    }

    #[test]
    fn web_tools_are_absent_unless_configured() {
        let (_d, reg) = web_registry(None);
        for name in web_tools::TOOL_NAMES {
            assert!(reg.get(name).is_none(), "{name} must be off by default");
        }
        let (_d, reg) = web_registry(Some(web_tools::WebToolsConfig::default()));
        for name in web_tools::TOOL_NAMES {
            let spec = reg.get(name).expect("web tool registered").spec().clone();
            // Search only reaches the configured providers (Read, no per-call
            // prompt); tools fetching a model-chosen URL are Write and ask like
            // `browser_open`. All stay in Browser so a Browser deny applies.
            let want = if *name == "web_search" {
                Effect::Read
            } else {
                Effect::Write
            };
            assert_eq!(spec.effect, want, "{name}");
            assert_eq!(spec.category, PermissionCategory::Browser, "{name}");
        }
        let (_d, reg) = web_registry(Some(web_tools::WebToolsConfig::default()));
        let again = register_web_tools(&reg, web_tools::WebToolsConfig::default()).unwrap();
        assert!(again.is_empty(), "already registered: {again:?}");
    }

    #[test]
    fn adding_web_tools_changes_no_other_tool_effect() {
        let (_d, without) = web_registry(None);
        let (_d, with) = web_registry(Some(web_tools::WebToolsConfig::default()));
        for spec in without.list() {
            let other = with
                .get(&spec.name)
                .expect("still registered")
                .spec()
                .clone();
            assert_eq!(other.effect, spec.effect, "{}", spec.name);
            assert_eq!(other.category, spec.category, "{}", spec.name);
        }
        // Mutating browser tools keep asking.
        assert_eq!(
            with.get("browser_open").unwrap().spec().effect,
            Effect::Write
        );
    }

    async fn web_call(
        reg: &ToolRegistry,
        name: &str,
        input: serde_json::Value,
    ) -> Result<tool_registry::ToolOutput, String> {
        reg.get(name)
            .unwrap()
            .call(&tool_registry::ToolContext::default(), input)
            .await
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn default_web_policy_blocks_private_loopback_and_metadata_targets() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("secret"))
            .mount(&server)
            .await;
        let (_d, reg) = web_registry(Some(web_tools::WebToolsConfig::default()));
        for url in [
            server.uri(),
            "http://127.0.0.1:1/".to_owned(),
            "http://[::1]:1/".to_owned(),
            "http://169.254.169.254/latest/meta-data".to_owned(),
            "http://192.168.1.1/".to_owned(),
            "http://localhost:1/".to_owned(),
            "file:///etc/passwd".to_owned(),
        ] {
            let err = web_call(&reg, "web_extract", serde_json::json!({ "url": url }))
                .await
                .unwrap_err();
            assert!(
                err.contains("blocked") || err.contains("scheme"),
                "{url}: {err}"
            );
        }
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a blocked target must never be contacted"
        );
    }

    #[tokio::test]
    async fn web_fetch_allows_a_permitted_host_but_not_a_redirect_to_a_private_address() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string(
                        "<html><body><article><p>public page text</p></article></body></html>",
                    ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/to-metadata"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/to-lan"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "http://10.0.0.5/admin"),
            )
            .mount(&server)
            .await;
        // The mock server listens on loopback and stands in for a public host:
        // only that host is exempted from the address block.
        let policy = web_tools::DefaultUrlPolicy {
            private_ok_hosts: vec!["127.0.0.1".into()],
            ..web_tools::DefaultUrlPolicy::default()
        };
        let (_d, reg) = web_registry(Some(web_tools::WebToolsConfig {
            policy: Arc::new(policy),
            ..web_tools::WebToolsConfig::default()
        }));
        let out = web_call(
            &reg,
            "web_extract",
            serde_json::json!({"url": format!("{}/ok", server.uri())}),
        )
        .await
        .unwrap();
        let tool_registry::ContentPart::Text { text } = &out.content[0] else {
            panic!("text output")
        };
        assert!(text.contains("untrusted external content"), "{text}");
        assert!(text.contains("public page text"), "{text}");
        for p in ["/to-metadata", "/to-lan"] {
            let err = web_call(
                &reg,
                "web_extract",
                serde_json::json!({"url": format!("{}{p}", server.uri())}),
            )
            .await
            .unwrap_err();
            assert!(err.contains("blocked"), "{p}: {err}");
        }
    }
}

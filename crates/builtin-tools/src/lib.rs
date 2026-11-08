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
pub mod script;
pub mod shell;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use desktop_api::DesktopBackend;
use tool_registry::{Tool, ToolRegistry};

pub use browser::{BrowserDriver, BrowserSettings, CdpDriver, Wait};
pub use checkpoint::{Checkpoint, CheckpointEntry, CheckpointStore};
pub use control::{ControlRequest, DesktopControlGate};
pub use desktop::{DesktopSettings, SharedOcr};
pub use meta::{META_TOOL_NAMES, QUESTION_CARD_KIND, TODO_LIST_KIND, TodoItem, TodoStatus};
pub use script::{
    GateHost, GateRequest, ResolverGate, SCRIPT_TOOL_NAME, ToolGate, raw_command_for,
};

pub use connector_email;

/// Everything needed to build the built-in tool set.
pub struct BuiltinConfig {
    /// Application data directory (checkpoints, spill files, screenshots).
    pub data_dir: PathBuf,
    /// Desktop backend (local or remote). Desktop tools are skipped when `None`.
    pub desktop: Option<Arc<dyn DesktopBackend>>,
    /// OCR engine used for set-of-marks when the accessibility tree is weak.
    pub ocr: Option<SharedOcr>,
    /// Desktop timing settings.
    pub desktop_settings: DesktopSettings,
    /// Browser driver. `None` uses a lazily launched [`CdpDriver`] with `browser_settings`.
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
}

impl BuiltinConfig {
    /// A config with defaults: isolated headless browser, default script limits,
    /// no desktop backend, no OCR and no email connector.
    #[must_use]
    pub fn new(data_dir: PathBuf, gate: Arc<dyn ToolGate>) -> Self {
        Self {
            browser_settings: BrowserSettings::isolated(&data_dir),
            data_dir,
            desktop: None,
            ocr: None,
            desktop_settings: DesktopSettings::default(),
            browser: None,
            gate,
            script_limits: script_runtime::Limits::default(),
            control: None,
            email: None,
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
    let mut all: Vec<Arc<dyn Tool>> = Vec::new();
    if let Some(backend) = cfg.desktop {
        all.extend(desktop::tools(
            backend,
            cfg.ocr,
            &cfg.data_dir,
            cfg.desktop_settings,
            cfg.control.clone(),
        ));
    }
    let driver: Arc<dyn BrowserDriver> = cfg
        .browser
        .unwrap_or_else(|| Arc::new(CdpDriver::new(cfg.browser_settings)));
    all.extend(browser::tools(driver, &cfg.data_dir, cfg.control));
    all.extend(files::tools(&cfg.data_dir));
    all.push(shell::tool(&cfg.data_dir));
    all.push(script::tool(registry.clone(), cfg.gate, cfg.script_limits));
    all.extend(meta::tools());
    if let Some(email) = &cfg.email {
        all.extend(connector_email::tools(email));
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
}

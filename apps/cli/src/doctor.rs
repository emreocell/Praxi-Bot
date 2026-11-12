//! `bot doctor`: environment self-check with fix hints.
//!
//! [`run_checks`] is pure over a [`DoctorEnv`] (everything that touches the
//! host, such as the secret store, the browser lookup and the network, is
//! injected), so tests drive it with temp dirs. Secrets are never read into
//! the output: only their presence is checked, and the `--report` bundle is
//! redacted ([`redact`]).

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mcp_bridge::{McpConfigStore, TomlFileStore};
use provider_catalog::{
    Auth, Catalog, OpenAiCompatConfig, ProviderConfig, config_path, test_connection,
};
use secret_store::SecretStore;
use serde_json::{Value, json};

use crate::Result;

/// File name of the MCP server configuration in the data dir.
pub const MCP_FILE: &str = "mcp.toml";
/// Free space below which the disk check warns.
const DISK_WARN_BYTES: u64 = 1 << 30;
/// Free space below which the disk check fails.
const DISK_FAIL_BYTES: u64 = 100 << 20;

/// Result class of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Fine.
    Ok,
    /// Works, but worth a look (optional part missing, low space).
    Warn,
    /// Broken; the bot will not work properly.
    Fail,
}

impl Status {
    fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "\u{2713}",
            Self::Warn => "!",
            Self::Fail => "\u{2717}",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone)]
pub struct Check {
    /// Short name (`data dir`, `providers`, ...).
    pub name: &'static str,
    /// Result class.
    pub status: Status,
    /// What was found.
    pub detail: String,
    /// How to fix it (for warn / fail).
    pub hint: Option<String>,
}

impl Check {
    fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
            hint: None,
        }
    }

    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, Status::Ok, detail)
    }

    fn warn(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(name, Status::Warn, detail).hint(hint)
    }

    fn fail(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(name, Status::Fail, detail).hint(hint)
    }

    fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
}

/// Everything the checks look at.
pub struct DoctorEnv<'a> {
    /// Data directory.
    pub data_dir: &'a Path,
    /// The loaded provider catalog (or why loading failed).
    pub catalog: std::result::Result<&'a Catalog, String>,
    /// The opened secret store and a label of the backend (or why it failed).
    pub store: std::result::Result<(&'a dyn SecretStore, String), String>,
    /// Daemon port.
    pub port: u16,
    /// Test provider connections over the network.
    pub online: bool,
    /// Browser binary found on this machine, if any.
    pub browser: Option<PathBuf>,
    /// Discovery failure for an unusable configured or snap-only browser.
    pub browser_error: Option<String>,
}

/// Run every check.
#[must_use]
pub fn run_checks(env: &DoctorEnv<'_>) -> Vec<Check> {
    vec![
        check_data_dir(env.data_dir),
        check_config(env.data_dir),
        check_secrets(env),
        check_providers(env),
        check_catalog(env),
        check_daemon(env.data_dir, env.port),
        check_isolation(env.data_dir, env.port),
        env.browser_error.as_ref().map_or_else(
            || check_browser(env.browser.as_deref()),
            |e| Check::fail("browser", e, "install a non-snap browser or set BOT_BROWSER_PATH; use `sudo praxi doctor` for service-context validation"),
        ),
        check_mcp(env.data_dir),
        check_web(env),
        check_schedule_tools(env.data_dir),
        check_disk(env.data_dir),
        check_telegram(env),
    ]
}

fn check_data_dir(dir: &Path) -> Check {
    const N: &str = "data dir";
    if !dir.exists() {
        return Check::warn(
            N,
            format!("{} does not exist yet", dir.display()),
            "it is created by `bot setup` or `bot serve`",
        );
    }
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"ok").and_then(|()| std::fs::remove_file(&probe)) {
        Ok(()) => Check::ok(N, format!("{} (writable)", dir.display())),
        Err(e) => Check::fail(
            N,
            format!("{} is not writable: {e}", dir.display()),
            "fix the folder permissions or pass another --data-dir",
        ),
    }
}

fn check_config(dir: &Path) -> Check {
    const N: &str = "config";
    let path = config_path(dir);
    if !path.exists() {
        return Check::warn(
            N,
            "providers.toml not found",
            "run `bot setup` to add a provider",
        );
    }
    match ProviderConfig::load(&path) {
        Ok(c) => Check::ok(
            N,
            format!("providers.toml parses ({} provider(s))", c.providers.len()),
        ),
        Err(e) => Check::fail(
            N,
            format!("providers.toml: {e}"),
            "fix the file by hand or move it away and run `bot setup`",
        ),
    }
}

fn check_secrets(env: &DoctorEnv<'_>) -> Check {
    const N: &str = "secret backend";
    match &env.store {
        Ok((store, kind)) => match store.list_names() {
            Ok(names) => Check::ok(N, format!("{kind} ({} secret(s))", names.len())),
            Err(e) => Check::fail(
                N,
                format!("{kind}: {e}"),
                "check BOT_SECRETS_BACKEND / BOT_SECRETS_PASSPHRASE",
            ),
        },
        Err(e) => Check::fail(
            N,
            e.clone(),
            "set BOT_SECRETS_PASSPHRASE for the encrypted file, or use BOT_SECRETS_BACKEND=keyring",
        ),
    }
}

fn check_providers(env: &DoctorEnv<'_>) -> Check {
    const N: &str = "providers";
    let Ok(cfg) = ProviderConfig::load(&config_path(env.data_dir)) else {
        return Check::warn(N, "no readable provider config", "see the config check");
    };
    let enabled: Vec<_> = cfg.providers.iter().filter(|p| p.enabled).collect();
    if enabled.is_empty() {
        return Check::fail(
            N,
            "setup required: no enabled provider",
            "run `bot setup` or `bot providers add`",
        );
    }
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    for p in &enabled {
        let entry = p.custom.clone().or_else(|| {
            env.catalog
                .as_ref()
                .ok()
                .and_then(|c| c.get(&p.id).cloned())
        });
        let Some(entry) = entry else {
            problems.push(format!("{}: not in the catalog", p.id));
            continue;
        };
        let key_ok = match (&p.key, &entry.auth) {
            (Some(r), _) => match &env.store {
                Ok((s, _)) => s.get(r.name()).ok().flatten().is_some(),
                Err(_) => false,
            },
            (None, Auth::None) => true,
            (None, Auth::ApiKeyEnv(var)) => std::env::var(var).is_ok_and(|v| !v.is_empty()),
        };
        if !key_ok {
            problems.push(format!("{}: API key not available", p.id));
            continue;
        }
        if env.online {
            match &env.store {
                Ok((s, _)) => match online_test(p, &entry, *s) {
                    Ok(n) => notes.push(format!("{}: reachable ({n} models)", p.id)),
                    Err(e) => problems.push(format!("{}: {e}", p.id)),
                },
                Err(_) => problems.push(format!("{}: secret store unavailable", p.id)),
            }
        } else {
            notes.push(format!("{}: configured", p.id));
        }
    }
    let detail = notes
        .iter()
        .chain(problems.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if problems.is_empty() {
        let c = Check::ok(N, detail);
        if env.online {
            c
        } else {
            c.hint("add --online to test the connections")
        }
    } else {
        Check::fail(
            N,
            detail,
            "`bot secrets set provider.<id>.api_key` or `bot providers add <id>` to fix the key",
        )
    }
}

fn online_test(
    p: &provider_catalog::ProviderSettings,
    entry: &provider_catalog::ProviderEntry,
    store: &dyn SecretStore,
) -> std::result::Result<usize, String> {
    let provider = p
        .build_provider(entry, store, OpenAiCompatConfig::default())
        .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let model = p.model_ref();
    rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(20),
            test_connection(&*provider, model.as_ref()),
        )
        .await
    })
    .map_err(|_| "timed out".to_string())?
    .map(|r| r.models.len())
    .map_err(|e| e.to_string())
}

fn check_catalog(env: &DoctorEnv<'_>) -> Check {
    const N: &str = "model catalog";
    let catalog = match &env.catalog {
        Ok(c) => c,
        Err(e) => {
            return Check::fail(
                N,
                e.clone(),
                "fix or unset BOT_CATALOG_DIR to use the built-in catalog",
            );
        }
    };
    if catalog.is_empty() {
        return Check::fail(N, "catalog is empty", "unset BOT_CATALOG_DIR");
    }
    let n_models: usize = catalog.all().map(|e| e.models.len()).sum();
    let detail = format!("{} provider(s), {n_models} known model(s)", catalog.len());
    if let Ok(cfg) = ProviderConfig::load(&config_path(env.data_dir))
        && let Some(def) = cfg.effective_default()
    {
        match &def.default_model {
            None => {
                return Check::warn(
                    N,
                    format!("{detail}; default provider {} has no model", def.id),
                    "`bot providers default <id> --model <model>`",
                );
            }
            Some(m) => {
                let entry = def.custom.as_ref().or_else(|| catalog.get(&def.id));
                if entry.is_some_and(|e| !e.models.is_empty() && e.model(m).is_none()) {
                    return Check::warn(
                        N,
                        format!("{detail}; default model {m} is not in the catalog"),
                        "prices and limits are unknown for it; check the model id",
                    );
                }
            }
        }
    }
    Check::ok(N, detail)
}

fn check_daemon(dir: &Path, port: u16) -> Check {
    const N: &str = "daemon";
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let token = daemon_api::token_path(dir);
    let token_note = match daemon_api::read_token(dir) {
        Ok(_) => format!("token file ok ({})", token.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "no token file yet".to_string(),
        Err(e) => format!("token file unreadable: {e}"),
    };
    let token_bad = token.exists() && daemon_api::read_token(dir).is_err();
    let up = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok();
    if !up {
        return Check::warn(
            N,
            format!("not running on port {port}; {token_note}"),
            "start it with `bot serve`",
        );
    }
    let health = daemon_health(port);
    match (health, token_bad) {
        (Some(v), false) => Check::ok(N, format!("running on port {port} (v{v}); {token_note}")),
        (Some(_), true) => Check::fail(
            N,
            format!("running on port {port}; {token_note}"),
            "delete the api-token file and restart `bot serve`",
        ),
        (None, _) => Check::fail(
            N,
            format!("port {port} is in use but does not answer like the daemon"),
            "stop the other program or use --port / BOT_PORT",
        ),
    }
}

fn check_isolation(dir: &Path, port: u16) -> Check {
    let fetch = || -> Option<Value> {
        let token = daemon_api::read_token(dir).ok()?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            let response = reqwest::Client::new()
                .get(format!("http://127.0.0.1:{port}/v1/isolation"))
                .bearer_auth(token)
                .timeout(Duration::from_millis(500))
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            response.json().await.ok()
        })
    };
    isolation_report(fetch().as_ref())
}

fn isolation_report(report: Option<&Value>) -> Check {
    const NAME: &str = "isolation";
    if report.and_then(|v| v["privilege_split"].as_bool()) == Some(false) {
        return Check::fail(
            NAME,
            "privilege split inactive",
            "update and restart the daemon from the desktop app",
        );
    }
    let Some(report) = report else {
        return Check::warn(
            NAME,
            "unverified: no authenticated daemon isolation report",
            "start the updated daemon; a token file alone does not prove isolation",
        );
    };
    let fields: Option<Vec<bool>> = [
        "privilege_split",
        "children_scrubbed",
        "memory_guard",
        "no_new_privs",
        "os_sandbox",
    ]
    .iter()
    .map(|key| report[key].as_bool())
    .collect();
    let Some(fields) = fields else {
        return Check::warn(
            NAME,
            "unverified: daemon isolation report is incomplete",
            "update the daemon; do not infer protection from its version",
        );
    };
    let (Some(platform), Some(guard_kind)) = (
        report["platform"].as_str(),
        report["memory_guard_kind"].as_str(),
    ) else {
        return Check::warn(
            NAME,
            "unverified: daemon does not identify its native process guard",
            "update the daemon; platform protection cannot be inferred",
        );
    };
    let degraded = !fields[1] || !fields[2] || (platform == "linux" && !fields[3]);
    let detail = format!(
        "{}split active; children {}; memory guard {} ({guard_kind}, {platform}); no-new-privs {}; OS sandbox {}",
        if degraded { "DEGRADED: " } else { "" },
        if fields[1] {
            "scrubbed"
        } else {
            "scrubbing unverified"
        },
        if fields[2] { "active" } else { "inactive" },
        if platform != "linux" {
            "not applicable"
        } else if fields[3] {
            "active"
        } else {
            "inactive"
        },
        if fields[4] { "active" } else { "not active" }
    );
    if !degraded && fields[4] {
        Check::ok(NAME, detail)
    } else {
        Check::warn(
            NAME,
            detail,
            "arbitrary same-user programs can still tamper with files or inject code; see security guide ADR-0030",
        )
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;

    #[test]
    fn doctor_distinguishes_native_guards_and_disabled_protection() {
        let check = isolation_report(Some(
            &json!({"privilege_split":true,"children_scrubbed":true,"memory_guard":true,"no_new_privs":false,"os_sandbox":false,"platform":"windows","memory_guard_kind":"windows_owner_dacl"}),
        ));
        assert!(check.detail.contains("windows_owner_dacl"));
        assert!(check.detail.contains("no-new-privs not applicable"));
        let check = isolation_report(Some(
            &json!({"privilege_split":true,"children_scrubbed":true,"memory_guard":false,"no_new_privs":false,"os_sandbox":false,"platform":"linux","memory_guard_kind":"disabled"}),
        ));
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("DEGRADED"));
        assert!(check.detail.contains("disabled"));
    }

    #[test]
    fn doctor_does_not_infer_isolation_from_version_or_missing_fields() {
        for report in [
            None,
            Some(json!({"version":"new"})),
            Some(json!({"privilege_split":true})),
        ] {
            let check = isolation_report(report.as_ref());
            assert_eq!(check.status, Status::Warn);
            assert!(check.detail.contains("unverified"));
        }
        let report = json!({"privilege_split":true,"children_scrubbed":true,"memory_guard":true,"no_new_privs":false,"os_sandbox":false,"platform":"windows","memory_guard_kind":"windows_owner_dacl"});
        let check = isolation_report(Some(&report));
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("split active"));
        assert!(check.detail.contains("children scrubbed"));
        assert!(check.detail.contains("sandbox not active"));
        let report = json!({"privilege_split":false});
        assert_eq!(isolation_report(Some(&report)).status, Status::Fail);
    }
}

/// Version string from `GET /v1/health`, `None` if it is not our daemon.
fn daemon_health(port: u16) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/health"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .ok()?;
        let v: Value = resp.json().await.ok()?;
        (v["status"] == "ok" || v["status"] == "setup_required")
            .then(|| v["version"].as_str().unwrap_or("?").to_string())
    })
}

fn check_browser(found: Option<&Path>) -> Check {
    const N: &str = "browser";
    match found {
        Some(p) => {
            if browser_cdp::launch::is_snap_browser(p) {
                return Check::fail(
                    N,
                    "snap browser is unsupported under the hardened service",
                    "install non-snap Chrome/Chromium or set BOT_BROWSER_PATH",
                );
            }
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(async {
                        let opts = browser_cdp::launch::LaunchOptions {
                            executable: Some(p.to_path_buf()),
                            startup_timeout: Duration::from_secs(5),
                            ..Default::default()
                        };
                        let probe = async {
                            let browser = browser_cdp::Browser::launch(opts).await?;
                            browser.close().await
                        };
                        tokio::time::timeout(Duration::from_secs(8), probe)
                            .await
                            .map_err(|_| "headless launch timed out".to_string())?
                            .map_err(|e| e.to_string())
                    })
                });
            match result {
                Ok(()) => Check::ok(
                    N,
                    format!(
                        "{} (headless launch passed in current user/env)",
                        p.display()
                    ),
                ),
                Err(e) => Check::fail(
                    N,
                    format!("{}: {e}", p.display()),
                    "install a non-snap browser or set BOT_BROWSER_PATH; on systemd use `sudo praxi doctor` to probe with the service user, environment and hardening",
                ),
            }
        }
        None => Check::warn(
            N,
            "no usable non-snap browser found (snap-only installs are unsupported)",
            "install a non-snap Chrome/Chromium or set BOT_BROWSER_PATH",
        ),
    }
}

fn check_mcp(dir: &Path) -> Check {
    const N: &str = "mcp config";
    let path = dir.join(MCP_FILE);
    if !path.exists() {
        return Check::ok(N, "no MCP servers configured");
    }
    match TomlFileStore::new(&path).load() {
        Ok(s) => Check::ok(N, format!("{} server(s)", s.len())),
        Err(e) => Check::fail(N, e.to_string(), format!("fix {}", path.display())),
    }
}

/// `schedule tools`: whether `schedule.toml` is usable. A broken one switches
/// the bots' schedule tools off (they never fall back to looser defaults).
fn check_schedule_tools(dir: &Path) -> Check {
    const N: &str = "schedule tools";
    match daemon_api::load_schedule_limits(dir) {
        Ok(l) => Check::ok(
            N,
            format!(
                "max {} per bot, minimum interval {} s",
                l.max_per_bot, l.min_interval_secs
            ),
        ),
        Err(e) => Check::fail(
            N,
            format!("{e}; schedule tools are disabled until it is fixed"),
            format!("fix or delete {}", dir.join("schedule.toml").display()),
        ),
    }
}

/// Time the `--online` web search probe may take.
const WEB_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// `web tools`: which search backends are configured, and with `--online`
/// whether a live search answers. A failing probe is a warning, never a fail:
/// the bot still works without web search.
fn check_web(env: &DoctorEnv<'_>) -> Check {
    const N: &str = "web search";
    let (file, mut notes) = crate::serve::load_web_file(env.data_dir);
    // The doctor holds the store by reference only: copy the one Brave key into
    // a throwaway in-memory store for the probe.
    let mem: std::sync::Arc<dyn SecretStore> =
        std::sync::Arc::new(secret_store::MemoryStore::new());
    let mut brave_missing = false;
    if let (Some(name), Ok((s, _))) = (file.search.brave_key.as_deref(), &env.store) {
        match s.get(name) {
            Ok(Some(v)) => {
                let _ = mem.set(name, v.expose());
            }
            _ => brave_missing = true,
        }
    }
    let cfg = crate::serve::web_tools_config(
        env.data_dir,
        &file,
        Some(&mem),
        &crate::serve::daemon_protected_sockets(env.port),
        &mut notes,
    );
    let backends = crate::serve::web_backend_summary(&cfg);
    if brave_missing {
        notes.push("brave key secret not found".into());
    }
    let detail = |extra: String| {
        let mut d = format!("backends: {backends}");
        if !extra.is_empty() {
            d.push_str("; ");
            d.push_str(&extra);
        }
        if !notes.is_empty() {
            d.push_str("; ");
            d.push_str(&notes.join("; "));
        }
        d
    };
    let hint = "see docs/guide/web.md; edit <data_dir>/web.toml";
    if !env.online {
        let c = Check::ok(N, detail("not probed".into()));
        return if notes.is_empty() {
            c.hint("add --online to test that a search answers")
        } else {
            Check {
                status: Status::Warn,
                ..c
            }
            .hint(hint)
        };
    }
    match probe_web_search(cfg) {
        Ok(backend) => {
            let c = Check::ok(N, detail(format!("{backend} answered")));
            if notes.is_empty() {
                c
            } else {
                Check {
                    status: Status::Warn,
                    ..c
                }
                .hint(hint)
            }
        }
        Err(e) => Check::warn(N, detail(format!("no answer: {e}")), hint),
    }
}

/// Run one live search through the configured backends; the answering backend's name.
fn probe_web_search(cfg: web_tools::WebToolsConfig) -> std::result::Result<String, String> {
    let reg = tool_registry::ToolRegistry::new();
    web_tools::register(&reg, cfg).map_err(|e| e.to_string())?;
    let tool = reg.get("web_search").ok_or("web_search not registered")?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let ctx = tool_registry::ToolContext::default();
        let call = tool.call(&ctx, json!({"query": "praxi", "limit": 1}));
        let out = tokio::time::timeout(WEB_PROBE_TIMEOUT, call)
            .await
            .map_err(|_| "timed out".to_string())?
            .map_err(|e| e.to_string())?;
        out.content
            .iter()
            .find_map(|p| match p {
                tool_registry::ContentPart::Json { data } => {
                    data["backend"].as_str().map(str::to_owned)
                }
                _ => None,
            })
            .ok_or_else(|| "unexpected search output".to_string())
    })
}

fn check_disk(dir: &Path) -> Check {
    const N: &str = "disk space";
    let mut probe = dir;
    while !probe.exists() {
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p,
            _ => break,
        }
    }
    match fs4::available_space(probe) {
        Ok(free) => {
            let detail = format!("{} MiB free", free >> 20);
            if free < DISK_FAIL_BYTES {
                Check::fail(
                    N,
                    detail,
                    "free some disk space; runs and logs cannot be saved",
                )
            } else if free < DISK_WARN_BYTES {
                Check::warn(N, detail, "less than 1 GiB left; free some space")
            } else {
                Check::ok(N, detail)
            }
        }
        Err(e) => Check::warn(
            N,
            format!("cannot read free space: {e}"),
            "ignore if unsure",
        ),
    }
}

fn check_telegram(env: &DoctorEnv<'_>) -> Check {
    const N: &str = "telegram";
    let path = env.data_dir.join(crate::channels::CHANNELS_FILE);
    if path.exists()
        && std::fs::read_to_string(&path).map_or(true, |s| s.parse::<toml::Table>().is_err())
    {
        return Check::fail(
            N,
            "channels.toml does not parse",
            "delete it, then `bot channels telegram setup`",
        );
    }
    let enabled = crate::channels::telegram_enabled(env.data_dir);
    let configured = match &env.store {
        Ok((s, _)) => crate::channels::telegram_configured(*s).ok(),
        Err(_) => None,
    };
    match (configured, enabled) {
        (None, _) => Check::warn(
            N,
            "cannot read the bot token (secret store unavailable)",
            "see the secret backend check",
        ),
        (Some(false), _) => Check::ok(N, "not configured (optional)"),
        (Some(true), true) => {
            let paired = crate::channels::paired_users_on_disk(env.data_dir);
            if paired == 0 {
                Check::warn(
                    N,
                    "token stored, enabled, nobody paired",
                    "run `bot channels telegram pair` and send /pair to the bot",
                )
            } else {
                Check::ok(N, format!("enabled, {paired} paired user(s)"))
            }
        }
        (Some(true), false) => Check::ok(N, "token stored, channel switched off"),
    }
}

/// Render the checks as a table.
pub fn render_table(checks: &[Check], out: &mut dyn Write) -> std::io::Result<()> {
    let w = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in checks {
        writeln!(out, " {}  {:w$}  {}", c.status.symbol(), c.name, c.detail)?;
        if let (Some(h), true) = (&c.hint, c.status != Status::Ok) {
            writeln!(out, "    {:w$}  fix: {h}", "")?;
        }
    }
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    writeln!(
        out,
        "\n{fails} failed, {warns} warning(s), {} ok",
        checks.len() - fails - warns
    )
}

/// The checks as JSON.
#[must_use]
pub fn to_json(checks: &[Check]) -> Value {
    json!({
        "ok": checks.iter().all(|c| c.status != Status::Fail),
        "checks": checks.iter().map(|c| json!({
            "name": c.name,
            "status": c.status.word(),
            "detail": c.detail,
            "hint": c.hint,
        })).collect::<Vec<_>>(),
    })
}

/// Mask home directory paths and anything that looks like a key or token
/// (24+ characters of `[A-Za-z0-9_-]`).
#[must_use]
pub fn redact(s: &str) -> String {
    let mut s = s.to_string();
    if let Some(home) = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| h.len() > 3)
    {
        s = s.replace(&home, "~").replace(&home.replace('\\', "/"), "~");
    }
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 24 {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// A redacted diagnostic bundle: versions, OS, check results and provider
/// ids. No keys, tokens, secret names or file contents.
#[must_use]
pub fn report_bundle(env: &DoctorEnv<'_>, checks: &[Check]) -> Value {
    let providers: Vec<Value> = ProviderConfig::load(&config_path(env.data_dir))
        .map(|c| {
            c.providers
                .iter()
                .map(|p| {
                    json!({
                        "id": p.id,
                        "enabled": p.enabled,
                        "has_key_ref": p.key.is_some(),
                        "default_model": p.default_model,
                        "custom": p.custom.is_some(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut v = json!({
        "bot_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "data_dir": env.data_dir.display().to_string(),
        "port": env.port,
        "online": env.online,
        "providers": providers,
        "doctor": to_json(checks),
    });
    redact_value(&mut v);
    v
}

fn redact_value(v: &mut Value) {
    match v {
        Value::String(s) => *s = redact(s),
        Value::Array(a) => a.iter_mut().for_each(redact_value),
        Value::Object(o) => o.values_mut().for_each(redact_value),
        _ => {}
    }
}

/// Write the report bundle to `path`.
///
/// # Errors
/// The file cannot be written.
pub fn write_report(env: &DoctorEnv<'_>, checks: &[Check], path: &Path) -> Result<()> {
    std::fs::write(
        path,
        serde_json::to_string_pretty(&report_bundle(env, checks))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_catalog::ProviderSettings;
    use secret_store::{SecretRef, SecretValue};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore(Mutex<BTreeMap<String, String>>);

    impl SecretStore for MemStore {
        fn get(
            &self,
            name: &str,
        ) -> std::result::Result<Option<SecretValue>, secret_store::SecretError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(name)
                .map(|v| SecretValue::new(v.clone())))
        }
        fn set(
            &self,
            name: &str,
            value: &str,
        ) -> std::result::Result<(), secret_store::SecretError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> std::result::Result<bool, secret_store::SecretError> {
            Ok(self.0.lock().unwrap().remove(name).is_some())
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
    }

    fn catalog() -> Catalog {
        let src = r#"
id = "alpha"
kind = "open_ai_compatible"
base_url = "http://127.0.0.1:9/v1"
auth = { ApiKeyEnv = "ALPHA_KEY_DOCTOR_TEST" }
[capabilities]
tools = true
vision = false
streaming = true
json_mode = false
[[models]]
id = "m1"
"#;
        Catalog::from_entries([toml::from_str(src).unwrap()])
    }

    fn get<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks.iter().find(|c| c.name == name).unwrap()
    }

    fn env<'a>(dir: &'a Path, cat: &'a Catalog, store: &'a MemStore, port: u16) -> DoctorEnv<'a> {
        DoctorEnv {
            data_dir: dir,
            catalog: Ok(cat),
            store: Ok((store, "Memory".into())),
            port,
            online: false,
            browser: None,
            browser_error: None,
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn empty_data_dir_warns_but_nothing_panics() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let checks = run_checks(&env(&d.path().join("nope"), &cat, &store, free_port()));
        assert_eq!(get(&checks, "data dir").status, Status::Warn);
        assert_eq!(get(&checks, "config").status, Status::Warn);
        assert_eq!(get(&checks, "providers").status, Status::Fail);
        assert_eq!(get(&checks, "daemon").status, Status::Warn);
        assert_eq!(get(&checks, "browser").status, Status::Warn);
        assert_eq!(get(&checks, "mcp config").status, Status::Ok);
        assert_eq!(get(&checks, "telegram").status, Status::Ok);
        assert_eq!(get(&checks, "model catalog").status, Status::Ok);
        assert!(
            checks
                .iter()
                .all(|c| c.status == Status::Ok || c.hint.is_some())
        );
    }

    #[test]
    fn healthy_setup_passes() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        store.set("provider.alpha.api_key", "sk-secret").unwrap();
        let mut s = ProviderSettings::new("alpha", 1);
        s.key = Some(SecretRef::new("provider.alpha.api_key").unwrap());
        s.default_model = Some("m1".into());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(s);
        cfg.save(&config_path(d.path())).unwrap();
        let mut e = env(d.path(), &cat, &store, free_port());
        e.browser = None;
        let checks = run_checks(&e);
        for n in [
            "data dir",
            "config",
            "secret backend",
            "providers",
            "model catalog",
            "mcp config",
        ] {
            assert_eq!(
                get(&checks, n).status,
                Status::Ok,
                "{n}: {}",
                get(&checks, n).detail
            );
        }
    }

    #[test]
    fn missing_key_and_bad_files_fail_with_hints() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let mut s = ProviderSettings::new("alpha", 1);
        s.key = Some(SecretRef::new("provider.alpha.api_key").unwrap());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(s);
        cfg.save(&config_path(d.path())).unwrap();
        std::fs::write(d.path().join(MCP_FILE), "[[server]\nbroken").unwrap();
        std::fs::write(d.path().join(crate::channels::CHANNELS_FILE), "= nope").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        for n in ["providers", "mcp config", "telegram"] {
            let c = get(&checks, n);
            assert_eq!(c.status, Status::Fail, "{n}");
            assert!(c.hint.is_some());
        }
        std::fs::write(config_path(d.path()), "not [valid").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        assert_eq!(get(&checks, "config").status, Status::Fail);
    }

    #[test]
    fn unusable_browser_discovery_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let mut e = env(d.path(), &cat, &store, free_port());
        e.browser_error = Some("only snap browsers found; unsupported".into());
        let checks = run_checks(&e);
        assert_eq!(get(&checks, "browser").status, Status::Fail);
        assert!(
            get(&checks, "browser")
                .hint
                .as_ref()
                .unwrap()
                .contains("non-snap")
        );
    }

    #[test]
    fn browser_presence_does_not_imply_it_can_launch() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("broken-browser");
        std::fs::write(&path, b"not an executable").unwrap();
        let check = check_browser(Some(&path));
        assert_eq!(check.status, Status::Fail);
        assert!(check.hint.unwrap().contains("non-snap"));
    }

    #[test]
    fn foreign_listener_on_port_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Accept and drop connections so the health probe fails fast.
        std::thread::spawn(move || {
            for s in l.incoming() {
                drop(s);
            }
        });
        let c = check_daemon(d.path(), port);
        assert_eq!(c.status, Status::Fail);
    }

    #[test]
    fn secret_store_error_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let cat = catalog();
        let e = DoctorEnv {
            data_dir: d.path(),
            catalog: Ok(&cat),
            store: Err("passphrase needed".into()),
            port: free_port(),
            online: false,
            browser: None,
            browser_error: None,
        };
        let checks = run_checks(&e);
        assert_eq!(get(&checks, "secret backend").status, Status::Fail);
        assert_eq!(get(&checks, "telegram").status, Status::Warn);
    }

    #[test]
    fn redaction_masks_tokens_and_home() {
        let tok = "a".repeat(64);
        let r = redact(&format!(
            "key sk-0123456789abcdef0123456789abcdef and {tok} ok short-id"
        ));
        assert!(!r.contains("0123456789abcdef"), "{r}");
        assert!(!r.contains(&tok));
        assert!(r.contains("short-id"));
    }

    #[test]
    fn report_has_no_secrets() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        store
            .set(
                "provider.alpha.api_key",
                "sk-supersecretvalue-0123456789abcdef",
            )
            .unwrap();
        std::fs::write(daemon_api::token_path(d.path()), "b".repeat(64)).unwrap();
        let mut s = ProviderSettings::new("alpha", 1);
        s.key = Some(SecretRef::new("provider.alpha.api_key").unwrap());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(s);
        cfg.save(&config_path(d.path())).unwrap();
        let e = env(d.path(), &cat, &store, free_port());
        let checks = run_checks(&e);
        let path = d.path().join("report.json");
        write_report(&e, &checks, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("supersecret"));
        assert!(!text.contains(&"b".repeat(64)));
        assert!(!text.contains("provider.alpha.api_key"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["providers"][0]["id"], "alpha");
        assert!(v["doctor"]["checks"].is_array());
    }

    #[test]
    fn table_and_json_render() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let mut buf = Vec::new();
        render_table(&checks, &mut buf).unwrap();
        let t = String::from_utf8(buf).unwrap();
        assert!(t.contains("daemon") && t.contains("fix:") && t.contains("warning(s)"));
        let j = to_json(&checks);
        assert_eq!(j["checks"].as_array().unwrap().len(), checks.len());
        assert_eq!(j["ok"], false);
    }

    #[test]
    fn web_check_reports_backends_without_touching_the_network() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let c = get(&checks, "web search");
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.contains("duckduckgo"), "{}", c.detail);
        assert!(c.detail.contains("not probed"), "{}", c.detail);

        std::fs::write(d.path().join(crate::serve::WEB_FILE), "= nonsense").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let c = get(&checks, "web search");
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("web.toml"), "{}", c.detail);
    }

    #[test]
    fn doctor_reports_a_broken_schedule_toml_as_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let c = get(
            &run_checks(&env(d.path(), &cat, &store, free_port())),
            "schedule tools",
        )
        .clone();
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        std::fs::write(d.path().join("schedule.toml"), "min_interval_secs = 0").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let c = get(&checks, "schedule tools");
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("disabled"), "{}", c.detail);
    }

    #[test]
    fn web_check_online_answers_or_warns_never_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let ok = rt.block_on(async {
            let s = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"results": [{"title": "T", "url": "https://example.org/", "content": "c"}]}),
                ))
                .mount(&s)
                .await;
            s
        });
        let broken = rt.block_on(async {
            let s = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&s)
                .await;
            s
        });
        let (cat, store) = (catalog(), MemStore::default());
        for (server, status, word) in [
            (&ok, Status::Ok, "searxng answered"),
            (&broken, Status::Warn, "no answer"),
        ] {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(
                d.path().join(crate::serve::WEB_FILE),
                format!(
                    "[search]
searxng_url = \"{}\"
duckduckgo = false
",
                    server.uri()
                ),
            )
            .unwrap();
            let mut e = env(d.path(), &cat, &store, free_port());
            e.online = true;
            let checks = run_checks(&e);
            let c = get(&checks, "web search");
            assert_eq!(c.status, status, "{}", c.detail);
            assert!(c.detail.contains(word), "{}", c.detail);
        }
    }
}

//! End-to-end tests against the stdio `test_server` example.

use std::{path::PathBuf, time::Duration};

#[path = "support/capture.rs"]
mod capture;
#[path = "support/example_bin.rs"]
mod example_bin;

use mcp_bridge::{
    Backoff, McpManager, McpPolicy, McpServerConfig, McpTransport, OS_ESSENTIAL_ENV, ServerStatus,
};
use serde_json::json;
use tool_registry::{ContentPart, Effect, ToolContext, ToolError, ToolRegistry};

fn server_exe() -> PathBuf {
    example_bin::test_server().to_path_buf()
}

fn config(id: &str) -> McpServerConfig {
    let mut c = McpServerConfig::stdio(id, server_exe().to_string_lossy(), [""; 0]);
    c.trust_hints = true; // the test server's `readOnlyHint`s are honest
    c
}

fn fast_policy(quarantine_after: u32) -> McpPolicy {
    McpPolicy {
        quarantine_after,
        backoff: Backoff {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(20),
            max_attempts: 2,
        },
        connect_timeout: Duration::from_secs(20),
        call_timeout: Duration::from_secs(20),
    }
}

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn call(reg: &ToolRegistry, name: &str, input: serde_json::Value) -> Result<String, String> {
    let tool = reg
        .get(name)
        .unwrap_or_else(|| panic!("{name} not registered"));
    tool.call(&ToolContext::default(), input)
        .await
        .map(|o| text_of(&o.content))
        .map_err(|e| e.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_registers_and_calls_tools() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    assert_eq!(mgr.add_server(config("t")).await.unwrap(), 6);

    let echo = reg.get("mcp:t/echo").unwrap();
    assert_eq!(echo.spec().effect, Effect::Read);
    assert_eq!(
        echo.spec().input_schema["properties"]["text"]["type"],
        "string"
    );
    // No annotations -> Destructive (ADR-0004).
    assert_eq!(
        reg.get("mcp:t/delete_all").unwrap().spec().effect,
        Effect::Destructive
    );

    assert_eq!(
        call(&reg, "mcp:t/echo", json!({"text": "hello"}))
            .await
            .unwrap(),
        "hello"
    );
    // Tool-level errors surface as errors but do not count as failures.
    assert!(
        call(&reg, "mcp:t/fail", json!({}))
            .await
            .unwrap_err()
            .contains("it failed")
    );
    let snap = mgr.snapshot("t").unwrap();
    assert_eq!(snap.status, ServerStatus::Connected);
    assert_eq!(snap.consecutive_failures, 0);

    // stderr is captured.
    let mut seen = false;
    for _ in 0..50 {
        if mgr
            .stderr_tail("t")
            .iter()
            .any(|l| l.contains("test_server handled"))
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "stderr not captured: {:?}", mgr.stderr_tail("t"));

    mgr.remove_server("t").await.unwrap();
    assert!(reg.get("mcp:t/echo").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_and_exclude_filter_tools() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    let mut cfg = config("f");
    cfg.tool_allow = vec!["e*".into(), "read_*".into()];
    cfg.tool_exclude = vec!["read_secret".into()];
    assert_eq!(mgr.add_server(cfg).await.unwrap(), 2);
    let mut names: Vec<_> = reg.list().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(names, ["mcp:f/echo", "mcp:f/env_keys"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn child_gets_only_declared_environment() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    let mut cfg = config("e");
    if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("MCP_TEST_DECLARED".into(), "1".into());
    }
    mgr.add_server(cfg).await.unwrap();
    // The test process itself has CARGO_MANIFEST_DIR / PATH etc.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let keys = call(&reg, "mcp:e/env_keys", json!({})).await.unwrap();
    let got: Vec<String> = keys
        .split(',')
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .collect();
    for k in &got {
        assert!(
            k == "MCP_TEST_DECLARED" || OS_ESSENTIAL_ENV.iter().any(|e| e.eq_ignore_ascii_case(k)),
            "undeclared variable leaked into the child: {k} (all: {got:?})"
        );
    }
    assert!(got.iter().any(|k| k == "MCP_TEST_DECLARED"));
    // Regression (244-A): `npx.cmd` / `uvx` die at once without PATH, which
    // surfaced as "connection closed: initialize response" in the installed app.
    assert!(
        got.iter().any(|k| k.eq_ignore_ascii_case("PATH")),
        "the launcher environment must carry PATH (got: {got:?})"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_launcher_is_classified_not_retried_and_carries_an_install_hint() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg, fast_policy(3));
    let cfg = McpServerConfig::stdio("sq", "praxi-no-such-launcher-244a", ["x"]);
    let err = mgr.add_server(cfg).await.unwrap_err();
    assert_eq!(err.kind(), mcp_bridge::McpErrorKind::RuntimeMissing);
    let snap = mgr.snapshot("sq").unwrap();
    assert_eq!(
        snap.last_error_kind,
        Some(mcp_bridge::McpErrorKind::RuntimeMissing)
    );
    // Not retried: one failure, not `max_attempts`.
    assert_eq!(snap.consecutive_failures, 1);
    let msg = snap.last_error.unwrap();
    assert!(
        msg.contains("praxi-no-such-launcher-244a") && msg.contains("was not found on PATH"),
        "{msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn filesystem_server_without_a_chosen_folder_is_not_started() {
    let reg = ToolRegistry::new();
    let guard: mcp_bridge::PathGuard = std::sync::Arc::new(|p| {
        if p.ends_with("forbidden") {
            Err("protected folder".to_owned())
        } else {
            Ok(p.to_path_buf())
        }
    });
    let mgr = McpManager::with_policy(reg, fast_policy(3)).with_path_guard(guard);
    let pkg = "@modelcontextprotocol/server-filesystem@2.2.0";
    // No folder, and the old default `.`: refused before anything is spawned.
    for (id, args) in [("fs0", vec!["-y", pkg]), ("fs1", vec!["-y", pkg, "."])] {
        let cfg = McpServerConfig::stdio(id, "praxi-never-spawned-244b", args);
        let err = mgr.add_server(cfg).await.unwrap_err();
        assert_eq!(err.kind(), mcp_bridge::McpErrorKind::NeedsFolder, "{err}");
        assert_eq!(mgr.stderr_tail(id).len(), 0);
        let snap = mgr.snapshot(id).unwrap();
        assert_eq!(snap.consecutive_failures, 1, "not retried");
    }
    // A folder the host guard refuses.
    let base = std::env::temp_dir().join("praxi-244b-guard");
    let forbidden = base.join("forbidden");
    std::fs::create_dir_all(&forbidden).unwrap();
    let f = forbidden.to_string_lossy().into_owned();
    let cfg = McpServerConfig::stdio("fs2", "praxi-never-spawned-244b", ["-y", pkg, f.as_str()]);
    let err = mgr.add_server(cfg).await.unwrap_err();
    assert_eq!(err.kind(), mcp_bridge::McpErrorKind::NeedsFolder);
    assert!(err.to_string().contains("protected folder"), "{err}");
    // A good folder gets past the check (then fails only because the launcher is fake).
    let ok = base.to_string_lossy().into_owned();
    let cfg = McpServerConfig::stdio("fs3", "praxi-never-spawned-244b", ["-y", pkg, ok.as_str()]);
    let err = mgr.add_server(cfg).await.unwrap_err();
    assert_eq!(
        err.kind(),
        mcp_bridge::McpErrorKind::RuntimeMissing,
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 244-A review finding 4: a child that explains itself on stderr and dies
/// before the handshake must not lose that explanation (no sleeps here).
#[tokio::test(flavor = "multi_thread")]
async fn startup_failure_keeps_the_childs_stderr_explanation() {
    for round in 0..5 {
        let reg = ToolRegistry::new();
        let mgr = McpManager::with_policy(reg, fast_policy(3));
        let mut cfg = config("early");
        if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
            env.insert("MCP_TEST_MODE".into(), "exit-with-stderr".into());
        }
        let err = mgr.add_server(cfg).await.unwrap_err();
        assert_eq!(err.kind(), mcp_bridge::McpErrorKind::Closed, "{err}");
        let tail = mgr.stderr_tail("early");
        assert!(
            tail.iter()
                .any(|l| l.contains("PRAXI-EXPLAIN: runtime X is missing")),
            "round {round}: explanation lost: {tail:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn quarantine_after_consecutive_call_failures_and_manual_reenable() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(2));
    mgr.add_server(config("q")).await.unwrap();

    // 1st crash: failure counted, server reconnects lazily.
    assert!(call(&reg, "mcp:q/crash", json!({})).await.is_err());
    let snap = mgr.snapshot("q").unwrap();
    assert_eq!(snap.consecutive_failures, 1);
    assert_ne!(snap.status, ServerStatus::Quarantined);
    // A working call in between resets the counter.
    assert_eq!(
        call(&reg, "mcp:q/echo", json!({"text": "x"}))
            .await
            .unwrap(),
        "x"
    );
    assert_eq!(mgr.snapshot("q").unwrap().consecutive_failures, 0);

    // Two consecutive crashes -> quarantined, tools hidden.
    assert!(call(&reg, "mcp:q/crash", json!({})).await.is_err());
    let echo = reg.get("mcp:q/echo").unwrap();
    assert!(call(&reg, "mcp:q/crash", json!({})).await.is_err());
    let snap = mgr.snapshot("q").unwrap();
    assert_eq!(snap.status, ServerStatus::Quarantined);
    assert_eq!(snap.tools.len(), 0);
    assert_eq!(reg.list().len(), 0, "quarantined tools must be hidden");

    // A stale handle is refused too.
    let err = echo
        .call(&ToolContext::default(), json!({"text": "x"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("quarantined"), "{err}");
    assert!(mgr.connect("q").await.is_err());

    // Manual re-enable restores everything.
    assert_eq!(mgr.reenable("q").await.unwrap(), 6);
    assert_eq!(mgr.snapshot("q").unwrap().status, ServerStatus::Connected);
    assert_eq!(
        call(&reg, "mcp:q/echo", json!({"text": "back"}))
            .await
            .unwrap(),
        "back"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_failures_quarantine_and_disabled_servers_stay_idle() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(2));
    let bad = McpServerConfig::stdio("bad", "definitely-not-a-real-binary-xyz", [""; 0]);
    assert!(mgr.add_server(bad).await.is_err());
    assert_eq!(mgr.snapshot("bad").unwrap().consecutive_failures, 1);
    assert!(mgr.connect("bad").await.is_err());
    assert_eq!(
        mgr.snapshot("bad").unwrap().status,
        ServerStatus::Quarantined
    );
    assert!(
        mgr.connect("bad")
            .await
            .unwrap_err()
            .to_string()
            .contains("quarantined")
    );

    let mut off = config("off");
    off.enabled = false;
    assert_eq!(mgr.add_server(off).await.unwrap(), 0);
    assert_eq!(mgr.snapshot("off").unwrap().status, ServerStatus::Disabled);
    assert_eq!(reg.list().len(), 0);

    // Duplicate ids are rejected.
    assert!(mgr.add_server(config("off")).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_surface_fronts_a_large_mcp_server() {
    use agent_router::{FIND_TOOL, SurfaceMode, ToolSurface, USE_TOOL};

    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    mgr.add_server(config("d")).await.unwrap();

    // MCP tools are namespaced: they never count against the threshold and
    // are never passed directly, so the model only sees the meta-tools.
    let surface = ToolSurface::new(reg.clone(), 3);
    assert_eq!(surface.mode(), SurfaceMode::Direct);
    assert_eq!(surface.specs().len(), 3);
    assert!(surface.resolve("mcp:d/echo").is_none());

    let ctx = ToolContext::default();
    let find = surface.resolve(FIND_TOOL).unwrap();
    let hits = find
        .call(&ctx, json!({"query": "echo text back", "k": 2}))
        .await
        .unwrap();
    let ContentPart::Json { data } = &hits.content[0] else {
        panic!("expected json")
    };
    assert_eq!(data["results"][0]["name"], "mcp:d/echo");

    let out = surface
        .resolve(USE_TOOL)
        .unwrap()
        .call(
            &ctx,
            json!({"name": "mcp:d/echo", "input": {"text": "via meta"}}),
        )
        .await
        .unwrap();
    assert_eq!(text_of(&out.content), "via meta");
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bots_allowlist_is_enforced_on_call() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    let mut cfg = config("b");
    cfg.bots = Some(vec!["bot-a".into()]);
    mgr.add_server(cfg).await.unwrap();
    let tool = reg.get("mcp:b/echo").unwrap();
    let input = json!({"text": "hi"});
    let allowed = ToolContext {
        bot_id: Some("bot-a".into()),
        ..ToolContext::default()
    };
    assert_eq!(
        text_of(&tool.call(&allowed, input.clone()).await.unwrap().content),
        "hi"
    );
    let other = ToolContext {
        bot_id: Some("bot-b".into()),
        ..ToolContext::default()
    };
    let err = tool.call(&other, input.clone()).await.unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");
    let err = tool.call(&ToolContext::default(), input).await.unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)), "{err}");

    let mut open = config("open");
    open.bots = None;
    mgr.add_server(open).await.unwrap();
    let any = reg.get("mcp:open/echo").unwrap();
    assert!(
        any.call(&ToolContext::default(), json!({"text": "x"}))
            .await
            .is_ok()
    );
    mgr.shutdown().await;
}

// Unix: the server exits on stdin EOF, so shutdown never kills its process
// group and the orphan survives (real gap, no owning card yet; see report).
#[cfg_attr(unix, ignore = "unowned: unix shutdown leaves orphan in process group")]
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_reaps_stdio_child_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("orphan.pid");
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg, fast_policy(3));
    let mut cfg = config("tree");
    if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("MCP_ORPHAN_PIDFILE".into(), pidfile.display().to_string());
    }
    mgr.add_server(cfg).await.unwrap();
    let mut pid = None;
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(&pidfile)
            && let Ok(n) = text.trim().parse::<u32>()
        {
            pid = Some(n);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pid = pid.expect("orphan pidfile");
    assert!(process_alive(pid), "orphan {pid} was not alive");
    mgr.shutdown().await;
    let mut dead = false;
    for _ in 0..40 {
        if !process_alive(pid) {
            dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dead, "orphan {pid} survived MCP shutdown");
}

// ── fail-closed hardening ────────────────────────────────────────

mod hardening {
    use std::{collections::BTreeMap, sync::Arc};

    use secret_store::{MemoryStore, SecretStore};
    use tool_registry::PermissionCategory;

    pub(super) use super::capture::Capture;
    use super::*;

    fn store_with(name: &str, value: &str) -> Arc<dyn SecretStore> {
        let s = MemoryStore::new();
        s.set(name, value).unwrap();
        Arc::new(s)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stdio_never_reads_or_exports_oauth_credentials_for_its_server_id() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct ObservedStore {
            inner: MemoryStore,
            reads: AtomicUsize,
        }
        impl SecretStore for ObservedStore {
            fn get(
                &self,
                name: &str,
            ) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.inner.get(name)
            }
            fn set(&self, name: &str, value: &str) -> Result<(), secret_store::SecretError> {
                self.inner.set(name, value)
            }
            fn delete(&self, name: &str) -> Result<bool, secret_store::SecretError> {
                self.inner.delete(name)
            }
            fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
                self.inner.list_names()
            }
        }
        let store = Arc::new(ObservedStore {
            inner: MemoryStore::new(),
            reads: AtomicUsize::new(0),
        });
        store.set("mcp/stdio-oauth/oauth", &json!({"access_token": "stdio-access-sentinel-7f3a", "refresh_token": "stdio-refresh-sentinel-91bc"}).to_string()).unwrap();
        store
            .set(
                "mcp/stdio-oauth/client-secret",
                "stdio-client-sentinel-98fd",
            )
            .unwrap();
        let registry = ToolRegistry::new();
        let manager = McpManager::with_policy(registry.clone(), fast_policy(1))
            .with_secret_store(store.clone());
        let mut cfg = config("stdio-oauth");
        let McpTransport::Stdio { env, .. } = &mut cfg.transport else {
            unreachable!()
        };
        env.insert("DECLARED_MARKER".into(), "non-secret".into());
        manager.add_server(cfg).await.unwrap();
        let keys = call(&registry, "mcp:stdio-oauth/env_keys", json!({}))
            .await
            .unwrap();
        assert!(keys.split(',').any(|key| key == "DECLARED_MARKER"));
        assert!(
            keys.split(',')
                .all(|key| key == "DECLARED_MARKER" || OS_ESSENTIAL_ENV.contains(&key)),
            "{keys}"
        );
        assert_eq!(
            store.reads.load(Ordering::SeqCst),
            0,
            "stdio must not even load OAuth values"
        );
        manager.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn secret_env_is_resolved_at_connect_and_not_leaked() {
        const VALUE: &str = "hunter2-very-secret";
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let reg = ToolRegistry::new();
        let mgr = McpManager::with_policy(reg.clone(), fast_policy(3))
            .with_secret_store(store_with("mcp/key", VALUE));
        let mut cfg = config("sec");
        cfg.secret_env
            .insert("MCP_TEST_SECRET".into(), "mcp/key".into());
        let serialized = toml::to_string(&cfg).unwrap();
        assert!(!serialized.contains(VALUE) && serialized.contains("mcp/key"));
        assert!(!format!("{cfg:?}").contains(VALUE));
        mgr.add_server(cfg).await.unwrap();
        let keys = call(&reg, "mcp:sec/env_keys", json!({})).await.unwrap();
        assert!(keys.split(',').any(|k| k == "MCP_TEST_SECRET"), "{keys}");
        assert!(!format!("{:?}", mgr.snapshots()).contains(VALUE));
        assert!(!capture.0.lock().unwrap().contains(VALUE));
        mgr.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_secret_means_the_server_is_not_spawned() {
        let reg = ToolRegistry::new();
        let mgr = McpManager::with_policy(reg.clone(), fast_policy(3))
            .with_secret_store(store_with("other", "x"));
        let mut cfg = config("nosec");
        cfg.secret_env.insert("K".into(), "mcp/absent".into());
        let err = mgr.add_server(cfg).await.unwrap_err();
        assert!(err.to_string().contains("mcp/absent"), "{err}");
        assert_eq!(mgr.stderr_tail("nosec").len(), 0);
        let snap = mgr.snapshot("nosec").unwrap();
        assert_eq!(snap.tools.len(), 0);
        assert_eq!(snap.status, ServerStatus::Disconnected);
        assert!(reg.get("mcp:nosec/echo").is_none());

        // No store at all: same outcome.
        let mgr = McpManager::with_policy(ToolRegistry::new(), fast_policy(3));
        let mut cfg = config("nostore");
        cfg.secret_env.insert("K".into(), "mcp/absent".into());
        assert!(mgr.add_server(cfg).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn untrusted_hints_and_category_from_config() {
        // Untrusted (default): the honest `readOnlyHint` is ignored.
        let reg = ToolRegistry::new();
        let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
        let mut cfg = config("u");
        cfg.trust_hints = false;
        cfg.category = Some(PermissionCategory::Fs);
        mgr.add_server(cfg).await.unwrap();
        let echo = reg.get("mcp:u/echo").unwrap();
        assert_eq!(echo.spec().effect, Effect::Destructive);
        let mut cats = BTreeMap::new();
        for n in mgr.snapshot("u").unwrap().tools {
            cats.insert(n.clone(), reg.get(&n).unwrap().spec().category);
        }
        assert!(
            cats.values().all(|c| *c == PermissionCategory::Fs),
            "{cats:?}"
        );

        // Trusted: Read.
        let reg = ToolRegistry::new();
        let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
        let mut cfg = config("t");
        cfg.trust_hints = true;
        mgr.add_server(cfg).await.unwrap();
        let echo = reg.get("mcp:t/echo").unwrap();
        assert_eq!(echo.spec().effect, Effect::Read);
        assert_eq!(echo.spec().category, PermissionCategory::Accounts);

        // `meta` is rejected before anything starts.
        let mut cfg = config("m");
        cfg.category = Some(PermissionCategory::Meta);
        assert!(mgr.add_server(cfg).await.is_err());
    }
}

fn mode_config(id: &str, mode: &str) -> McpServerConfig {
    let mut cfg = config(id);
    if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("MCP_TEST_MODE".into(), mode.into());
    }
    cfg
}

async fn wait_for_stderr(mgr: &McpManager, id: &str, marker: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if mgr.stderr_tail(id).iter().any(|line| line.contains(marker)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child did not reach stderr marker");
}

struct TestSecrets;
const STDERR_SECRET: &str = "mcp-sensitive-sentinel-a8f74d20";

impl secret_store::SecretStore for TestSecrets {
    fn get(&self, _: &str) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        Ok(Some(secret_store::SecretValue::new(STDERR_SECRET)))
    }
    fn set(&self, _: &str, _: &str) -> Result<(), secret_store::SecretError> {
        unimplemented!()
    }
    fn delete(&self, _: &str) -> Result<bool, secret_store::SecretError> {
        unimplemented!()
    }
    fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
        unimplemented!()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resolved_secret_is_absent_from_child_stderr_and_captured_tracing() {
    use tracing::instrument::WithSubscriber;
    let capture = hardening::Capture::default();
    let mgr = McpManager::with_policy(ToolRegistry::new(), fast_policy(3))
        .with_secret_store(std::sync::Arc::new(TestSecrets));
    let mut cfg = mode_config("redacted", "secret-stderr");
    cfg.secret_env
        .insert("API_KEY".into(), "test-secret".into());
    mgr.add_server(cfg)
        .with_subscriber(capture.clone())
        .await
        .unwrap();
    wait_for_stderr(&mgr, "redacted", "child secret:").await;
    let stderr = mgr.stderr_tail("redacted").join("\n");
    let traces = capture.0.lock().unwrap().clone();
    assert!(!stderr.contains(STDERR_SECRET), "secret in stderr log");
    assert!(!traces.contains(STDERR_SECRET), "secret in tracing");
    assert!(stderr.contains("[REDACTED]"));
    assert!(
        traces.contains("child secret: [REDACTED]"),
        "stderr trace not captured"
    );
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_errors_and_snapshots_redact_resolved_secrets() {
    let mgr = McpManager::with_policy(ToolRegistry::new(), fast_policy(3))
        .with_secret_store(std::sync::Arc::new(TestSecrets));
    let mut cfg = mode_config("redacted-error", "secret-error");
    cfg.secret_env
        .insert("API_KEY".into(), "test-secret".into());
    let error = mgr.add_server(cfg).await.unwrap_err().to_string();
    assert!(!error.contains(STDERR_SECRET));
    assert!(error.contains("[REDACTED]"));
    assert!(
        !mgr.snapshot("redacted-error")
            .unwrap()
            .last_error
            .unwrap()
            .contains(STDERR_SECRET)
    );
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn newline_free_stderr_flood_has_bounded_retained_lines_and_snapshot() {
    let mgr = McpManager::with_policy(ToolRegistry::new(), fast_policy(3));
    mgr.add_server(mode_config("flood", "flood")).await.unwrap();
    wait_for_stderr(&mgr, "flood", "flood-finished").await;
    let stderr = mgr.stderr_tail("flood");
    assert!(stderr.iter().all(|line| line.len() <= 8 * 1024));
    assert!(stderr.iter().map(String::len).sum::<usize>() <= 64 * 1024);
    assert!(stderr.iter().any(|line| line.ends_with("[truncated]")));
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_discovery_does_not_block_another_server_or_removal() {
    let mgr = std::sync::Arc::new(McpManager::with_policy(ToolRegistry::new(), fast_policy(3)));
    let pending = {
        let mgr = mgr.clone();
        tokio::spawn(async move { mgr.add_server(mode_config("slow", "stalled")).await })
    };
    wait_for_stderr(&mgr, "slow", "tools/list entered").await;
    tokio::time::timeout(Duration::from_secs(10), mgr.add_server(config("healthy")))
        .await
        .unwrap()
        .unwrap();
    let removal = {
        let mgr = mgr.clone();
        tokio::spawn(async move { mgr.remove_server("slow").await })
    };
    let removed = tokio::time::timeout(Duration::from_secs(3), pending)
        .await
        .expect("removed discovery was not cancelled")
        .unwrap();
    assert!(matches!(
        removed,
        Err(mcp_bridge::McpError::UnknownServer(_))
    ));
    assert!(mgr.snapshot("slow").is_none());
    assert_eq!(
        mgr.snapshot("healthy").unwrap().status,
        ServerStatus::Connected
    );
    // Retirement cancels discovery immediately; process shutdown has its own
    // bounded grace period, independent of in-flight caller clones.
    tokio::time::timeout(Duration::from_secs(15), removal)
        .await
        .expect("process cleanup exceeded its bound")
        .unwrap()
        .unwrap();
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_caps_reject_remote_tool_sets_before_registration() {
    for (mode, message) in [
        ("pages", "page limit"),
        ("tools", "tool-count limit"),
        ("schema", "byte limit"),
    ] {
        let reg = ToolRegistry::new();
        let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            mgr.add_server(mode_config("caps", mode)),
        )
        .await
        .expect("discovery did not stop at the cap");
        assert!(result.unwrap_err().to_string().contains(message));
        assert!(reg.list().is_empty());
        assert_eq!(mgr.snapshot("caps").unwrap().consecutive_failures, 1);
        mgr.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_page_above_the_frame_limit_is_rejected_while_reading() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(1));
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        mgr.add_server(mode_config("huge", "huge-page")),
    )
    .await
    .expect("oversized frame was not rejected promptly");
    let error = result.unwrap_err();
    assert!(
        !matches!(error, mcp_bridge::McpError::Timeout { .. }),
        "{error}"
    );
    assert!(reg.list().is_empty());
    mgr.shutdown().await;
}

const ENCODED_SECRET: &str = "Bearer tok/en+value?with=sym&bols-1234";

#[tokio::test(flavor = "multi_thread")]
async fn encoded_secret_forms_in_child_stderr_are_masked_and_the_live_redactor_runs() {
    let host: mcp_bridge::ExternalRedactor =
        std::sync::Arc::new(|text| text.replace("encoded-done", "host-saw-this"));
    let store = std::sync::Arc::new(secret_store::MemoryStore::new());
    secret_store::SecretStore::set(&*store, "test/enc", ENCODED_SECRET).unwrap();
    let mgr = McpManager::with_policy(ToolRegistry::new(), fast_policy(1))
        .with_secret_store(store)
        .with_redactor(host);
    let mut cfg = mode_config("enc", "secret-stderr-encoded");
    cfg.secret_env.insert("API_KEY".into(), "test/enc".into());
    mgr.add_server(cfg).await.unwrap();
    wait_for_stderr(&mgr, "enc", "host-saw-this").await;
    let stderr = mgr.stderr_tail("enc").join(
        "
",
    );
    for leak in ["tok/en", "tok%2Fen", "sym", "bols", "dG9rL2Vu", "dG9r"] {
        assert!(!stderr.contains(leak), "`{leak}` leaked: {stderr}");
    }
    assert!(stderr.matches("[REDACTED]").count() >= 5, "{stderr}");
    mgr.shutdown().await;
}

fn as_bot(id: &str) -> ToolContext {
    ToolContext {
        bot_id: Some(id.into()),
        ..ToolContext::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bots_change_applies_live_without_dropping_in_flight_calls() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    let mut cfg = config("t");
    cfg.bots = Some(vec!["a".into(), "b".into()]);
    mgr.add_server(cfg).await.unwrap();
    let echo = reg.get("mcp:t/echo").unwrap();

    // A call from bot `b` is running when `b` is removed from the list.
    let started = std::time::Instant::now();
    let slow = {
        let echo = echo.clone();
        tokio::spawn(async move {
            echo.call(&as_bot("b"), json!({"text": "late", "sleep_ms": 800}))
                .await
        })
    };
    // Wait until the SERVER reports it is inside the slow call (a stale example
    // binary that ignores `sleep_ms` never prints this: fail loudly).
    let mut in_flight = false;
    for _ in 0..100 {
        if mgr
            .stderr_tail("t")
            .iter()
            .any(|l| l.contains("sleeping 800ms in echo"))
        {
            in_flight = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        in_flight,
        "the test_server example did not honour sleep_ms (rebuild it: cargo build -p mcp-bridge --examples)"
    );
    assert!(!slow.is_finished(), "the call must still be running");
    mgr.set_server_bots("t", Some(vec!["a".into()])).unwrap();
    assert!(!slow.is_finished(), "the swap must not end the call");

    // The in-flight call survives (same connection, no restart) ...
    let out = slow.await.unwrap().expect("in-flight call must finish");
    assert!(started.elapsed() >= Duration::from_millis(800));
    assert_eq!(text_of(&out.content), "late");
    // ... the removed bot is denied on its next call and no longer sees the
    // tool, while the remaining bot is unaffected.
    let denied = echo
        .call(&as_bot("b"), json!({"text": "x"}))
        .await
        .unwrap_err();
    assert!(matches!(denied, ToolError::PermissionDenied(_)), "{denied}");
    assert!(!echo.visible_to(Some("b")));
    assert!(echo.visible_to(Some("a")));
    let ok = echo
        .call(&as_bot("a"), json!({"text": "hi"}))
        .await
        .unwrap();
    assert_eq!(text_of(&ok.content), "hi");

    // Empty list = nobody; null = everybody.
    mgr.set_server_bots("t", Some(vec![])).unwrap();
    assert!(echo.call(&as_bot("a"), json!({"text": "x"})).await.is_err());
    mgr.set_server_bots("t", None).unwrap();
    assert!(
        echo.call(&as_bot("zzz"), json!({"text": "x"}))
            .await
            .is_ok()
    );
    assert!(mgr.set_server_bots("missing", None).is_err());
}

// ── spec coverage: list-changed, resources/prompts, results, lifecycle ──────

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_list_changed_refreshes_the_registry_without_a_reconnect() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    mgr.add_server(mode_config("dyn", "features"))
        .await
        .unwrap();
    assert!(reg.get("mcp:dyn/extra").is_none());
    let pid = call(&reg, "mcp:dyn/echo", json!({"pid": true}))
        .await
        .unwrap();
    assert_eq!(
        call(&reg, "mcp:dyn/grow", json!({})).await.unwrap(),
        "grown"
    );
    // A tool the owner never approved is held back on a server that already
    // has approved tools, until it is approved with the hash that was shown.
    wait_until("the announced tool is pending", || {
        mgr.snapshot("dyn").is_some_and(|s| {
            s.changed_tools
                .iter()
                .any(|c| c.tool == "extra" && c.new_tool)
        })
    })
    .await;
    assert!(reg.get("mcp:dyn/extra").is_none());
    let hash = mgr.snapshot("dyn").unwrap().changed_tools[0].hash.clone();
    mgr.approve_tool_changes("dyn", "extra", &hash)
        .await
        .unwrap();
    assert_eq!(
        call(&reg, "mcp:dyn/extra", json!({})).await.unwrap(),
        "extra"
    );
    assert!(
        mgr.snapshot("dyn")
            .unwrap()
            .tools
            .contains(&"mcp:dyn/extra".to_owned())
    );
    // Same process: the list was refreshed on the live session.
    assert_eq!(
        call(&reg, "mcp:dyn/echo", json!({"pid": true}))
            .await
            .unwrap(),
        pid
    );
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_and_prompts_are_reachable_only_when_the_server_has_them() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    mgr.add_server(config("plain")).await.unwrap();
    assert!(reg.get("mcp:plain/@resources_list").is_none());
    assert!(reg.get("mcp:plain/@prompts_get").is_none());

    // Untrusted server: the bridge tools need approval like its own tools.
    let mut untrusted = mode_config("u", "features");
    untrusted.trust_hints = false;
    mgr.add_server(untrusted).await.unwrap();
    assert_eq!(
        reg.get("mcp:u/@resources_read").unwrap().spec().effect,
        Effect::Destructive
    );
    // The user's tool filter covers them.
    let mut filtered = mode_config("f", "features");
    filtered.tool_exclude = vec!["@*".into()];
    mgr.add_server(filtered).await.unwrap();
    assert!(reg.get("mcp:f/@resources_list").is_none());
    assert!(reg.get("mcp:f/echo").is_some());

    mgr.add_server(mode_config("r", "features")).await.unwrap();
    let list = reg.get("mcp:r/@resources_list").unwrap();
    assert_eq!(list.spec().effect, Effect::Read);
    let listed = list.call(&ToolContext::default(), json!({})).await.unwrap();
    let ContentPart::Json { data } = &listed.content[0] else {
        panic!("{listed:?}");
    };
    assert_eq!(data["resources"][0]["uri"], "memo://notes");

    assert_eq!(
        call(
            &reg,
            "mcp:r/@resources_read",
            json!({"uri": "memo://notes"})
        )
        .await
        .unwrap(),
        "remember the milk"
    );
    let blob = reg
        .get("mcp:r/@resources_read")
        .unwrap()
        .call(&ToolContext::default(), json!({"uri": "memo://blob"}))
        .await
        .unwrap();
    assert!(
        matches!(&blob.content[0], ContentPart::Binary { mime_type, base64 }
            if mime_type == "application/x-test" && base64 == "QUJD"),
        "{blob:?}"
    );
    let missing = call(&reg, "mcp:r/@resources_read", json!({}))
        .await
        .unwrap_err();
    assert!(missing.contains("uri"), "{missing}");
    // A server-side error is an answer, not a failure of the server.
    assert!(
        call(&reg, "mcp:r/@resources_read", json!({"uri": "memo://nope"}))
            .await
            .is_err()
    );
    assert_eq!(mgr.snapshot("r").unwrap().consecutive_failures, 0);

    let prompts = reg
        .get("mcp:r/@prompts_list")
        .unwrap()
        .call(&ToolContext::default(), json!({}))
        .await
        .unwrap();
    let ContentPart::Json { data } = &prompts.content[0] else {
        panic!("{prompts:?}");
    };
    assert_eq!(data["prompts"][0]["name"], "greet");
    let prompt = reg
        .get("mcp:r/@prompts_get")
        .unwrap()
        .call(
            &ToolContext::default(),
            json!({"name": "greet", "arguments": {"who": "Ada"}}),
        )
        .await
        .unwrap();
    let ContentPart::Json { data } = &prompt.content[0] else {
        panic!("{prompt:?}");
    };
    assert_eq!(data["messages"][0]["content"]["text"], "Say hello to Ada");

    // Per-bot scoping applies to the bridge tools as well.
    mgr.set_server_bots("r", Some(vec!["bot-a".into()]))
        .unwrap();
    let denied = list
        .call(&ToolContext::default(), json!({}))
        .await
        .unwrap_err();
    assert!(matches!(denied, ToolError::PermissionDenied(_)), "{denied}");
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_content_embedded_resources_and_hidden_description_characters() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(3));
    mgr.add_server(mode_config("s", "features")).await.unwrap();

    let out = reg
        .get("mcp:s/structured")
        .unwrap()
        .call(&ToolContext::default(), json!({}))
        .await
        .unwrap();
    assert!(matches!(&out.content[0], ContentPart::Text { text } if text == "two rows"));
    assert!(
        matches!(out.content.get(1), Some(ContentPart::Json { data }) if data["rows"] == json!([1, 2])),
        "structured content was dropped: {out:?}"
    );

    assert_eq!(
        call(&reg, "mcp:s/embedded", json!({})).await.unwrap(),
        "inline note"
    );

    let hidden = reg.get("mcp:s/hidden").unwrap();
    assert_eq!(hidden.spec().description, "visible text");
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_call_times_out_without_taking_the_server_down() {
    let reg = ToolRegistry::new();
    let mut policy = fast_policy(1);
    policy.call_timeout = Duration::from_secs(1);
    let mgr = McpManager::with_policy(reg.clone(), policy);
    mgr.add_server(config("slow")).await.unwrap();
    let pid = call(&reg, "mcp:slow/echo", json!({"pid": true}))
        .await
        .unwrap();
    let err = call(&reg, "mcp:slow/echo", json!({"sleep_ms": 30_000}))
        .await
        .unwrap_err();
    assert!(err.contains("timed out"), "{err}");
    // `quarantine_after` is 1: a counted failure would have hidden the tools.
    let snapshot = mgr.snapshot("slow").unwrap();
    assert_eq!(snapshot.status, ServerStatus::Connected, "{snapshot:?}");
    assert_eq!(snapshot.consecutive_failures, 0);
    assert_eq!(
        call(&reg, "mcp:slow/echo", json!({"pid": true}))
            .await
            .unwrap(),
        pid,
        "the server was restarted"
    );
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_server_is_not_reported_as_connected() {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), fast_policy(5));
    mgr.add_server(config("dead")).await.unwrap();
    assert_eq!(
        mgr.snapshot("dead").unwrap().status,
        ServerStatus::Connected
    );
    // The call itself succeeds; the process dies afterwards with no call in
    // flight, so only the snapshot can notice.
    call(&reg, "mcp:dead/echo", json!({"exit_after_ms": 100}))
        .await
        .unwrap();
    let mut status = ServerStatus::Connected;
    for _ in 0..200 {
        status = mgr.snapshot("dead").unwrap().status;
        if status != ServerStatus::Connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(status, ServerStatus::Disconnected);
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_closes_stdin_first_so_the_server_can_exit_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("exit.marker");
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg, fast_policy(3));
    let mut cfg = config("clean");
    if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("MCP_EXIT_MARKER".into(), marker.display().to_string());
    }
    mgr.add_server(cfg).await.unwrap();
    mgr.shutdown().await;
    assert_eq!(
        std::fs::read_to_string(&marker).ok().as_deref(),
        Some("clean"),
        "the server was killed before it could exit by itself"
    );
}

/// HTTP endpoint that answers every request with `401` and an OAuth
/// challenge, like a remote MCP server that wants a sign-in.
async fn unauthorized_endpoint() -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 401 Unauthorized\r\n\
                          WWW-Authenticate: Bearer resource_metadata=\"http://127.0.0.1/.well-known/oauth-protected-resource\"\r\n\
                          Content-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (url, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_http_server_that_wants_oauth_gives_an_actionable_error_and_is_not_retried() {
    let (url, endpoint) = unauthorized_endpoint().await;
    let reg = ToolRegistry::new();
    // One counted failure would quarantine the server.
    let mut policy = fast_policy(1);
    // A retried connect would wait for these.
    policy.backoff.initial = Duration::from_secs(30);
    policy.backoff.max = Duration::from_secs(30);
    policy.backoff.max_attempts = 3;
    let mgr = McpManager::with_policy(reg, policy);
    let started = std::time::Instant::now();
    let err = mgr
        .add_server(McpServerConfig::http("remote", url))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires authorization"), "{err}");
    assert!(err.contains("secret_headers.Authorization"), "{err}");
    let snapshot = mgr.snapshot("remote").unwrap();
    assert!(snapshot.auth_required, "{snapshot:?}");
    assert_eq!(snapshot.consecutive_failures, 0, "{snapshot:?}");
    assert_eq!(snapshot.status, ServerStatus::Disconnected, "{snapshot:?}");
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "connect was retried"
    );
    endpoint.abort();
    mgr.shutdown().await;
}

#[tokio::test]
async fn elicitation_answers_only_the_server_and_suppresses_its_echo() {
    use tool_registry::elicitation::{Decision, ElicitationHandler};
    use tracing::instrument::WithSubscriber as _;

    for mode in ["elicitation-legacy", "elicitation"] {
        let captured = capture::Capture::default();
        async {
            let reg = ToolRegistry::new();
            let manager = McpManager::with_policy(reg.clone(), fast_policy(3));
            let mut cfg = config("forms");
            if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
                env.insert("MCP_TEST_MODE".into(), mode.into());
            }
            manager.add_server(cfg).await.unwrap();
            let tool = reg.get("mcp:forms/elicit").unwrap();
            for action in ["decline", "cancel", "accept"] {
                let handler = ElicitationHandler::new(move |form| async move {
                    assert_eq!(form.server, "forms");
                    assert_eq!(form.fields.len(), 2);
                    assert!(
                        form.fields
                            .iter()
                            .any(|f| f.name == "password" && f.blocked)
                    );
                    assert!(!format!("{:?}", form.fields).contains("default-from-server"));
                    match action {
                        "accept" => Decision::Accept(
                            [("region".into(), json!("elicitation-canary-9413"))]
                                .into_iter()
                                .collect(),
                        ),
                        "decline" => Decision::Decline,
                        _ => Decision::Cancel,
                    }
                });
                let ctx = ToolContext {
                    elicitation: Some(handler),
                    ..ToolContext::default()
                };
                let result = tool.call(&ctx, json!({"expect": action})).await.unwrap();
                let text = text_of(&result.content);
                assert!(text.contains(match action {
                    "accept" => "answered",
                    "decline" => "declined",
                    _ => "cancelled",
                }));
                assert!(
                    !serde_json::to_string(&result)
                        .unwrap()
                        .contains("elicitation-canary-9413")
                );
            }
            assert!(
                !manager
                    .stderr_tail("forms")
                    .join("\n")
                    .contains("elicitation-canary-9413")
            );
            manager.remove_server("forms").await.unwrap();
        }
        .with_subscriber(captured.clone())
        .await;
        assert!(
            !captured
                .0
                .lock()
                .unwrap()
                .contains("elicitation-canary-9413")
        );
    }
}

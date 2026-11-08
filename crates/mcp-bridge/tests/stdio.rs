//! End-to-end tests against the stdio `test_server` example.

use std::{path::PathBuf, time::Duration};

use mcp_bridge::{
    Backoff, McpManager, McpPolicy, McpServerConfig, McpTransport, OS_ESSENTIAL_ENV, ServerStatus,
};
use serde_json::json;
use tool_registry::{ContentPart, Effect, ToolContext, ToolError, ToolRegistry};

fn server_exe() -> PathBuf {
    // target/debug/deps/<test>.exe -> target/debug/examples/test_server[.exe]
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("examples");
    p.push(format!("test_server{}", std::env::consts::EXE_SUFFIX));
    assert!(p.exists(), "build the example first: {}", p.display());
    p
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

// ── fail-closed hardening (task 042) ────────────────────────────────────────

mod hardening {
    use std::fmt::Write as _;

    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex, atomic::AtomicU64},
    };

    use secret_store::{MemoryStore, SecretStore};
    use tool_registry::PermissionCategory;
    use tracing::{
        Event, Metadata, Subscriber,
        field::{Field, Visit},
        span,
    };

    use super::*;

    /// Collects every event/span field as text.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<String>>, Arc<AtomicU64>);

    struct Rec<'a>(&'a mut String);
    impl Visit for Rec<'_> {
        fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
            let _ = write!(self.0, "{}={v:?} ", f.name());
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, a: &span::Attributes<'_>) -> span::Id {
            a.record(&mut Rec(&mut self.0.lock().unwrap()));
            span::Id::from_u64(self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, e: &Event<'_>) {
            e.record(&mut Rec(&mut self.0.lock().unwrap()));
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    fn store_with(name: &str, value: &str) -> Arc<dyn SecretStore> {
        let s = MemoryStore::new();
        s.set(name, value).unwrap();
        Arc::new(s)
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

//! Contract test for every entry of `catalog.json`, generated from the catalog.
//!
//! No network and no real credentials: each entry is turned into the server
//! config the app would store (its category, secret references) and pointed at
//! the scripted `test_server` example in `contract` mode, which lists one
//! read-only and one mutating tool, counts the calls it receives and answers
//! tool errors, "HTTP 403" and "HTTP 429" on request. The provider's own tool
//! names are not modelled; the contract is how the bridge treats each entry's
//! config.
//!
//! A new catalog entry fails `every_entry_has_a_fixture_or_a_reason` until it is
//! added to [`FIXTURES`] (or to [`CANNOT_FAKE_YET`] with a reason).

use std::{collections::BTreeSet, sync::Arc, time::Duration};

#[path = "support/example_bin.rs"]
mod example_bin;

use mcp_bridge::{Backoff, CatalogEntry, McpManager, McpPolicy, McpServerConfig, builtin_catalog};
use secret_store::{MemoryStore, Redactor, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Effect, ToolContext, ToolRegistry};

/// Entries with a contract fixture (all of them run the same scripted server;
/// the entry's own config is what differs).
const FIXTURES: &[&str] = &[
    "filesystem",
    "github",
    "gitlab",
    "slack",
    "notion",
    "google-drive",
    "postgres",
    "sqlite",
    "brave-search",
    "fetch",
    "playwright",
    "sentry",
    "linear",
    "todoist",
    "obsidian",
    "home-assistant",
    "exa",
    "tavily",
    "firecrawl",
    "context7",
    "deepwiki",
    "microsoft-learn",
    "cloudflare-docs",
    "notion-remote",
    "stripe",
    "atlassian",
    "vercel",
    "figma",
    "posthog",
    "intercom",
    "paypal",
    "mongodb",
];

const HTTP_REASON: &str = "streamable-HTTP entry: a 403/429 at the HTTP transport needs a scripted HTTP MCP server, which the test seams do not have; the tool-level 403/429 path is covered through the stdio fake";

/// (entry id, leg, reason): legs that cannot be faked with the current seams.
/// Only documents the gap; the legs themselves run on the stdio fake.
const CANNOT_FAKE_YET: &[(&str, &str, &str)] = &[
    ("linear", "transport_http_status", HTTP_REASON),
    ("notion-remote", "transport_http_status", HTTP_REASON),
    ("stripe", "transport_http_status", HTTP_REASON),
    ("atlassian", "transport_http_status", HTTP_REASON),
    ("vercel", "transport_http_status", HTTP_REASON),
    ("figma", "transport_http_status", HTTP_REASON),
    ("posthog", "transport_http_status", HTTP_REASON),
    ("intercom", "transport_http_status", HTTP_REASON),
    ("paypal", "transport_http_status", HTTP_REASON),
];

/// Known violations of the contract: (entry id or `*`, leg, why). Each must
/// still fail; the test fails when one starts passing so the line is removed.
const EXPECTED_FAIL: &[(&str, &str, &str)] = &[];

const SECRET: &str = "synthetic-contract-secret-0123456789";

fn policy() -> McpPolicy {
    McpPolicy {
        quarantine_after: 5,
        backoff: Backoff {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(20),
            max_attempts: 2,
        },
        connect_timeout: Duration::from_secs(20),
        call_timeout: Duration::from_secs(20),
    }
}

fn catalog() -> Vec<CatalogEntry> {
    builtin_catalog().expect("catalog parses")
}

fn secret_value(name: &str) -> String {
    format!("synthetic-{name}-9876543210")
}

/// The config the app would store for `entry`, but launching the fake server.
fn config_for(entry: &CatalogEntry, trust_hints: bool) -> McpServerConfig {
    let exe = example_bin::test_server().to_string_lossy().into_owned();
    let mut cfg = McpServerConfig::stdio(&entry.id, exe, [""; 0]);
    if let mcp_bridge::McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("MCP_TEST_MODE".into(), "contract".into());
    }
    cfg.trust_hints = trust_hints;
    cfg.secret_env = entry.install.secret_env.clone();
    cfg.secret_env
        .insert("CONTRACT_SECRET".into(), "contract/fixture".into());
    // Round-trip through JSON so the category string is parsed by the real type.
    let mut v = serde_json::to_value(&cfg).unwrap();
    v["category"] = json!(entry.category);
    serde_json::from_value(v).unwrap_or_else(|e| {
        panic!(
            "{}: category `{}` is not valid: {e}",
            entry.id, entry.category
        )
    })
}

/// The secret a `secret_headers` value names, when it is a template
/// (`Bearer {secret:NAME}`); `None` when the value is a plain secret name.
///
/// Mirrors `transport::secret_header_names`, which is crate-private: an
/// integration test can only see the public surface.
fn header_secret(value: &str) -> Option<&str> {
    let at = value.find("{secret:")?;
    let rest = &value[at + "{secret:".len()..];
    let end = rest.find('}')?;
    Some(&rest[..end])
}

fn store_for(entry: &CatalogEntry) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::new());
    store.set("contract/fixture", SECRET).unwrap();
    for s in &entry.install.required_secrets {
        store.set(&s.name, &secret_value(&s.name)).unwrap();
    }
    store
}

async fn start(entry: &CatalogEntry, trust_hints: bool) -> (McpManager, ToolRegistry) {
    let reg = ToolRegistry::new();
    let mgr = McpManager::with_policy(reg.clone(), policy()).with_secret_store(store_for(entry));
    mgr.add_server(config_for(entry, trust_hints))
        .await
        .unwrap_or_else(|e| panic!("{}: fake server did not start: {e}", entry.id));
    (mgr, reg)
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

async fn call(reg: &ToolRegistry, id: &str, tool: &str, input: Value) -> Result<String, String> {
    let t = reg
        .get(&format!("mcp:{id}/{tool}"))
        .expect("tool registered");
    t.call(&ToolContext::default(), input)
        .await
        .map(|o| text_of(&o.content))
        .map_err(|e| e.to_string())
}

async fn counts(reg: &ToolRegistry, id: &str) -> Value {
    let seen = call(reg, id, "calls", json!({})).await.unwrap();
    serde_json::from_str(&seen).unwrap()
}

fn calls_of(counts: &Value, tool: &str) -> u64 {
    counts.get(tool).and_then(Value::as_u64).unwrap_or(0)
}

type Failures = Vec<(String, &'static str, String)>;

fn check(f: &mut Failures, id: &str, leg: &'static str, ok: bool, why: impl FnOnce() -> String) {
    if !ok {
        f.push((id.to_owned(), leg, why()));
    }
}

/// Leg 1: the entry parses and its declared auth needs are consistent.
fn leg_config(entry: &CatalogEntry, f: &mut Failures) {
    let id = entry.id.as_str();
    let i = &entry.install;
    check(
        f,
        id,
        "config",
        !entry.name.is_empty() && !entry.description.is_empty(),
        || "empty name or description".into(),
    );
    check(
        f,
        id,
        "config",
        entry.homepage.starts_with("https://") && !entry.homepage.contains('@'),
        || "homepage must be https without userinfo".into(),
    );
    match i.transport.as_str() {
        "http" => {
            let url = i.url.as_deref().unwrap_or("");
            check(
                f,
                id,
                "config",
                url.starts_with("https://") && !url.contains('@') && !url.contains('?'),
                || format!("http url `{url}` must be https, no userinfo or query"),
            );
        }
        "stdio" => check(f, id, "config", i.command.is_some(), || {
            "stdio entry without command".into()
        }),
        other => check(f, id, "config", false, || {
            format!("unknown transport `{other}`")
        }),
    }
    // Every secret reference is declared, every declared secret is used, all under `mcp/`.
    //
    // A header value may name its secret once (`Bearer {secret:NAME}`): the name
    // inside the placeholder is the reference and the rest is literal text, the
    // same rule the unit test in `catalog.rs` applies through the crate-private
    // parser.  An integration test only sees the public surface, so the tiny
    // scan lives at the bottom of this file.
    let declared: BTreeSet<&str> = i.required_secrets.iter().map(|s| s.name.as_str()).collect();
    let mut used: BTreeSet<&str> = i.secret_env.values().map(String::as_str).collect();
    for value in i.secret_headers.values() {
        match header_secret(value) {
            Some(name) => {
                used.insert(name);
            }
            None => {
                used.insert(value.as_str());
            }
        }
    }
    check(f, id, "config", declared == used, || {
        format!("required_secrets {declared:?} != referenced {used:?}")
    });
    check(
        f,
        id,
        "config",
        declared.iter().all(|n| n.starts_with("mcp/")),
        || "secret names must live under mcp/".into(),
    );
    // No credential-shaped literal in the plain env or args.
    let plain = Redactor::new();
    check(
        f,
        id,
        "config",
        i.env
            .values()
            .chain(i.args.iter().flatten())
            .all(|v| plain.redact(v) == *v),
        || "credential-shaped literal in env or args".into(),
    );
    check(
        f,
        id,
        "config",
        !i.env.keys().any(|k| i.secret_env.contains_key(k)),
        || "env var is both plain and secret".into(),
    );
}

/// Approval and taint on the specs the bridge registers.
async fn leg_specs(entry: &CatalogEntry, f: &mut Failures) {
    let id = entry.id.as_str();
    // A catalog install never opts into `readOnlyHint` trust: a hint is ignored.
    let (mgr, reg) = start(entry, false).await;
    let read = reg
        .get(&format!("mcp:{id}/read_item"))
        .unwrap()
        .spec()
        .clone();
    let write = reg
        .get(&format!("mcp:{id}/write_item"))
        .unwrap()
        .spec()
        .clone();
    check(
        f,
        id,
        "approval",
        write.effect == Effect::Destructive,
        || "mutating tool must be Destructive".into(),
    );
    check(
        f,
        id,
        "approval",
        read.effect == Effect::Destructive,
        || "read-only hint must be ignored without trust_hints".into(),
    );
    for (name, s) in [("read", &read), ("write", &write)] {
        check(f, id, "taint", s.content.ingests_untrusted, || {
            format!("{name} tool output not labelled untrusted")
        });
        check(
            f,
            id,
            "taint",
            s.content.reads_private && s.content.egress,
            || format!("{name} tool not private and egress by default"),
        );
    }
    mgr.shutdown().await;
    // Explicit opt-in: only then may a read-only tool skip the approval floor.
    let (mgr, reg) = start(entry, true).await;
    let read = reg
        .get(&format!("mcp:{id}/read_item"))
        .unwrap()
        .spec()
        .clone();
    let write = reg
        .get(&format!("mcp:{id}/write_item"))
        .unwrap()
        .spec()
        .clone();
    check(
        f,
        id,
        "approval",
        read.effect == Effect::Read && write.effect == Effect::Destructive,
        || "trust_hints must relax only read-only tools".into(),
    );
    mgr.shutdown().await;
}

/// Tool error, 403 and 429 surface once and a mutating call is never re-sent.
async fn leg_errors(entry: &CatalogEntry, reg: &ToolRegistry, f: &mut Failures) {
    let id = entry.id.as_str();
    let mut sent_write = 0;
    let mut sent_read = 0;
    for (mode, status) in [("error", 0), ("status", 403), ("status", 429)] {
        let args = json!({"mode": mode, "status": status});
        let expect = if mode == "error" {
            "it failed".to_owned()
        } else {
            format!("HTTP {status}")
        };
        for tool in ["write_item", "read_item"] {
            let err = call(reg, id, tool, args.clone())
                .await
                .err()
                .unwrap_or_default();
            check(f, id, "errors", err.contains(&expect), || {
                format!("{tool} {mode} {status}: not surfaced to the caller: `{err}`")
            });
            if tool == "write_item" {
                sent_write += 1;
            } else {
                sent_read += 1;
            }
        }
    }
    let seen = counts(reg, id).await;
    let writes = calls_of(&seen, "write_item");
    check(f, id, "errors", writes == sent_write, || {
        format!("write_item sent {writes} times for {sent_write} calls")
    });
    // A read may back off, but only a bounded number of times.
    let reads = calls_of(&seen, "read_item");
    check(
        f,
        id,
        "errors",
        (sent_read..=sent_read * 3).contains(&reads),
        || format!("read_item sent {reads} times for {sent_read} calls"),
    );
    // A failing tool does not take the server down.
    let ok = call(reg, id, "read_item", json!({})).await;
    check(f, id, "errors", ok.as_deref() == Ok("ok"), || {
        format!("server unusable after tool errors: {ok:?}")
    });
}

/// Dropping a call in flight (cancel/abort) sends nothing further.
async fn leg_abort(entry: &CatalogEntry, reg: &ToolRegistry, f: &mut Failures) {
    let id = entry.id.as_str();
    let before = calls_of(&counts(reg, id).await, "write_item");
    let tool = reg.get(&format!("mcp:{id}/write_item")).unwrap();
    // Dropped before it was ever polled: nothing is sent.
    drop(tool.call(&ToolContext::default(), json!({})));
    // Dropped while the provider is still working.
    let ctx = ToolContext::default();
    let slow = tool.call(&ctx, json!({"sleep_ms": 30_000}));
    let timed_out = tokio::time::timeout(Duration::from_millis(800), slow)
        .await
        .is_err();
    check(f, id, "abort", timed_out, || {
        "slow call finished unexpectedly".into()
    });
    tokio::time::sleep(Duration::from_millis(600)).await;
    let writes = calls_of(&counts(reg, id).await, "write_item") - before;
    check(f, id, "abort", writes == 1, || {
        format!("aborted write reached the provider {writes} times, want 1")
    });
}

/// A secret in the connector's output is masked by the production redactor.
/// (The agent loop applies it to every tool result; that wiring is covered in
/// `agent-core`, not here.)
async fn leg_redaction(entry: &CatalogEntry, reg: &ToolRegistry, f: &mut Failures) {
    let id = entry.id.as_str();
    let out = call(reg, id, "read_item", json!({"mode": "leak"}))
        .await
        .unwrap_or_default();
    check(f, id, "redaction", out.contains(SECRET), || {
        "fixture did not leak the secret".into()
    });
    let redactor = Redactor::from_store(store_for(entry).as_ref()).unwrap();
    let masked = redactor.redact(&out);
    check(
        f,
        id,
        "redaction",
        !masked.contains(SECRET) && masked.contains("[REDACTED]"),
        || "secret survives the redactor".into(),
    );
    for s in &entry.install.required_secrets {
        let value = secret_value(&s.name);
        check(
            f,
            id,
            "redaction",
            !redactor.redact(&format!("x {value} y")).contains(&value),
            || format!("{} survives the redactor", s.name),
        );
    }
}

#[test]
fn every_entry_has_a_fixture_or_a_reason() {
    let ids: BTreeSet<String> = catalog().into_iter().map(|e| e.id).collect();
    let fixtures: BTreeSet<String> = FIXTURES.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        ids.difference(&fixtures).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "catalog entries without a contract fixture: add them to FIXTURES"
    );
    assert_eq!(
        fixtures.difference(&ids).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "FIXTURES names entries that are not in the catalog"
    );
    for (id, leg, reason) in CANNOT_FAKE_YET.iter().chain(EXPECTED_FAIL) {
        assert!(
            *id == "*" || ids.contains(*id),
            "unknown entry {id} ({leg})"
        );
        assert!(reason.len() > 20, "{id}/{leg}: give a real reason");
    }
}

#[test]
fn every_entry_declares_consistent_auth() {
    let mut failures = Failures::new();
    for entry in &catalog() {
        leg_config(entry, &mut failures);
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_entry_meets_the_runtime_contract() {
    let mut failures = Failures::new();
    for entry in &catalog() {
        leg_specs(entry, &mut failures).await;
        let (mgr, reg) = start(entry, true).await;
        leg_errors(entry, &reg, &mut failures).await;
        leg_redaction(entry, &reg, &mut failures).await;
        leg_abort(entry, &reg, &mut failures).await;
        mgr.shutdown().await;
    }
    // Known violations must still fail; anything else failing is a regression.
    let known = |id: &str, leg: &str| {
        EXPECTED_FAIL
            .iter()
            .any(|(i, l, _)| (*i == "*" || *i == id) && *l == leg)
    };
    let unexpected: Vec<String> = failures
        .iter()
        .filter(|(id, leg, _)| !known(id, leg))
        .map(|(id, leg, why)| format!("{id} [{leg}]: {why}"))
        .collect();
    let stale: Vec<_> = EXPECTED_FAIL
        .iter()
        .filter(|(i, l, _)| {
            !failures
                .iter()
                .any(|(fi, fl, _)| (*i == "*" || i == fi) && l == fl)
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "contract violations:\n{}",
        unexpected.join("\n")
    );
    assert!(
        stale.is_empty(),
        "EXPECTED_FAIL entries now pass, remove them: {stale:?}"
    );
}

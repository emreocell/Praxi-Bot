//! Contract test for every toolkit of `curated.json` (`popular`), generated from
//! the table. No network and no real key: one fake Composio server on loopback
//! per toolkit, scripted with one read, one write and one payment action.
//!
//! A new toolkit needs no edit here: it is picked up from the table. A toolkit
//! that cannot be faked goes into [`CANNOT_FAKE_YET`] with a reason; one that
//! violates a leg goes into [`EXPECTED_FAIL`] (the test fails when it starts
//! passing, so the line is removed).

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::json;
use tool_registry::{ToolContext, ToolError};
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

use super::{Harness, KEY, ctx, harness};
use crate::curated::Curated;

/// (toolkit slug, why it cannot be faked). Empty: every toolkit goes through
/// the same Composio HTTP API, so one scripted server covers all of them.
const CANNOT_FAKE_YET: &[(&str, &str)] = &[];

/// (toolkit slug or `*`, leg, why): known violations of the contract.
const EXPECTED_FAIL: &[(&str, &str, &str)] = &[];

type Failures = Vec<(String, &'static str, String)>;

fn check(f: &mut Failures, slug: &str, leg: &'static str, ok: bool, why: impl FnOnce() -> String) {
    if !ok {
        f.push((slug.to_owned(), leg, why()));
    }
}

fn upper(slug: &str) -> String {
    slug.to_ascii_uppercase()
}

async fn mount_toolkit(h: &Harness, slug: &str, status: &str) {
    h.server.reset().await;
    let up = upper(slug);
    Mock::given(method("GET"))
        .and(path("/toolkits"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"items": [{"slug": slug, "name": slug}]})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth_configs"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "ac_1"}]})))
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/connected_accounts/link"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "link_token": "lt_secret_link_token",
            "redirect_url": "https://connect.composio.dev/link/lk_1",
            "expires_at": "2030-01-01T00:00:00Z",
            "connected_account_id": "ca_secret_conn_1"})))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/connected_accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "ca_secret_conn_1", "status": status, "toolkit": {"slug": slug}}]})))
        .mount(&h.server)
        .await;
    let action = |name: &str| {
        let method = if name.starts_with("LIST") {
            "GET"
        } else {
            "POST"
        };
        json!({"slug": format!("{up}_{name}"), "description": name, "toolkit": {"slug": slug},
            "version": "20260901_00", "http_method": method,
            "input_parameters": {"type": "object", "properties": {"x": {"type": "string"}}}})
    };
    Mock::given(method("GET"))
        .and(path("/tools"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            action("LIST_ITEMS"),
            action("SEND_ITEM"),
            action("PAY_INVOICE")
        ]})))
        .mount(&h.server)
        .await;
}

/// A harness with `slug` connected and its two actions registered.
async fn connected(slug: &str) -> Harness {
    let h = harness().await;
    mount_toolkit(&h, slug, "INITIATED").await;
    h.svc.set_key(KEY).await.unwrap();
    h.svc.connect(slug).await.unwrap();
    mount_toolkit(&h, slug, "ACTIVE").await;
    h.svc.app_status(slug).await.unwrap();
    h
}

async fn execute_posts(h: &Harness) -> usize {
    h.server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().starts_with("/tools/execute/"))
        .count()
}

/// One approved call of `action` (`list_items` / `send_item`) of `slug`.
async fn run(h: &Harness, slug: &str, action: &str) -> Result<String, ToolError> {
    let t = h
        .registry
        .get(&format!("app:{slug}/{action}"))
        .unwrap_or_else(|| panic!("{slug}: tool {action} not registered"));
    let args = json!({"x": "v"});
    let c: ToolContext = ctx();
    t.approval_details(&c, &args).await?;
    t.call(&c, args).await.map(|o| format!("{:?}", o.content))
}

async fn script_execute(h: &Harness, slug: &str, response: ResponseTemplate) {
    mount_toolkit(h, slug, "ACTIVE").await;
    Mock::given(method("POST"))
        .and(path_regex("^/tools/execute/"))
        .respond_with(response)
        .mount(&h.server)
        .await;
}

fn leg_config(f: &mut Failures) {
    let curated = Curated::builtin();
    let mut seen = BTreeSet::new();
    for p in &curated.popular {
        let s = p.slug.as_str();
        check(f, s, "config", seen.insert(s.to_owned()), || {
            "duplicate slug".into()
        });
        check(f, s, "config", crate::surface::toolkit_allowed(s), || {
            "slug is refused by the control-plane boundary".into()
        });
        check(
            f,
            s,
            "config",
            !p.name.is_empty() && !p.description.is_empty(),
            || "empty name or description".into(),
        );
        check(
            f,
            s,
            "config",
            crate::GROUPS.contains(&p.group.as_str()),
            || format!("unknown group `{}`", p.group),
        );
    }
}

/// Approval follows the class: a read needs no card, a write follows the
/// level, money always asks and is its own permission category.
async fn leg_approval(slug: &str, f: &mut Failures) {
    let h = connected(slug).await;
    for (action, effect, category, asks) in [
        (
            "list_items",
            tool_registry::Effect::Read,
            tool_registry::PermissionCategory::Accounts,
            false,
        ),
        (
            "send_item",
            tool_registry::Effect::Write,
            tool_registry::PermissionCategory::Accounts,
            false,
        ),
        (
            "pay_invoice",
            tool_registry::Effect::Destructive,
            tool_registry::PermissionCategory::Money,
            true,
        ),
    ] {
        let t = h.registry.get(&format!("app:{slug}/{action}")).unwrap();
        check(f, slug, "approval", t.always_ask() == asks, || {
            format!("{action}: always_ask is not {asks}")
        });
        check(
            f,
            slug,
            "approval",
            t.spec().effect == effect && t.spec().category == category,
            || format!("{action}: wrong effect or category"),
        );
        check(f, slug, "taint", t.spec().content.ingests_untrusted, || {
            format!("{action} output not declared untrusted")
        });
    }
    // A money call without a fresh approval never reaches the provider.
    script_execute(&h, slug, ResponseTemplate::new(200)).await;
    let t = h.registry.get(&format!("app:{slug}/pay_invoice")).unwrap();
    let unapproved = t.call(&ctx(), json!({"x": "v"})).await;
    check(
        f,
        slug,
        "approval",
        unapproved.is_err() && execute_posts(&h).await == 0,
        || "money call without approval was sent".into(),
    );
}

async fn leg_toolkit(slug: &str, f: &mut Failures) {
    leg_approval(slug, f).await;

    // Tool error, 403, 429: surfaced once, sent once, for a mutating and a read action.
    let cases: [(&str, ResponseTemplate, &str); 3] = [
        (
            "tool error",
            ResponseTemplate::new(200).set_body_json(json!({"successful": false})),
            "action failed",
        ),
        ("403", ResponseTemplate::new(403), "rejected"),
        (
            "429",
            ResponseTemplate::new(429).insert_header("Retry-After", "7"),
            "retry after 7",
        ),
    ];
    for (name, response, expect) in cases {
        for action in ["send_item", "list_items"] {
            let h = connected(slug).await;
            script_execute(&h, slug, response.clone()).await;
            let err = run(&h, slug, action).await.err().map(|e| e.to_string());
            check(
                f,
                slug,
                "errors",
                err.as_deref().is_some_and(|e| e.contains(expect)),
                || format!("{action} {name}: surfaced as {err:?}"),
            );
            let sent = execute_posts(&h).await;
            check(f, slug, "errors", sent == 1, || {
                format!("{action} {name}: {sent} requests reached the provider, want 1")
            });
        }
    }

    // Abort: dropping the call mid-flight sends nothing more.
    let h = connected(slug).await;
    script_execute(
        &h,
        slug,
        ResponseTemplate::new(200).set_delay(Duration::from_secs(30)),
    )
    .await;
    let timed_out = tokio::time::timeout(Duration::from_millis(800), run(&h, slug, "send_item"))
        .await
        .is_err();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let sent = execute_posts(&h).await;
    check(f, slug, "abort", timed_out && sent == 1, || {
        format!("aborted call: timed_out={timed_out}, {sent} requests, want 1")
    });

    // Output: labelled untrusted, secrets masked by the production redactor.
    let h = connected(slug).await;
    script_execute(
        &h,
        slug,
        ResponseTemplate::new(200)
            .set_body_json(json!({"successful": true, "data": {"note": format!("token {KEY}")}})),
    )
    .await;
    match run(&h, slug, "list_items").await {
        Ok(out) => {
            check(f, slug, "taint", out.contains("Untrusted content"), || {
                "output carries no untrusted label".into()
            });
            check(f, slug, "redaction", out.contains(KEY), || {
                "fixture did not leak the key".into()
            });
            h.redactor.register(KEY);
            let masked = h.redactor.redact(&out);
            check(f, slug, "redaction", !masked.contains(KEY), || {
                "key survives the redactor".into()
            });
        }
        Err(e) => check(f, slug, "taint", false, || format!("read call failed: {e}")),
    }
}

#[test]
fn every_toolkit_is_well_formed() {
    let mut f = Failures::new();
    leg_config(&mut f);
    assert!(f.is_empty(), "{f:#?}");
    let curated = Curated::builtin();
    let slugs: BTreeSet<&str> = curated.popular.iter().map(|p| p.slug.as_str()).collect();
    for (slug, reason) in CANNOT_FAKE_YET {
        assert!(slugs.contains(slug) && reason.len() > 20, "{slug}");
    }
    for (slug, leg, why) in EXPECTED_FAIL {
        assert!(*slug == "*" || slugs.contains(slug), "{slug} {leg}");
        assert!(why.len() > 20, "{slug}/{leg}: give a real reason");
    }
}

#[tokio::test]
async fn every_toolkit_meets_the_runtime_contract() {
    let skip: BTreeSet<&str> = CANNOT_FAKE_YET.iter().map(|(s, _)| *s).collect();
    let mut f = Failures::new();
    for p in &Curated::builtin().popular {
        if !skip.contains(p.slug.as_str()) {
            leg_toolkit(&p.slug, &mut f).await;
        }
    }
    let known = |s: &str, leg: &str| {
        EXPECTED_FAIL
            .iter()
            .any(|(i, l, _)| (*i == "*" || *i == s) && *l == leg)
    };
    let unexpected: Vec<String> = f
        .iter()
        .filter(|(s, leg, _)| !known(s, leg))
        .map(|(s, leg, why)| format!("{s} [{leg}]: {why}"))
        .collect();
    let stale: Vec<_> = EXPECTED_FAIL
        .iter()
        .filter(|(i, l, _)| {
            !f.iter()
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

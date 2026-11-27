//! Owner-gated routes seen from every paired role: the administrator desktop of
//! a host is the owner there, nobody else is (card c860).
use super::host_admin::{Rig, peer_call, rig};
use super::*;
use crate::PeerRole;

const OWNER: &str = super::OWNER_KEY;

fn concrete(pattern: &str) -> String {
    pattern.replace("{}", "x").replace('*', "a/b")
}

/// Refused by the authority checks (not by the route's own validation).
fn refused(r: &Resp) -> bool {
    r.status == StatusCode::FORBIDDEN
        && (r.body.is_empty() // a HEAD answer has no body
            || matches!(
                r.json()["error"]["code"].as_str(),
                Some("forbidden" | "owner_key_required" | "host_admin_required" | "host_admin_forbidden")
            ))
}

fn code(r: &Resp) -> String {
    r.json()["error"]["code"].as_str().unwrap_or("").to_owned()
}

/// One request of a paired peer, with extra headers (a peer may try anything).
async fn peer_request(
    t: &T,
    who: (&str, PeerRole),
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Resp {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .extension(crate::PeerIdentity {
            key: who.0.into(),
            role: who.1,
        });
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = b
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    let r = crate::peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    let body = axum::body::to_bytes(r.into_body(), 1 << 20)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

/// What a paired device does for the owner every day, spelled out as concrete
/// routes (not derived from the tables, so a drifting table is noticed).
const DEVICE_TASKS: &[(&str, &str)] = &[
    ("POST", "/v1/approvals/x"),
    ("POST", "/v1/questions/x"),
    ("POST", "/v1/control/x"),
    ("POST", "/v1/control/takeover"),
    ("POST", "/v1/control/override"),
    ("POST", "/v1/control/recover"),
    ("POST", "/v1/control/release"),
    ("POST", "/v1/schedules"),
    ("PUT", "/v1/schedules/x"),
    ("DELETE", "/v1/schedules/x"),
    ("POST", "/v1/schedules/x/run-now"),
    ("POST", "/v1/schedules/x/occurrences/x/retry"),
    ("POST", "/v1/schedules/x/reclaim"),
    // (On a host the guard refuses every `/v1/host` write to every paired device; see below.)
    ("POST", "/v1/host/invite"),
    ("DELETE", "/v1/host/devices/x"),
    ("PUT", "/v1/host/devices/x/admin"),
    ("POST", "/v1/host/standby"),
    ("POST", "/v1/host/activate"),
    ("POST", "/v1/host/update"),
];

/// Further tasks of a paired desktop (not a phone).
const DESKTOP_TASKS: &[(&str, &str)] = &[
    ("POST", "/v1/runs/x/workflow-proposal"),
    ("POST", "/v1/workflows"),
    ("DELETE", "/v1/workflows/x"),
    ("POST", "/v1/workflows/x/run"),
    ("GET", "/v1/workflows"),
    ("GET", "/v1/workflows/x"),
    ("POST", "/v1/components/x/install"),
    ("GET", "/v1/email/channel"),
    ("GET", "/v1/email/channel/replies"),
    ("GET", "/v1/email/accounts/x/messages"),
    ("PUT", "/v1/composio/key"),
    ("DELETE", "/v1/composio/key"),
    ("POST", "/v1/composio/login"),
    ("DELETE", "/v1/composio/apps/x"),
    ("POST", "/v1/composio/apps/x/connect"),
    ("PUT", "/v1/composio/apps/x/enabled"),
];

fn is_task(list: &[(&str, &str)], method: &str, path: &str) -> bool {
    list.iter().any(|(m, p)| *m == method && *p == path)
}

#[test]
fn the_device_task_tables_only_name_owner_routes() {
    let owner: std::collections::HashSet<(&str, &str)> =
        crate::owner::OWNER_ROUTES.iter().copied().collect();
    for entry in crate::owner::PEER_OK_ROUTES
        .iter()
        .chain(crate::owner::PEER_OK_DESKTOP_ROUTES)
    {
        assert!(owner.contains(entry), "{entry:?} is not an owner route");
    }
    // The spelled-out lists above and the tables agree.
    assert_eq!(DEVICE_TASKS.len(), crate::owner::PEER_OK_ROUTES.len());
    assert_eq!(
        DESKTOP_TASKS.len(),
        crate::owner::PEER_OK_DESKTOP_ROUTES.len()
    );
    for (method, pattern) in crate::owner::PEER_OK_ROUTES {
        assert!(
            is_task(DEVICE_TASKS, method, &concrete(pattern)),
            "{pattern}"
        );
    }
    for (method, pattern) in crate::owner::PEER_OK_DESKTOP_ROUTES {
        assert!(
            is_task(DESKTOP_TASKS, method, &concrete(pattern)),
            "{pattern}"
        );
    }
}

/// The administrator desktop is the owner: every owner-gated route reaches its
/// handler, except the short never-list, which is refused.
#[tokio::test]
async fn the_admin_desktop_reaches_every_owner_route_except_the_never_list() {
    let r = rig().await;
    r.t.state.set_email(super::email::FakeEmail::new());
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    let mut never = 0;
    let mut turned_away = Vec::new();
    for (method, pattern) in crate::owner::OWNER_ROUTES {
        let path = concrete(pattern);
        let m: Method = method.parse().unwrap();
        r.t.state.leader().reset_admin_budget();
        let resp = peer_request(&r.t, admin, m.clone(), &path, &[], Some(json!({}))).await;
        if crate::peer::admin_desktop_never(&m, &path).is_some() {
            never += 1;
            assert!(
                refused(&resp),
                "{method} {path} must stay refused: {}",
                resp.status
            );
        } else if !path.starts_with("/v1/control/") && refused(&resp) {
            // (The screen takeover buttons are decided by the live-view grants of the device, not here.)
            turned_away.push(format!("{method} {path}: {} {}", resp.status, code(&resp)));
        }
    }
    assert!(
        turned_away.is_empty(),
        "the administrator desktop was refused: {turned_away:#?}"
    );
    // The never-list is a small part of the table (pairing, host links, a few more families).
    let total = crate::owner::OWNER_ROUTES.len();
    assert!(
        never > 0 && never * 5 < total,
        "{never} of {total} routes on the never-list"
    );
    r.net.shutdown().await;
}

/// A desktop the owner has not confirmed keeps its everyday tasks and is told how
/// to get confirmed for everything else; it is never silently let through.
#[tokio::test]
async fn an_unconfirmed_desktop_does_its_tasks_and_is_told_how_to_be_confirmed_for_the_rest() {
    let r = rig().await;
    r.t.state.set_email(super::email::FakeEmail::new());
    let legacy = (r.legacy_key.as_str(), PeerRole::Desktop);
    for (method, pattern) in crate::owner::OWNER_ROUTES {
        let path = concrete(pattern);
        let m: Method = method.parse().unwrap();
        if path.starts_with("/v1/control/") || path.starts_with("/v1/host/") {
            continue; // control: the device's live-view grants decide; /v1/host: refused to every peer on a host
        }
        let resp = peer_request(&r.t, legacy, m, &path, &[], Some(json!({}))).await;
        let task = is_task(DEVICE_TASKS, method, &path) || is_task(DESKTOP_TASKS, method, &path);
        if task {
            assert!(
                !refused(&resp),
                "{method} {path} is a desktop task: {}",
                code(&resp)
            );
        } else {
            assert!(
                refused(&resp),
                "{method} {path} reached an unconfirmed desktop"
            );
            // HEAD answers have no body; every other refusal names the way out or the never-list.
            if *method != "HEAD" {
                let c = code(&resp);
                assert!(
                    c == "host_admin_required" || c == "forbidden",
                    "{method} {path}: {c}"
                );
            }
        }
    }
    let told = peer_request(
        &r.t,
        legacy,
        Method::PUT,
        "/v1/policy",
        &[],
        Some(json!({"preset": "read-only"})),
    )
    .await;
    assert_eq!(code(&told), "host_admin_required");
    assert!(
        told.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains(crate::host_admin::CONFIRM_COMMAND)
    );
    r.net.shutdown().await;
}

/// A phone, a node and another host are never the owner, whatever key they hold.
#[tokio::test]
async fn phones_nodes_and_hosts_get_no_owner_route_beyond_their_device_tasks() {
    let r = rig().await;
    r.t.state.set_email(super::email::FakeEmail::new());
    for role in [PeerRole::Phone, PeerRole::Node, PeerRole::Host] {
        for (method, pattern) in crate::owner::OWNER_ROUTES {
            let path = concrete(pattern);
            if path.starts_with("/v1/control/") || path.starts_with("/v1/host/") {
                continue; // control: live-view grants decide; /v1/host: refused to every peer on a host
            }
            let m: Method = method.parse().unwrap();
            let resp = peer_request(
                &r.t,
                (r.admin_key.as_str(), role),
                m,
                &path,
                &[("x-praxi-owner-key", OWNER)],
                Some(json!({})),
            )
            .await;
            let allowed = role == PeerRole::Phone && is_task(DEVICE_TASKS, method, &path);
            assert_eq!(
                refused(&resp),
                !allowed,
                "{role:?} {method} {path}: {} {}",
                resp.status,
                code(&resp)
            );
        }
    }
    r.net.shutdown().await;
}

async fn local_get(r: &Rig, path: &str) -> Value {
    call(&r.t, Method::GET, path, None).await.json()
}

/// The owner's own complaints, end to end: policy, search, spending limit.
#[tokio::test]
async fn the_admin_desktop_changes_the_policy_searches_messages_and_sets_the_limit() {
    let r = rig().await;
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    let before = local_get(&r, "/v1/policy").await;

    let put = peer_request(
        &r.t,
        admin,
        Method::PUT,
        "/v1/policy",
        &[],
        Some(json!({"preset": "read-only"})),
    )
    .await;
    assert_eq!(put.status, StatusCode::OK, "{:?}", put.json());
    assert_ne!(
        local_get(&r, "/v1/policy").await,
        before,
        "the policy changed on the host"
    );

    // Message search: the overlay's request. It answers with a page, never `owner_key_required`.
    let search = peer_request(
        &r.t,
        admin,
        Method::GET,
        "/v1/chats/search?q=linux",
        &[],
        None,
    )
    .await;
    assert_eq!(search.status, StatusCode::OK, "{:?}", search.json());
    assert!(search.json()["results"].is_array());

    let limit = peer_request(
        &r.t,
        admin,
        Method::PUT,
        "/v1/spend-limit",
        &[],
        Some(json!({"limit_usd": 50.0})),
    )
    .await;
    assert_eq!(limit.status, StatusCode::OK, "{:?}", limit.json());

    // Every one of those was attributed: class, method, path, nothing else.
    let audit = r.t.state.storage().list_audit(r.t.state.user_id()).unwrap();
    let rows: Vec<_> = audit
        .iter()
        .filter(|a| a.action == "host.admin.settings")
        .collect();
    assert!(rows.iter().all(|a| a.actor.starts_with("desktop-admin:")));
    let classes: Vec<&str> = rows
        .iter()
        .filter_map(|a| a.detail["class"].as_str())
        .collect();
    assert!(classes.contains(&"policy"), "{classes:?}");
    assert!(classes.contains(&"spend-limit"), "{classes:?}");
    // The policy row also says how big the change was: coarse level before and after, nothing else.
    let policy_row = rows
        .iter()
        .find(|a| a.detail["level_after"].is_string())
        .expect("a policy row with the levels");
    assert_eq!(policy_row.detail["level_after"], "read-only");
    assert_ne!(
        policy_row.detail["level_before"],
        policy_row.detail["level_after"]
    );
    assert!(
        !format!("{audit:?}").contains("\"preset\""),
        "never the body"
    );

    // The same requests from an unconfirmed desktop change nothing and say how to be confirmed.
    let legacy = (r.legacy_key.as_str(), PeerRole::Desktop);
    for (method, path, body) in [
        (Method::PUT, "/v1/policy", Some(json!({"preset": "full"}))),
        (Method::GET, "/v1/chats/search?q=linux", None),
        (
            Method::PUT,
            "/v1/spend-limit",
            Some(json!({"limit_usd": 1.0})),
        ),
    ] {
        let resp = peer_request(&r.t, legacy, method.clone(), path, &[], body).await;
        assert_eq!(code(&resp), "host_admin_required", "{method} {path}");
    }
    r.net.shutdown().await;
}

/// GET and HEAD are classified like the other methods (axum serves HEAD for a GET handler).
#[tokio::test]
async fn get_and_head_of_an_owner_read_follow_the_same_rule() {
    let r = rig().await;
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    let legacy = (r.legacy_key.as_str(), PeerRole::Desktop);
    for path in ["/v1/audit", "/v1/logs/recent", "/v1/email/guard/settings"] {
        for method in [Method::GET, Method::HEAD] {
            let ok = peer_request(&r.t, admin, method.clone(), path, &[], None).await;
            assert!(!refused(&ok), "admin {method} {path}: {}", ok.status);
            let no = peer_request(&r.t, legacy, method.clone(), path, &[], None).await;
            assert!(refused(&no), "legacy {method} {path}: {}", no.status);
        }
    }
    // The host's own links are on the never-list for every method, HEAD included.
    for method in [Method::GET, Method::HEAD] {
        let resp = peer_request(&r.t, admin, method.clone(), "/v1/hosts", &[], None).await;
        assert!(refused(&resp), "admin {method} /v1/hosts");
    }
    r.net.shutdown().await;
}

/// Explicit never-list items stay refused for the administrator desktop, and the
/// refusals do not leave an audit row for a change that did not happen.
#[tokio::test]
async fn the_never_list_holds_for_the_admin_desktop_and_leaves_no_change_row() {
    let r = rig().await;
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    for (method, path) in [
        (Method::POST, "/v1/host/invite"),
        (Method::PUT, "/v1/host/devices/abc/admin"),
        (Method::DELETE, "/v1/host/devices/abc"),
        (Method::POST, "/v1/host/update"),
        (Method::POST, "/v1/host/import"),
        (Method::POST, "/v1/hosts/pair"),
        (Method::POST, "/v1/hosts/abc/copy-setup"),
        (Method::POST, "/v1/mcp-server/tokens"),
        (Method::POST, "/v1/doctor/repair/x"),
        (Method::POST, "/v1/channels/telegram/pair-code"),
        (Method::POST, "/v1/incoming-webhooks/abc"),
    ] {
        let resp = peer_request(&r.t, admin, method.clone(), path, &[], Some(json!({}))).await;
        assert_eq!(resp.status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    let audit = r.t.state.storage().list_audit(r.t.state.user_id()).unwrap();
    assert!(!audit.iter().any(|a| a.action == "host.admin.settings"));
    r.net.shutdown().await;
}

/// A peer never brings the owner key: a header it sends is dropped, so a handler
/// that asks for the owner still refuses a device that is not the owner.
#[tokio::test]
async fn a_peer_cannot_present_the_owner_key_itself() {
    // A plain (not host) daemon with a paired desktop: the older, wider reach, no administrator.
    let t = setup();
    let resp = peer_request(
        &t,
        ("some-desktop", PeerRole::Desktop),
        Method::GET,
        "/v1/chats/search?q=linux",
        &[("x-praxi-owner-key", OWNER)],
        None,
    )
    .await;
    assert_eq!(code(&resp), "owner_key_required", "{:?}", resp.json());
    // An unconfirmed desktop of a host, same header: refused, not let in.
    let r = rig().await;
    let resp = peer_request(
        &r.t,
        (r.legacy_key.as_str(), PeerRole::Desktop),
        Method::PUT,
        "/v1/policy",
        &[("x-praxi-owner-key", OWNER)],
        Some(json!({"preset": "full"})),
    )
    .await;
    assert_eq!(code(&resp), "host_admin_required");
    r.net.shutdown().await;
}

/// The owner's message while a tool approval is open skips the approval and starts the next
/// turn, also when the app talks to a REMOTE host as a paired desktop (not through the local
/// owner key): the host's administrator desktop is `peer_admin`, an unconfirmed one only queues.
#[tokio::test]
async fn a_message_from_the_admin_desktop_skips_the_pending_approval_on_a_host() {
    let r = rig().await;
    let (_, chat) = make_chat(&r.t).await;
    let path = format!("/v1/chats/{chat}/messages");
    let body = |text: &str| Some(json!({ "text": text, "mode": "queue" }));
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    let legacy = (r.legacy_key.as_str(), PeerRole::Desktop);

    let started = peer_request(
        &r.t,
        admin,
        Method::POST,
        &path,
        &[],
        body("call:danger_run {}"),
    )
    .await;
    assert_eq!(started.status, StatusCode::ACCEPTED, "{:?}", started.json());
    let run = started.json()["run_id"].as_str().unwrap().to_owned();
    let (call_id, _) = pending_call(&r.t).await;
    wait_state(&r.t, &run, "waiting_approval").await;

    // An unconfirmed desktop is not the owner's own app: its message waits, whatever hint it sends.
    let waits = peer_request(
        &r.t,
        legacy,
        Method::POST,
        &path,
        &[
            ("x-praxi-message-source", "local_ui"),
            ("x-praxi-owner-key", OWNER),
        ],
        body("later"),
    )
    .await;
    assert_eq!(waits.json()["delivery"], "queued", "{:?}", waits.json());
    wait_state(&r.t, &run, "waiting_approval").await;

    // The administrator desktop skips the card: denied, the run stops, the message starts the next turn.
    let sent = peer_request(
        &r.t,
        admin,
        Method::POST,
        &path,
        &[],
        body("do something else"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{:?}", sent.json());
    assert_eq!(sent.json()["delivery"], "redirected", "{:?}", sent.json());
    wait_state(&r.t, &run, "cancelled").await;
    let all = call(
        &r.t,
        Method::GET,
        &format!("/v1/approvals?run_id={run}"),
        None,
    )
    .await
    .json();
    assert_eq!(all[0]["decision"], "denied", "{all}");
    let late = call(
        &r.t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "once"})),
    )
    .await;
    assert_eq!(late.status, StatusCode::NOT_FOUND);
    r.net.shutdown().await;
}

#[tokio::test]
async fn peer_call_helper_still_reaches_the_same_router() {
    // Guards the shared helper this file builds on: a plain read passes for a confirmed desktop.
    let r = rig().await;
    let ok = peer_call(
        &r.t,
        Some((r.admin_key.as_str(), PeerRole::Desktop)),
        Method::GET,
        "/v1/health",
        None,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
    r.net.shutdown().await;
}

/// Revealing a stored secret never works over the link, also for the administrator desktop, and
/// the refusal leaves no `fill_secret.reveal` row (it never reached the vault).
#[tokio::test]
async fn the_admin_desktop_cannot_reveal_a_stored_secret() {
    let r = rig().await;
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    let resp = peer_request(
        &r.t,
        admin,
        Method::POST,
        "/v1/fill-secrets/reveal",
        &[("x-praxi-owner-key", OWNER)],
        Some(json!({"name": "site/login", "confirm": true})),
    )
    .await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN, "{:?}", resp.json());
    let audit = r.t.state.storage().list_audit(r.t.state.user_id()).unwrap();
    assert!(!audit.iter().any(|a| a.action == "fill_secret.reveal"));
    assert!(!audit.iter().any(|a| a.action == "host.admin.settings"));
    r.net.shutdown().await;
}

/// Reads of sensitive collections by the administrator desktop are attributed (path and method).
#[tokio::test]
async fn an_admin_read_of_a_sensitive_collection_leaves_an_audit_row() {
    let r = rig().await;
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    for path in [
        "/v1/audit/export",
        "/v1/usage/export",
        "/v1/email/guard/send-log.csv",
    ] {
        peer_request(&r.t, admin, Method::GET, path, &[], None).await;
    }
    // An ordinary read is not a row.
    peer_request(&r.t, admin, Method::GET, "/v1/policy", &[], None).await;
    let audit = r.t.state.storage().list_audit(r.t.state.user_id()).unwrap();
    let paths: Vec<&str> = audit
        .iter()
        .filter(|a| a.action == "host.admin.settings")
        .filter_map(|a| a.detail["path"].as_str())
        .collect();
    for path in [
        "/v1/audit/export",
        "/v1/usage/export",
        "/v1/email/guard/send-log.csv",
    ] {
        assert!(paths.contains(&path), "{path} in {paths:?}");
    }
    assert!(!paths.contains(&"/v1/policy"));
    r.net.shutdown().await;
}

/// A mission belongs to the owner: a paired phone or an unconfirmed desktop can neither read
/// it nor write into its chat or touch its sessions; the administrator desktop (the owner of
/// the host) can.
#[tokio::test]
async fn a_paired_device_cannot_read_or_steer_a_mission_but_the_admin_desktop_can() {
    let r = rig().await;
    r.t.state.enable_missions().unwrap();
    let bot = {
        let created = super::call(
            &r.t,
            Method::POST,
            "/v1/bots",
            Some(json!({"profile_toml": super::PROFILE, "name": "Ada"})),
        )
        .await;
        created.json()["id"].as_str().unwrap().to_owned()
    };
    let (mission, chat, run) = super::missions::seed_mission(&r.t, &bot);
    let (chat, run) = (chat.0.to_string(), run.0.to_string());
    let probes = [
        (Method::GET, format!("/v1/missions/{mission}"), None),
        (Method::GET, format!("/v1/chats/{chat}/messages"), None),
        (
            Method::POST,
            format!("/v1/chats/{chat}/messages"),
            Some(json!({"text": "do something else", "mode": "steer"})),
        ),
        (Method::DELETE, format!("/v1/chats/{chat}"), None),
        (Method::POST, format!("/v1/runs/{run}/resume"), None),
        (Method::POST, format!("/v1/runs/{run}/cancel"), None),
    ];
    for who in [
        (r.legacy_key.as_str(), PeerRole::Desktop),
        (r.admin_key.as_str(), PeerRole::Phone),
    ] {
        for (m, path, body) in &probes {
            // A peer may try the owner key header too: the guard strips it.
            let resp = peer_request(
                &r.t,
                who,
                m.clone(),
                path,
                &[("x-praxi-owner-key", OWNER)],
                body.clone(),
            )
            .await;
            assert!(
                refused(&resp),
                "{:?} {m} {path} reached a mission: {} {}",
                who.1,
                resp.status,
                code(&resp)
            );
        }
    }
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    for (m, path, body) in &probes[..2] {
        r.t.state.leader().reset_admin_budget();
        let resp = peer_request(&r.t, admin, m.clone(), path, &[], body.clone()).await;
        assert_eq!(resp.status, StatusCode::OK, "{m} {path}: {:?}", resp.json());
    }
    r.net.shutdown().await;
}

/// The administrator desktop can see account identity and manage app access.
/// A desktop without host administration must complete owner confirmation.
#[tokio::test]
async fn app_management_and_identity_routes_need_the_confirmed_admin_desktop_on_a_host() {
    let r = rig().await;
    let legacy = (r.legacy_key.as_str(), PeerRole::Desktop);
    let admin = (r.admin_key.as_str(), PeerRole::Desktop);
    for (method, path, body) in [
        (Method::GET, "/v1/composio", None),
        (Method::GET, "/v1/composio/apps", None),
        (Method::GET, "/v1/composio/apps/github", None),
        (Method::GET, "/v1/composio/toolkits?search=git", None),
        (Method::GET, "/v1/composio/toolkits/github/actions", None),
        (Method::GET, "/v1/composio/logos/github", None),
        (Method::GET, "/v1/composio/login", None),
        (
            Method::PUT,
            "/v1/composio/apps/github/bots",
            Some(json!({"bots": null})),
        ),
        (Method::POST, "/v1/composio/apps/github/test", None),
    ] {
        let resp = peer_request(&r.t, legacy, method.clone(), path, &[], body.clone()).await;
        assert!(refused(&resp), "{method} {path}: {}", resp.status);
        assert_eq!(code(&resp), "host_admin_required", "{method} {path}");
        r.t.state.leader().reset_admin_budget();
        let resp = peer_request(&r.t, admin, method.clone(), path, &[], body).await;
        assert_ne!(code(&resp), "host_admin_required", "{method} {path}");
        assert!(
            !refused(&resp),
            "{method} {path}: {} {}",
            resp.status,
            code(&resp)
        );
    }
    r.net.shutdown().await;
}

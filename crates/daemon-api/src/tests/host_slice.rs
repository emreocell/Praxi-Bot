//! Card 252: what a paired Desktop may do on an always-on host, what stays
//! refused, persistent screen grants and the local-only `/v1/hosts` routes.
use super::*;
use crate::host::{HostConfig, HostMode, HostState};
use crate::host_links::{
    HostLinkConnection, HostLinkError, HostLinkInfo, HostLinkState, HostLinks,
};

const PEER: &str = "peerkey1";

fn host_daemon() -> T {
    setup_with(|data| {
        HostConfig {
            mode: HostMode::Host,
            state: HostState::Active,
            ..HostConfig::default()
        }
        .save(data)
        .unwrap();
    })
}

async fn peer_call(
    t: &T,
    role: crate::PeerRole,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Resp {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    req.extensions_mut().insert(crate::PeerIdentity {
        key: PEER.into(),
        role,
    });
    let resp = crate::peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

const ROLES: [crate::PeerRole; 4] = [
    crate::PeerRole::Desktop,
    crate::PeerRole::Phone,
    crate::PeerRole::Node,
    crate::PeerRole::Host,
];

fn count_audit(t: &T, action: &str) -> usize {
    t.state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .iter()
        .filter(|a| a.action == action)
        .count()
}

#[tokio::test]
async fn a_desktop_peer_on_a_host_cannot_administer_credentials_tools_or_pairing() {
    let t = host_daemon();
    let (bot, _) = make_chat(&t).await;
    let body = Some(json!({"value": "x"}));
    let cases: Vec<(Method, String)> = vec![
        (Method::POST, "/v1/secrets/provider/key".into()),
        (Method::POST, "/v1/secrets//provider/key".into()),
        (Method::POST, "/v1/secrets%2Fprovider%2Fkey".into()),
        (Method::POST, "/v1/secrets/provider/key/".into()),
        (Method::PUT, "/v1/secrets/provider/key".into()),
        (Method::POST, "/v1/mcp/servers".into()),
        (Method::POST, "/v1/mcp/servers/".into()),
        (Method::PUT, "/v1/mcp/servers/x".into()),
        (Method::DELETE, "/v1/mcp/servers/x".into()),
        (Method::DELETE, "//v1//mcp//servers//x".into()),
        (Method::POST, "/v1/mcp/servers/x/reconnect".into()),
        (Method::POST, "/v1/mcp%2Fservers".into()),
        (Method::POST, "/v1/skills".into()),
        (Method::PUT, "/v1/skills/".into()),
        (Method::PUT, format!("/v1/bots/{bot}/skills")),
        (Method::PUT, format!("/v1/bots/{bot}/skills/")),
        (Method::POST, "/v1/email/accounts".into()),
        (Method::PUT, "/v1/email/accounts/work".into()),
        (Method::DELETE, "/v1/email/accounts/work".into()),
        (Method::POST, "/v1/email/accounts/work/send-test".into()),
        (Method::PUT, "/v1/web/search".into()),
        (Method::PUT, "/v1/web/search/".into()),
        (Method::POST, "/v1/web/search/test".into()),
        (Method::POST, "/v1/channels/telegram/pair-code".into()),
        (Method::POST, "/v1/channels/telegram//pair-code".into()),
        (Method::POST, "/v1/host/import".into()),
        (Method::POST, "/v1/host/import/".into()),
        (Method::POST, "//v1/host//handover".into()),
        (Method::POST, "/v1/host%2Fhandover".into()),
        (Method::POST, "/v1/providers/cli/enable".into()),
        (Method::POST, "/v1/providers/reload".into()),
        (Method::GET, "/v1/doctor".into()),
    ];
    for (method, path) in &cases {
        let r = peer_call(
            &t,
            crate::PeerRole::Desktop,
            method.clone(),
            path,
            body.clone(),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "desktop {method} {path}");
    }
}

/// The dictation service address and key are host credentials: a desktop peer may
/// not repoint dictation audio at another service, but may still dictate.
#[tokio::test]
async fn a_desktop_peer_on_a_host_cannot_change_dictation_but_can_dictate() {
    let t = host_daemon();
    for path in ["/v1/voice", "/v1/voice/", "//v1//voice"] {
        let r = peer_call(
            &t,
            crate::PeerRole::Desktop,
            Method::PUT,
            path,
            Some(json!({"stt_url": "https://example.invalid/v1"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "desktop PUT {path}");
    }
    let r = peer_call(
        &t,
        crate::PeerRole::Desktop,
        Method::POST,
        "/v1/voice/transcribe",
        Some(json!({"audio_base64": "AAAA", "mime": "audio/webm"})),
    )
    .await;
    assert_ne!(
        r.status,
        StatusCode::FORBIDDEN,
        "dictation stays allowed: {:?}",
        r.json()
    );
}

#[tokio::test]
async fn only_a_desktop_peer_may_read_mailbox_headers() {
    let t = host_daemon();
    for role in [crate::PeerRole::Phone, crate::PeerRole::Node] {
        let r = peer_call(
            &t,
            role,
            Method::GET,
            "/v1/email/accounts/work/messages",
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?}");
    }
    // A desktop passes the guard (no embedder in this fixture, so 503 rather than 403).
    let r = peer_call(
        &t,
        crate::PeerRole::Desktop,
        Method::GET,
        "/v1/email/accounts/work/messages",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn a_desktop_peer_on_a_host_still_operates_bots_chats_and_runs() {
    let t = host_daemon();
    let (bot, chat) = make_chat(&t).await;
    for (method, path, body) in [
        (Method::GET, "/v1/bots".to_owned(), None),
        (Method::GET, "/v1/chats".to_owned(), None),
        (Method::GET, "/v1/runs".to_owned(), None),
        (Method::GET, "/v1/providers".to_owned(), None),
        (Method::GET, "/v1/host".to_owned(), None),
        (
            Method::POST,
            "/v1/chats".to_owned(),
            Some(json!({"bot_id": bot})),
        ),
        (
            Method::POST,
            format!("/v1/chats/{chat}/messages"),
            Some(json!({"text": "hi", "mode": "queue"})),
        ),
    ] {
        let r = peer_call(&t, crate::PeerRole::Desktop, method.clone(), &path, body).await;
        assert_ne!(r.status, StatusCode::FORBIDDEN, "desktop {method} {path}");
        assert!(
            r.status.is_success(),
            "desktop {method} {path}: {:?}",
            r.json()
        );
    }
}

#[tokio::test]
async fn on_a_standalone_daemon_the_desktop_rules_are_unchanged() {
    // The host-only denials must not leak into the PC daemon: a paired
    // Desktop keeps its skill access there (see `tests/skills.rs`).
    let t = setup();
    let (bot, _) = make_chat(&t).await;
    let r = peer_call(
        &t,
        crate::PeerRole::Desktop,
        Method::PUT,
        &format!("/v1/bots/{bot}/skills"),
        Some(json!({"skills": []})),
    )
    .await;
    assert_ne!(r.status, StatusCode::FORBIDDEN, "{:?}", r.json());
}

#[tokio::test]
async fn remote_chat_start_is_audited_for_phone_and_desktop_and_refused_for_node_and_host() {
    let t = host_daemon();
    let (bot, chat) = make_chat(&t).await;
    let local_before = count_audit(&t, "peer.chat_requested");
    assert_eq!(local_before, 0, "local use is not a peer action");
    for role in ROLES {
        for (path, body) in [
            ("/v1/chats".to_owned(), json!({"bot_id": bot})),
            (
                format!("/v1/chats/{chat}/messages"),
                json!({"text": "hi", "mode": "queue"}),
            ),
        ] {
            let before = count_audit(&t, "peer.chat_requested");
            let r = peer_call(&t, role, Method::POST, &path, Some(body)).await;
            let after = count_audit(&t, "peer.chat_requested");
            match role {
                crate::PeerRole::Desktop | crate::PeerRole::Phone => {
                    assert!(r.status.is_success(), "{role:?} {path}: {:?}", r.json());
                    assert_eq!(after, before + 1, "{role:?} {path} must be audited");
                }
                _ => {
                    assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} {path}");
                    assert_eq!(after, before, "{role:?} {path} left a row");
                }
            }
        }
    }
    let audit = t.state.storage().list_audit(t.state.user_id()).unwrap();
    let row = audit
        .iter()
        .find(|a| a.action == "peer.chat_requested" && a.actor.starts_with("desktop:"))
        .expect("desktop row");
    assert!(row.detail.to_string().contains(PEER));
}

#[tokio::test]
async fn screen_routes_need_a_local_grant_and_never_open_for_node_or_host() {
    let t = host_daemon();
    // No grant yet: even a Desktop sees nothing and cannot take over.
    let r = peer_call(
        &t,
        crate::PeerRole::Desktop,
        Method::GET,
        "/v1/view/targets",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "ungranted targets");
    let take = json!({"session_id": new_id(), "request_id": new_id()});
    let r = peer_call(
        &t,
        crate::PeerRole::Desktop,
        Method::POST,
        "/v1/control/takeover",
        Some(take.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // A peer cannot grant itself anything: the privilege route is local only.
    let self_grant = json!({"view": true, "input": true});
    for role in ROLES {
        let r = peer_call(
            &t,
            role,
            Method::PUT,
            &format!("/v1/view/privileges/{PEER}"),
            Some(self_grant.clone()),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} self grant");
    }
    // The local user grants view only.
    let g = call(
        &t,
        Method::PUT,
        &format!("/v1/view/privileges/{PEER}"),
        Some(json!({"view": true, "input": false})),
    )
    .await;
    assert_eq!(g.status, StatusCode::NO_CONTENT);
    let r = peer_call(
        &t,
        crate::PeerRole::Desktop,
        Method::GET,
        "/v1/view/targets",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // Node and host roles never reach the screen API, grant or not.
    for role in [crate::PeerRole::Node, crate::PeerRole::Host] {
        for (method, path) in [
            (Method::GET, "/v1/view/targets"),
            (Method::POST, "/v1/view/sessions"),
            (Method::POST, "/v1/control/takeover"),
            (Method::POST, "/v1/control/release"),
        ] {
            let r = peer_call(&t, role, method.clone(), path, Some(json!({}))).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} {method} {path}");
        }
    }
}

fn privileges_file(t: &T) -> Value {
    let bytes = std::fs::read(t.state.data_dir().join("view-privileges.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn screen_grants_are_persisted_and_revocation_removes_them() {
    let t = host_daemon();
    let path = format!("/v1/view/privileges/{PEER}");
    let r = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"view": true, "input": true})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let file = privileges_file(&t);
    assert_eq!(file[format!("peer:{PEER}")]["view"], true);
    assert_eq!(file[format!("peer:{PEER}")]["input"], true);
    // Revoking removes the entry: nothing keeps a stale grant on disk.
    let r = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"view": false, "input": false})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(privileges_file(&t).as_object().unwrap().is_empty());
}

#[tokio::test]
async fn loading_grants_applies_well_formed_peer_entries_only() {
    let t = setup_with(|data| {
        std::fs::write(
            data.join("view-privileges.json"),
            json!({
                "peer:goodkey1": {"view": true, "input": true},
                "peer:viewonly": {"view": true, "input": false},
                "peer:../evil": {"view": true, "input": true},
                "local-ui:someone": {"view": true, "input": true},
                "peer:noview": {"view": false, "input": true},
            })
            .to_string(),
        )
        .unwrap();
    });
    assert_eq!(t.state.load_screen_privileges().await, 2);
    let allowed = |key: &str, input: bool| {
        let actor = crate::live_view::Actor::peer(&crate::PeerIdentity {
            key: key.into(),
            role: crate::PeerRole::Desktop,
        });
        t.state.live_view().allowed(&actor, input)
    };
    assert!(allowed("goodkey1", true));
    assert!(allowed("viewonly", false));
    assert!(!allowed("viewonly", true));
    assert!(!allowed("noview", false));
    assert!(!allowed("../evil", false));
    // A device that is no longer paired loses its persisted grant.
    t.state
        .reconcile_paired_peers(&std::collections::BTreeSet::from([
            "peer:viewonly".to_owned()
        ]))
        .await
        .unwrap();
    assert!(!allowed("goodkey1", false));
    assert!(allowed("viewonly", false));
    let file = privileges_file(&t);
    assert!(file.get("peer:goodkey1").is_none());
    assert!(file.get("peer:viewonly").is_some());
}

#[tokio::test]
async fn a_malformed_grant_file_means_no_grants() {
    for content in ["not json", "[]", "{\"peer:x\": 5}"] {
        let t = setup_with(|data| {
            std::fs::write(data.join("view-privileges.json"), content).unwrap();
        });
        assert_eq!(t.state.load_screen_privileges().await, 0, "{content}");
    }
}

#[derive(Default)]
struct FakeLinks {
    connects: parking_lot::Mutex<Vec<String>>,
}

#[async_trait]
impl HostLinks for FakeLinks {
    fn list(&self) -> Vec<HostLinkInfo> {
        vec![HostLinkInfo {
            computer_access: false,
            computer_access_always: false,
            computer_browser: false,
            computer_commands: false,
            computer_files: false,
            id: "abc123".into(),
            name: "Oracle".into(),
            state: HostLinkState::Idle,
            detail: None,
        }]
    }
    async fn pair(&self, ticket: &str) -> Result<HostLinkInfo, HostLinkError> {
        if ticket == "bad" {
            return Err(HostLinkError::Invalid("not a pairing ticket".into()));
        }
        Ok(self.list().remove(0))
    }
    async fn connect(&self, id: &str) -> Result<HostLinkConnection, HostLinkError> {
        if id != "abc123" {
            return Err(HostLinkError::NotFound);
        }
        self.connects.lock().push(id.into());
        Ok(HostLinkConnection {
            id: id.into(),
            name: "Oracle".into(),
            base_url: "http://127.0.0.1:1".into(),
            token: "proxy-secret-token".into(),
            state: HostLinkState::Connecting,
        })
    }
    fn disconnect(&self, id: &str) -> bool {
        id == "abc123"
    }
}

#[tokio::test]
async fn host_links_are_local_only_and_closed_to_every_peer_role() {
    let t = setup();
    let links = Arc::new(FakeLinks::default());
    t.state.set_host_links(links.clone());
    let routes: Vec<(Method, &str)> = vec![
        (Method::GET, "/v1/hosts"),
        (Method::GET, "/v1/hosts/"),
        (Method::GET, "//v1//hosts"),
        (Method::GET, "/v1/%68osts"),
        (Method::POST, "/v1/hosts/pair"),
        (Method::POST, "/v1/hosts/pair/"),
        (Method::POST, "/v1/hosts/abc123/connect"),
        (Method::POST, "/v1/hosts//abc123//connect"),
        (Method::POST, "/v1/hosts%2Fabc123%2Fconnect"),
        (Method::DELETE, "/v1/hosts/abc123/connect"),
    ];
    for role in ROLES {
        for (method, path) in &routes {
            let r = peer_call(&t, role, method.clone(), path, Some(json!({"ticket": "t"}))).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} {method} {path}");
            assert!(
                !String::from_utf8_lossy(&r.body).contains("proxy-secret-token"),
                "no token in a refusal"
            );
        }
    }
    assert!(links.connects.lock().is_empty(), "no peer reached the seam");
}

#[tokio::test]
async fn host_link_routes_work_for_the_local_user_and_keep_secrets_out_of_the_audit() {
    let t = setup();
    // Without an embedder seam: empty list, actions unavailable.
    let r = call(&t, Method::GET, "/v1/hosts", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["hosts"], json!([]));
    assert_eq!(r.json()["local_version"], env!("CARGO_PKG_VERSION"));
    // /v1/host names the exact build, not just the cargo version every alpha shares.
    let r2 = call(&t, Method::GET, "/v1/host", None).await;
    assert_eq!(r2.status, StatusCode::OK);
    assert_eq!(r2.json()["build_id"], crate::build_id());
    assert!(
        r2.json()["build_id"]
            .as_str()
            .is_some_and(|b| b.starts_with(env!("CARGO_PKG_VERSION")) || b.starts_with('v')),
        "{}",
        r2.json()
    );
    let r = call(
        &t,
        Method::POST,
        "/v1/hosts/pair",
        Some(json!({"ticket": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    t.state.set_host_links(Arc::new(FakeLinks::default()));
    let r = call(&t, Method::GET, "/v1/hosts", None).await;
    assert_eq!(r.json()["hosts"][0]["id"], "abc123");
    assert_eq!(r.json()["hosts"][0]["state"], "idle");
    let r = call(
        &t,
        Method::POST,
        "/v1/hosts/pair",
        Some(json!({"ticket": "bad"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::POST,
        "/v1/hosts/pair",
        Some(json!({"ticket": "SECRET-TICKET"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(&t, Method::POST, "/v1/hosts/abc123/connect", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["token"], "proxy-secret-token");
    assert_eq!(r.json()["base_url"], "http://127.0.0.1:1");
    for bad in ["nope", "abc%2F123", "a%20b"] {
        let r = call(&t, Method::POST, &format!("/v1/hosts/{bad}/connect"), None).await;
        assert!(
            r.status == StatusCode::NOT_FOUND || r.status == StatusCode::BAD_REQUEST,
            "{bad}: {}",
            r.status
        );
    }
    let r = call(&t, Method::DELETE, "/v1/hosts/abc123/connect", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let audit = t
        .state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .iter()
        .map(|a| format!("{} {} {}", a.actor, a.action, a.detail))
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    assert!(audit.contains("host_link.pair") && audit.contains("host_link.connect"));
    assert!(
        !audit.contains("SECRET-TICKET"),
        "tickets never reach the audit log"
    );
    assert!(!audit.contains("proxy-secret-token"));
    let dbg = format!(
        "{:?}",
        HostLinkConnection {
            id: "i".into(),
            name: "n".into(),
            base_url: "u".into(),
            token: "proxy-secret-token".into(),
            state: HostLinkState::Idle
        }
    );
    assert!(!dbg.contains("proxy-secret-token"));
}

#[tokio::test]
async fn on_a_host_no_peer_can_mint_tickets_revoke_devices_or_change_leadership() {
    let t = host_daemon();
    for role in [crate::PeerRole::Desktop, crate::PeerRole::Phone] {
        for (method, path) in [
            (Method::POST, "/v1/host/invite"),
            (Method::POST, "//v1/host//invite/"),
            (Method::DELETE, "/v1/host/devices/abcd"),
            (Method::POST, "/v1/host/standby"),
            (Method::POST, "/v1/host/activate"),
            (Method::POST, "/v1/host/update"),
        ] {
            let r = peer_call(
                &t,
                role,
                method.clone(),
                path,
                Some(json!({"role": "phone"})),
            )
            .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} {method} {path}");
        }
    }
    let invited = count_audit(&t, "host.invite");
    assert_eq!(invited, 0, "a refused invite leaves no ticket or row");
    // Reading the status stays open to the desktop, and the local user can still invite.
    let r = peer_call(&t, crate::PeerRole::Desktop, Method::GET, "/v1/host", None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn no_paired_peer_can_start_a_server_update_or_probe_a_server() {
    let t = host_daemon();
    for role in [
        crate::PeerRole::Desktop,
        crate::PeerRole::Phone,
        crate::PeerRole::Host,
    ] {
        for (method, path) in [
            (Method::POST, "/v1/hosts/setup/update"),
            (Method::GET, "/v1/hosts/abcd/version"),
        ] {
            let r = peer_call(
                &t,
                role,
                method.clone(),
                path,
                Some(json!({"host_id": "abcd"})),
            )
            .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} {method} {path}");
        }
    }
    assert_eq!(count_audit(&t, "host_update.start"), 0);
}

#[tokio::test]
async fn on_a_host_a_peer_cannot_relax_execution_policy_but_can_edit_the_rest_of_a_bot() {
    let t = host_daemon();
    let desktop = crate::PeerRole::Desktop;
    // Global policy: refused for the peer, still available locally.
    let r = peer_call(&t, desktop, Method::PUT, "/v1/policy", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let profile = |extra: &str, name: &str| {
        format!(
            "[bot]\nid = \"tester\"\nname = \"{name}\"\n\n[models]\nexecutor = \"fake/echo\"\n{extra}"
        )
    };
    // Creating a bot with a permission preset or tool lists is refused; without them it works.
    for extra in [
        "\n[permissions]\npreset = \"full\"\n",
        "\n[tools]\nallow = [\"*\"]\n",
    ] {
        let r = peer_call(
            &t,
            desktop,
            Method::POST,
            "/v1/bots",
            Some(json!({"profile_toml": profile(extra, "A")})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{extra}");
    }
    let r = peer_call(
        &t,
        desktop,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": profile("", "Plain")})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    // A bot the local owner made with a strict preset: the peer can rename it,
    // but not change the preset, and omitting it keeps the stored one.
    let strict = "\n[permissions]\npreset = \"read-only\"\n";
    let made = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": profile(strict, "Owner")})),
    )
    .await;
    let id = made.json()["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/bots/{id}");
    let relax = "\n[permissions]\npreset = \"full\"\n";
    let r = peer_call(
        &t,
        desktop,
        Method::PUT,
        &path,
        Some(json!({"profile_toml": profile(relax, "Owner"), "expected_version": 1})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = peer_call(
        &t,
        desktop,
        Method::PUT,
        &path,
        Some(json!({"profile_toml": profile("", "Renamed"), "expected_version": 1})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let bot = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(bot["name"], "Renamed");
    assert_eq!(
        bot["config"]["permissions"]["preset"], "read-only",
        "omitted policy is preserved"
    );
    let r = peer_call(
        &t,
        desktop,
        Method::PUT,
        &path,
        Some(json!({"profile_toml": profile(strict, "Again"), "expected_version": 2})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "repeating the stored policy is not a change"
    );
}

#[derive(Default)]
struct FakeSetup {
    starts: parking_lot::Mutex<usize>,
}

fn fake_job() -> crate::host_setup::HostSetupJob {
    crate::host_setup::HostSetupJob {
        id: "job-1".into(),
        kind: crate::host_setup::HostJobKind::Setup,
        state: crate::host_setup::HostSetupState::Running,
        target: "ubuntu@203.0.113.7:22".into(),
        steps: Vec::new(),
        notes: Vec::new(),
        host_key: None,
        host_id: None,
        host_name: None,
        view_targets: None,
        install_available: false,
    }
}

impl crate::host_setup::HostSetup for FakeSetup {
    fn start(
        &self,
        _input: crate::host_setup::StartHostSetupInput,
    ) -> Result<crate::host_setup::HostSetupJob, crate::host_setup::HostSetupError> {
        *self.starts.lock() += 1;
        Ok(fake_job())
    }
    fn job(&self, id: &str) -> Option<crate::host_setup::HostSetupJob> {
        (id == "job-1").then(fake_job)
    }
    fn answer_host_key(
        &self,
        _id: &str,
        _accept: bool,
    ) -> Result<crate::host_setup::HostSetupJob, crate::host_setup::HostSetupError> {
        Ok(fake_job())
    }
    fn cancel(
        &self,
        _id: &str,
    ) -> Result<crate::host_setup::HostSetupJob, crate::host_setup::HostSetupError> {
        Ok(fake_job())
    }
    fn ssh_keys(&self) -> Vec<crate::host_setup::SshKeyHint> {
        Vec::new()
    }
    fn bootstrap(
        &self,
    ) -> Result<crate::host_setup::BootstrapCommand, crate::host_setup::HostSetupError> {
        Ok(crate::host_setup::BootstrapCommand {
            release_tag: "v0.1.0".into(),
            command: "echo hi".into(),
            ticket_minutes: 30,
            published: true,
        })
    }
}

#[tokio::test]
async fn server_setup_is_local_only_and_never_reaches_the_seam_from_a_peer() {
    let t = setup();
    let setup_seam = Arc::new(FakeSetup::default());
    t.state.set_host_setup(setup_seam.clone());
    let body = json!({"host": "203.0.113.7", "key_path": "/home/u/.ssh/id_ed25519"});
    let routes: Vec<(Method, &str)> = vec![
        (Method::POST, "/v1/hosts/setup"),
        (Method::GET, "/v1/hosts/setup/job-1"),
        (Method::POST, "/v1/hosts/setup/job-1/host-key"),
        (Method::POST, "/v1/hosts/setup/job-1/cancel"),
        (Method::GET, "/v1/hosts/setup/ssh-keys"),
        (Method::GET, "/v1/hosts/setup/bootstrap"),
    ];
    for role in ROLES {
        for (method, path) in &routes {
            let r = peer_call(&t, role, method.clone(), path, Some(body.clone())).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{role:?} {method} {path}");
        }
    }
    assert_eq!(*setup_seam.starts.lock(), 0, "no peer started a setup");

    // The local user can; the key file path is not audited.
    let r = call(&t, Method::POST, "/v1/hosts/setup", Some(body)).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(*setup_seam.starts.lock(), 1);
    assert_eq!(count_audit(&t, "host_setup.start"), 1);

    // The one-command setup text is served to the local app only.
    let r = call(&t, Method::GET, "/v1/hosts/setup/bootstrap", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["release_tag"], "v0.1.0");
    assert_eq!(v["ticket_minutes"], 30);
}

/// Review blocker 1 (card 258-A): the general API token alone, and even a
/// forged app `Origin`, must not flip the "bots may use this computer" switch,
/// list or answer approval cards; the owner key does.
#[tokio::test]
async fn computer_access_routes_need_the_owner_key_not_the_bearer_or_an_origin() {
    const OWNER: &str = "ownerkey0123456789abcdef0123456789abcdef0123456789abcdef";
    let t = setup();
    t.state.set_owner_key(OWNER.into());
    t.state.set_host_links(Arc::new(FakeLinks::default()));
    let (k, v) = auth();
    let bearer_only = [(k, v.as_str()), ("x-test-token-only", "1")];
    let forged_origin = [
        (k, v.as_str()),
        ("x-test-token-only", "1"),
        ("origin", "http://tauri.localhost"),
    ];
    let owner = [(k, v.as_str()), ("x-praxi-owner-key", OWNER)];
    let routes: [(Method, &str, Option<Value>); 3] = [
        (Method::GET, "/v1/hosts/computer-requests", None),
        (
            Method::POST,
            "/v1/hosts/computer-requests/abc",
            Some(json!({"allow": true})),
        ),
        (
            Method::PUT,
            "/v1/hosts/abc123/computer-access",
            Some(json!({"enabled": true})),
        ),
    ];
    for (method, path, body) in &routes {
        for headers in [&bearer_only[..], &forged_origin[..]] {
            let r = send(&t.app, method.clone(), path, body.clone(), headers).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
    let r = send(
        &t.app,
        Method::GET,
        "/v1/hosts/computer-requests",
        None,
        &owner,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "the owner key is accepted");
}

// ---------------------------------------------------------------- card 262

/// Concrete path of a table pattern.
fn concrete(pattern: &str) -> String {
    pattern.replace("{}", "x").replace('*', "a/b")
}

/// Every OWNER route: the general bearer alone is refused with
/// `owner_key_required`; with the owner key the request gets past the guard.
#[tokio::test]
async fn every_owner_route_refuses_the_general_bearer_and_accepts_the_owner_key() {
    let t = setup();
    for (method, pattern) in crate::owner::OWNER_ROUTES {
        let path = concrete(pattern);
        let m: Method = method.parse().unwrap();
        let body = Some(json!({}));
        let token_only = call_token_only(&t, m.clone(), &path, body.clone()).await;
        assert_eq!(token_only.status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(
            token_only.json()["error"]["code"],
            "owner_key_required",
            "{method} {path}"
        );
        let owner = call(&t, m, &path, body).await;
        assert_ne!(
            owner.json()["error"]["code"],
            "owner_key_required",
            "{method} {path} still refused with the owner key"
        );
    }
}

/// A new mutating route cannot ship unclassified: every documented one is in
/// the owner table or the explicit token-ok list.
#[test]
fn every_documented_mutating_route_is_classified() {
    let doc: Value = serde_json::from_str(&crate::openapi_json()).unwrap();
    let mut unclassified = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            if method == "get" || method == "head" {
                continue;
            }
            let ours = path.replace("{call_id}", "x");
            let generic = regex_free(&ours);
            if !crate::owner::is_owner_route(method, &generic)
                && !crate::owner::is_token_ok_route(method, &generic)
            {
                unclassified.push(format!("{} {path}", method.to_uppercase()));
            }
        }
    }
    assert!(
        unclassified.is_empty(),
        "unclassified routes: {unclassified:?}"
    );
}

/// `{name}`-style `OpenAPI` parameters become one segment; a wildcard secret name
/// may hold slashes, which one segment also matches.
fn regex_free(path: &str) -> String {
    path.split('/')
        .map(|seg| if seg.starts_with('{') { "x" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// A holder of only the token cannot approve its own tool call, answer its own
/// question or answer a desktop-control prompt.
#[tokio::test]
async fn approval_decisions_cannot_be_made_with_the_general_bearer() {
    let t = setup();
    for path in [
        "/v1/approvals/00000000-0000-0000-0000-000000000001",
        "/v1/questions/00000000-0000-0000-0000-000000000001",
        "/v1/control/00000000-0000-0000-0000-000000000001",
    ] {
        let r = call_token_only(
            &t,
            Method::POST,
            path,
            Some(json!({"decision": "approve", "scope": "always", "answer": "yes"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.json()["error"]["code"], "owner_key_required", "{path}");
    }
}

/// Permissions, tools or a CLI engine in a bot profile are authority; a plain
/// profile edit is not.
#[tokio::test]
async fn bot_profile_authority_needs_the_owner_key() {
    let t = setup();
    let plain = call_token_only(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": PROFILE })),
    )
    .await;
    assert_eq!(plain.status, StatusCode::CREATED, "{:?}", plain.json());
    let with_permissions = format!("{PROFILE}\n[permissions]\npreset = \"full\"\n");
    let powerful = call_token_only(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": with_permissions })),
    )
    .await;
    assert_eq!(
        powerful.json()["error"]["code"],
        "owner_key_required",
        "{:?}",
        powerful.json()
    );
}

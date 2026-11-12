use super::*;

async fn peer_call(
    t: &T,
    role: crate::PeerRole,
    method: Method,
    path: &str,
    body: Value,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(crate::PeerIdentity {
        key: "unprivileged-peer".into(),
        role,
    });
    crate::peer_router(t.state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn peers_cannot_manage_host_configuration_or_decide_local_overlays_by_default() {
    let t = setup();
    for role in [crate::PeerRole::Phone, crate::PeerRole::Desktop] {
        for (method, path) in [
            (Method::POST, "/v1/providers/reload"),
            (Method::POST, "/v1/providers/cli/enable"),
            (Method::PUT, "/v1/policy"),
            (Method::POST, "/v1/mcp/servers"),
            (Method::POST, "/v1/email/accounts"),
            (Method::POST, "/v1/bots"),
            (Method::POST, "/v1/schedules"),
            (
                Method::POST,
                "/v1/control/00000000-0000-0000-0000-000000000000",
            ),
        ] {
            assert_eq!(
                peer_call(&t, role, method, path, json!({})).await,
                StatusCode::FORBIDDEN,
                "{role:?} {path}"
            );
        }
    }
}

#[tokio::test]
async fn low_privilege_sse_omits_approval_events_on_replay() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = pending_call(&t).await;
    assert_eq!(
        call(
            &t,
            Method::POST,
            &format!("/v1/approvals/{call_id}"),
            Some(json!({"decision":"approve","scope":"once"}))
        )
        .await
        .status,
        StatusCode::OK
    );
    wait_state(&t, &run, "succeeded").await;
    let response = send(
        &t.app,
        Method::GET,
        &format!("/v1/events?run_id={run}"),
        None,
        &[("authorization", &format!("Bearer {LOW_TOKEN}"))],
    )
    .await;
    let replay = frames(&response.body);
    assert!(replay.iter().any(|event| event.1 == "run_finished"));
    for name in [
        "approval_needed",
        "tool_call_approved",
        "tool_call_denied",
        "control_requested",
    ] {
        assert!(
            !replay.iter().any(|event| event.1 == name),
            "low credential received {name}"
        );
    }
    assert!(
        events(&t, &format!("?run_id={run}"), None)
            .await
            .iter()
            .any(|event| event.1 == "approval_needed")
    );
}

#[tokio::test]
async fn isolation_status_reports_observations_without_claiming_a_sandbox() {
    let t = setup();
    let report = send(
        &t.app,
        Method::GET,
        "/v1/isolation",
        None,
        &[("authorization", &format!("Bearer {LOW_TOKEN}"))],
    )
    .await;
    assert_eq!(report.status, StatusCode::OK);
    let report = report.json();
    assert_eq!(report["privilege_split"], true);
    // Embedders do not get to assert production child wiring by default.
    assert_eq!(report["children_scrubbed"], false);
    assert_eq!(report["memory_guard"], false);
    assert_eq!(report["os_sandbox"], false);
    assert!(!report.to_string().contains(TOKEN));
    assert!(!report.to_string().contains(LOW_TOKEN));
}

#[test]
fn route_registry_covers_openapi_exactly() {
    use std::collections::BTreeSet;
    let doc: Value = serde_json::from_str(&crate::openapi_json()).unwrap();
    let mut described: BTreeSet<_> = doc["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .unwrap()
                .keys()
                .filter(|m| {
                    ["get", "post", "put", "delete", "patch", "head", "options"]
                        .contains(&m.as_str())
                })
                .map(move |method| (path.replace("{*name}", "{name}"), method.to_uppercase()))
        })
        .collect();
    described.insert(("/v1/openapi.json".to_owned(), "GET".to_owned()));
    let registered: BTreeSet<_> = crate::ROUTE_PRIVILEGES
        .iter()
        .map(|(path, method, _, _, _)| (path.replace("{*name}", "{name}"), (*method).to_owned()))
        .collect();
    assert_eq!(
        registered.len(),
        crate::ROUTE_PRIVILEGES.len(),
        "duplicate classification"
    );
    assert_eq!(
        described, registered,
        "every endpoint must have an explicit privilege and documentation"
    );
}

#[tokio::test]
async fn every_management_method_rejects_the_disk_credential() {
    let t = setup();
    for (path, method, level, _, _) in crate::ROUTE_PRIVILEGES {
        if *level != crate::auth::Privilege::Management {
            continue;
        }
        let path = path
            .replace("{id}", "00000000-0000-0000-0000-000000000000")
            .replace("{call_id}", "x")
            .replace("{request_id}", "x")
            .replace("{*name}", "x")
            .replace("{name}", "x");
        let r = send(
            &t.app,
            method.parse().unwrap(),
            &path,
            Some(json!({})),
            &[("authorization", &format!("Bearer {LOW_TOKEN}"))],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path}");
    }
}

#[tokio::test]
async fn management_token_is_header_only_and_unconfigured_means_disabled() {
    let t = setup();
    assert_eq!(
        send(
            &t.app,
            Method::GET,
            &format!("/v1/events?token={TOKEN}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &t.app,
            Method::HEAD,
            "/v1/control/state",
            None,
            &[("authorization", &format!("Bearer {LOW_TOKEN}"))]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&t, Method::GET, "/v1/control/state", None)
            .await
            .status,
        StatusCode::OK
    );
    let state = AppState::new(
        t.agent.clone(),
        t.state.storage_arc(),
        DaemonConfig::new(LOW_TOKEN),
    )
    .unwrap();
    let unconfigured = router(state);
    for credential in [LOW_TOKEN, TOKEN] {
        let status = send(
            &unconfigured,
            Method::GET,
            "/v1/control/state",
            None,
            &[("authorization", &format!("Bearer {credential}"))],
        )
        .await
        .status;
        assert!(!status.is_success());
    }
}

#[test]
fn bootstrap_rejects_partial_and_malformed_frames() {
    for frame in [vec![], b"short\n".to_vec(), vec![b'g'; 65], vec![b'a'; 65]] {
        assert!(crate::auth::read_launch_token(&mut frame.as_slice()).is_err());
    }
    let token = crate::auth::launch_token().unwrap();
    let frame = format!("{token}\n");
    assert_eq!(
        crate::auth::read_launch_token(&mut frame.as_bytes()).unwrap(),
        token
    );
}

/// The real shell can read the CLI credential; that must never confer authority.
#[tokio::test]
async fn shell_stolen_disk_token_cannot_manage_the_daemon() {
    let t = setup_with(|dir| std::fs::write(dir.join("api-token"), LOW_TOKEN).unwrap());
    let command = if cfg!(windows) {
        "Get-ChildItem -Recurse -File | ForEach-Object { try { [System.IO.File]::ReadAllText($_.FullName) } catch {} }; Get-ChildItem Env: | Out-String"
    } else {
        "find . -type f -exec cat {} \\; ; env"
    };
    let out = builtin_tools::shell::tool(t._dir.path())
        .call(
            &ToolContext::default(),
            json!({"command":command,"cwd":t._dir.path(),"timeout":30}),
        )
        .await
        .unwrap();
    let output = serde_json::to_string(&out).unwrap();
    assert!(
        !output.contains(TOKEN),
        "management credential leaked into shell output"
    );
    assert!(
        output.contains(LOW_TOKEN),
        "the shell must actually recover the disk credential"
    );
    let offset = output.find(LOW_TOKEN).unwrap();
    let stolen = &output[offset..offset + LOW_TOKEN.len()];
    let (_, chat) = make_chat(&t).await;
    let run = send_msg(&t, &chat, "call:danger_run {}", "queue").await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (call_id, _) = pending_call(&t).await;
    let decision = send(
        &t.app,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision":"approve","scope":"always"})),
        &[("authorization", &format!("Bearer {stolen}"))],
    )
    .await;
    assert_eq!(decision.status, StatusCode::FORBIDDEN);
    assert_eq!(
        pending_call(&t).await.0,
        call_id,
        "the approval must remain pending"
    );
    for (method, path) in [
        (Method::GET, "/v1/control/state"),
        (
            Method::POST,
            "/v1/approvals/00000000-0000-0000-0000-000000000000",
        ),
        (Method::PUT, "/v1/policy"),
        (Method::POST, "/v1/channels/telegram/pair-code"),
        (Method::POST, "/v1/bots"),
        (Method::POST, "/v1/mcp/servers"),
        (Method::POST, "/v1/email/accounts"),
        (Method::POST, "/v1/providers/reload"),
        (Method::POST, "/v1/schedules"),
        (Method::POST, "/v1/control/takeover"),
        (Method::GET, "/v1/view/targets"),
        (Method::POST, "/v1/devices"),
    ] {
        let r = send(
            &t.app,
            method,
            path,
            Some(json!({})),
            &[("authorization", &format!("Bearer {LOW_TOKEN}"))],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {:?}", r.json());
    }
    let approved = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision":"approve","scope":"once"})),
    )
    .await;
    assert_eq!(approved.status, StatusCode::OK);
    wait_state(&t, &run, "succeeded").await;
}

#[tokio::test]
async fn every_registry_peer_scope_is_enforced_and_unknown_routes_fail_closed() {
    let t = setup();
    for (path, method, _, phone, desktop) in crate::ROUTE_PRIVILEGES {
        let path = path
            .replace("{id}", "00000000-0000-0000-0000-000000000000")
            .replace("{call_id}", "x")
            .replace("{request_id}", "x")
            .replace("{*name}", "x")
            .replace("{name}", "x");
        for (role, scope) in [
            (crate::PeerRole::Phone, phone),
            (crate::PeerRole::Desktop, desktop),
        ] {
            if *scope != crate::auth::PeerScope::Allow {
                assert_eq!(
                    peer_call(&t, role, method.parse().unwrap(), &path, json!({})).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
        for role in [crate::PeerRole::Node, crate::PeerRole::Host] {
            assert_eq!(
                peer_call(&t, role, method.parse().unwrap(), &path, json!({})).await,
                StatusCode::FORBIDDEN
            );
        }
    }
    assert_eq!(
        peer_call(
            &t,
            crate::PeerRole::Desktop,
            Method::POST,
            "/v1/new-unclassified",
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let doc: Value = serde_json::from_str(&crate::openapi_json()).unwrap();
    for (path, method, _, phone, desktop) in crate::ROUTE_PRIVILEGES {
        if *path == "/v1/openapi.json" {
            continue;
        }
        let path = path.replace("{*name}", "{name}");
        assert_eq!(
            doc["paths"][&path][method.to_ascii_lowercase()]["x-praxi-peer-scopes"],
            json!({"phone":format!("{phone:?}"),"desktop":format!("{desktop:?}")})
        );
    }
}

#[tokio::test]
async fn explicitly_authorized_desktop_can_manage_but_never_decide_local_overlays() {
    let mut cfg = test_config();
    cfg.peer_managers.insert("unprivileged-peer".into());
    let t = setup_with_config(|_| {}, cfg);
    let bot = json!({"profile_toml":PROFILE});
    assert_eq!(
        peer_call(
            &t,
            crate::PeerRole::Phone,
            Method::POST,
            "/v1/bots",
            bot.clone()
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        peer_call(&t, crate::PeerRole::Desktop, Method::POST, "/v1/bots", bot).await,
        StatusCode::CREATED
    );
    assert_eq!(
        peer_call(
            &t,
            crate::PeerRole::Desktop,
            Method::POST,
            "/v1/control/00000000-0000-0000-0000-000000000000",
            json!({"decision":"proceed"})
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn paired_decisions_work_but_persistent_grants_require_host_management() {
    let t = setup();
    let (_, chat) = make_chat(&t).await;
    send_msg(&t, &chat, "call:danger_run {}", "queue").await;
    let (call_id, run) = pending_call(&t).await;
    assert_eq!(
        peer_call(
            &t,
            crate::PeerRole::Phone,
            Method::POST,
            &format!("/v1/approvals/{call_id}"),
            json!({"decision":"approve","scope":"always"})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(pending_call(&t).await.0, call_id);
    assert_eq!(
        send(
            &t.app,
            Method::GET,
            "/v1/approvals",
            None,
            &[("authorization", &format!("Bearer {LOW_TOKEN}"))]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &t.app,
            Method::GET,
            "/v1/approvals/count",
            None,
            &[("authorization", &format!("Bearer {LOW_TOKEN}"))]
        )
        .await
        .json()["pending"],
        1
    );
    assert_eq!(
        peer_call(
            &t,
            crate::PeerRole::Phone,
            Method::POST,
            &format!("/v1/approvals/{call_id}"),
            json!({"decision":"approve","scope":"once"})
        )
        .await,
        StatusCode::OK
    );
    wait_state(&t, &run, "succeeded").await;
}

#[tokio::test]
async fn only_local_management_global_streams_attach_the_overlay_bridge() {
    let t = setup();
    let request = Request::builder()
        .uri("/v1/events")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {LOW_TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let low = t.app.clone().oneshot(request).await.unwrap();
    assert!(!t.state.hub().app_connected());
    drop(low);
    let request = Request::builder()
        .uri("/v1/events")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let management = t.app.clone().oneshot(request).await.unwrap();
    assert!(t.state.hub().app_connected());
    drop(management);
    assert!(!t.state.hub().app_connected());
}

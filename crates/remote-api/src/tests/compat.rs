//! A server that runs another release is reachable for its status and the update request only.
use super::*;
use crate::{Compatibility, RemoteHealth, works_while_incompatible};
use std::sync::Mutex;

async fn checked(
    dir: &std::path::Path,
    compatible: Compatibility,
) -> (PeerNetwork, PeerNetwork, ApiProxy) {
    let mut cfg = config(dir, "host", Role::Host);
    cfg.extra_protocols.push((
        ALPN_API.to_vec(),
        Arc::new(ApiProtocol::new(state(&dir.join("db")))),
    ));
    let host = PeerNetwork::start(cfg, None).await.unwrap();
    let client = PeerNetwork::start(config(dir, "client", Role::Desktop), None)
        .await
        .unwrap();
    pair(&host, &client, Role::Desktop).await;
    let mut proxy = ApiProxy::start_checked(client.clone(), host.id(), compatible)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        proxy
            .state
            .wait_for(|s| matches!(s, ProxyState::Connected | ProxyState::Incompatible(_))),
    )
    .await
    .unwrap()
    .unwrap();
    (host, client, proxy)
}

async fn code_of(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn an_incompatible_server_answers_only_status_and_update_routes() {
    let dir = tempfile::tempdir().unwrap();
    let reason = "the server runs another Praxi release";
    let (host, client, proxy) =
        checked(dir.path(), Arc::new(move |_| Err(reason.to_owned()))).await;
    assert_eq!(
        *proxy.state.borrow(),
        ProxyState::Incompatible(reason.to_owned())
    );
    // Identity and status stay readable: the desktop needs them to say what is wrong.
    let health = request(&proxy, reqwest::Method::GET, "/v1/health")
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.json::<Value>().await.unwrap()["status"], "ok");
    // The routes that update the server are not held back by the proxy (the server decides).
    for (method, path) in [
        (reqwest::Method::GET, "/v1/host"),
        (reqwest::Method::POST, "/v1/host/update"),
        (reqwest::Method::POST, "/v1/admin/host-update"),
    ] {
        let (_, body) = code_of(request(&proxy, method.clone(), path).send().await.unwrap()).await;
        assert_ne!(
            body["error"]["code"], "host_incompatible",
            "{method} {path} must reach the server"
        );
    }
    // Work, bots, secrets and streams do not leave this computer.
    for (method, path) in [
        (reqwest::Method::GET, "/v1/bots"),
        (reqwest::Method::POST, "/v1/bots"),
        (reqwest::Method::DELETE, "/v1/bots/anything"),
        (reqwest::Method::GET, "/v1/admin/state"),
        (reqwest::Method::PUT, "/v1/admin/providers/openai"),
        (reqwest::Method::POST, "/v1/admin/import/bots"),
        (reqwest::Method::GET, "/v1/events"),
        (reqwest::Method::GET, "/v1/health/"),
        (reqwest::Method::GET, "/v1/host/devices"),
    ] {
        let (status, body) =
            code_of(request(&proxy, method.clone(), path).send().await.unwrap()).await;
        assert_eq!(status, 409, "{method} {path}");
        assert_eq!(
            body["error"]["code"], "host_incompatible",
            "{method} {path}"
        );
        assert_eq!(body["error"]["message"], reason, "{method} {path}");
    }
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn a_compatible_server_gets_everything_and_the_policy_sees_its_health() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<RemoteHealth>::new()));
    let recorder = seen.clone();
    let (host, client, proxy) = checked(
        dir.path(),
        Arc::new(move |health| {
            recorder.lock().unwrap().push(health.clone());
            Ok(())
        }),
    )
    .await;
    assert_eq!(*proxy.state.borrow(), ProxyState::Connected);
    let bots = request(&proxy, reqwest::Method::GET, "/v1/bots")
        .send()
        .await
        .unwrap();
    assert_eq!(bots.status(), 200);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "judged once per connection");
        assert_eq!(seen[0].api_version, daemon_api::API_VERSION);
        assert_ne!(seen[0].version, "");
        assert!(seen[0].build_id.is_some());
    }
    stop(host, client, proxy).await;
}

#[tokio::test]
async fn a_proxy_without_a_policy_keeps_its_old_behaviour() {
    let dir = tempfile::tempdir().unwrap();
    let (host, client, proxy) = networks(
        dir.path(),
        Role::Desktop,
        ApiProtocol::new(state(&dir.path().join("db"))),
    )
    .await;
    let bots = request(&proxy, reqwest::Method::GET, "/v1/bots")
        .send()
        .await
        .unwrap();
    assert_eq!(bots.status(), 200);
    stop(host, client, proxy).await;
}

#[test]
fn only_status_and_update_routes_are_exempt() {
    for (method, path) in [
        ("GET", "/v1/health"),
        ("GET", "/v1/host"),
        ("POST", "/v1/host/update"),
        ("POST", "/v1/admin/host-update"),
    ] {
        assert!(works_while_incompatible(method, path), "{method} {path}");
    }
    for (method, path) in [
        ("POST", "/v1/health"),
        ("PUT", "/v1/host"),
        ("GET", "/v1/host/update"),
        ("DELETE", "/v1/admin/host-update"),
        ("GET", "/v1/bots"),
        ("GET", "/v1/health/x"),
        ("GET", "/v1/events"),
        ("POST", "/v1/webhooks/abc"),
        ("get", "/v1/health"),
    ] {
        assert!(!works_while_incompatible(method, path), "{method} {path}");
    }
}

#[test]
fn a_health_body_that_names_no_release_reads_as_unidentified() {
    let full = RemoteHealth::from_json(&json!({
        "status": "ok", "version": "0.1.0", "api_version": "1.0",
        "release_tag": "v0.1.0-alpha.10", "build_id": "v0.1.0-alpha.10"
    }));
    assert_eq!(full.release_tag.as_deref(), Some("v0.1.0-alpha.10"));
    assert_eq!(full.api_version, "1.0");
    // An older build, an empty string or a wrong type are all "reports nothing".
    for body in [
        json!({"version": "0.1.0", "api_version": "1.0"}),
        json!({"version": "0.1.0", "release_tag": "", "build_id": null}),
        json!({"release_tag": 7}),
        json!("not an object"),
        json!(null),
    ] {
        let health = RemoteHealth::from_json(&body);
        assert_eq!(health.release_tag, None, "{body}");
        assert_eq!(health.build_id, None, "{body}");
    }
}

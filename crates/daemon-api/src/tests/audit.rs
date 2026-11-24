use axum::{
    Extension,
    http::{Method, StatusCode},
};
use serde_json::json;

use super::{TOKEN, call, send, setup};
use crate::{PeerIdentity, PeerRole};

fn seed(t: &super::T) {
    let st = t.state.storage();
    let u = t.state.user_id();
    st.append_audit(
        u,
        None,
        "user",
        "grant.always",
        &json!({"tool": "fs.write", "api_token": "abc123"}),
    )
    .unwrap();
    st.append_audit(
        u,
        None,
        "system",
        "profile.import",
        &json!({"note": "key sk-abcdefghijklmnop1234 here"}),
    )
    .unwrap();
    st.append_audit(u, None, "user", "trusted_folder.add", &json!({}))
        .unwrap();
}

#[tokio::test]
async fn audit_routes_are_owner_only_and_denied_to_peers() {
    let t = setup();
    for path in ["/v1/audit", "/v1/audit/export", "/v1/audit/verify"] {
        let r = send(
            &t.app,
            Method::GET,
            path,
            None,
            &[
                ("authorization", &format!("Bearer {TOKEN}")),
                ("x-test-token-only", "true"),
            ],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.json()["error"]["code"], "owner_key_required");
    }
    let peer = crate::peer_router(t.state.clone()).layer(Extension(PeerIdentity {
        key: "paired-test-peer".into(),
        role: PeerRole::Desktop,
    }));
    let r = send(&peer, Method::GET, "/v1/audit", None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn audit_list_filters_pages_and_redacts() {
    let t = setup();
    seed(&t);
    let r = call(&t, Method::GET, "/v1/audit", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["total"], 3);
    // newest first
    assert_eq!(j["items"][0]["action"], "trusted_folder.add");
    assert_eq!(j["items"][0]["kind"], "permission");
    let text = j.to_string();
    assert!(!text.contains("abc123") && !text.contains("sk-abcdefghijklmnop1234"));

    let r = call(&t, Method::GET, "/v1/audit?kind=import", None).await;
    assert_eq!(r.json()["total"], 1);
    let r = call(
        &t,
        Method::GET,
        "/v1/audit?actor=user&limit=1&offset=1",
        None,
    )
    .await;
    assert_eq!(r.json()["total"], 2);
    assert_eq!(r.json()["items"][0]["action"], "grant.always");
    let r = call(&t, Method::GET, "/v1/audit?q=fs.write", None).await;
    assert_eq!(r.json()["total"], 1);
    let r = call(&t, Method::GET, "/v1/audit?kind=nope", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn audit_export_is_redacted_and_verify_reports_ok() {
    let t = setup();
    seed(&t);
    let r = call(&t, Method::GET, "/v1/audit/export", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let csv = String::from_utf8(r.body).unwrap();
    assert!(csv.starts_with("id,time_utc_ms,actor,kind,action"));
    assert_eq!(csv.lines().count(), 4);
    assert!(!csv.contains("abc123") && !csv.contains("sk-abcdefghijklmnop1234"));
    let r = call(&t, Method::GET, "/v1/audit/export?format=json", None).await;
    assert_eq!(r.json()["events"].as_array().unwrap().len(), 3);
    let r = call(&t, Method::GET, "/v1/audit/verify", None).await;
    let j = r.json();
    assert_eq!(j["status"], "ok");
    assert_eq!(j["entries"], 3);
    assert_eq!(j["hash_chain"], false);
}

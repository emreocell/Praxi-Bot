use axum::http::{Method, StatusCode};
use serde_json::json;

use super::{call, call_token_only, setup};

#[tokio::test]
async fn elicitation_decisions_require_owner_authority_and_do_not_echo_malformed_input() {
    let t = setup();
    let id = protocol::ToolCallId::new();
    let path = format!("/v1/elicitations/{id}");
    let body = json!({"action":"decline"});
    for (path, body) in [(path.clone(), Some(body)), (format!("{path}/cancel"), None)] {
        let denied = call_token_only(&t, Method::POST, &path, body.clone()).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        assert_eq!(denied.json()["error"]["code"], "owner_key_required");
        let gone = call(&t, Method::POST, &path, body).await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND);
    }
    let canary = "form-body-canary-92744";
    let rejected = call(&t, Method::POST, &path, Some(json!({"action": canary}))).await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    assert!(!rejected.json().to_string().contains(canary));
}

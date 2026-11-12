//! Provider contract tests against a local mock of the `AgentMail` REST API
//! (wiremock). Nothing here touches the real service or any real key.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::config::AccountKind;
use crate::error::ProviderError;
use crate::imap::SearchQuery;
use crate::provider::{CreateInbox, SearchRequest};

const KEY: &str = "am_unit_test_KEY_9f3a7c1e2b";
const INBOX: &str = "bot@agentmail.to";

fn client(server: &MockServer) -> AgentMailClient {
    AgentMailClient::new(AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap())
        .unwrap()
}

fn bound(server: &MockServer) -> AgentMailProvider {
    AgentMailProvider::new(
        client(server),
        ApiKey::new(KEY).unwrap(),
        Some((INBOX.into(), INBOX.into())),
    )
    .unwrap()
}

fn unbound(server: &MockServer) -> AgentMailProvider {
    AgentMailProvider::new(client(server), ApiKey::new(KEY).unwrap(), None).unwrap()
}

fn inbox_json(id: &str) -> Value {
    json!({
        "pod_id": "pod_1", "inbox_id": id, "email": id,
        "updated_at": "2026-11-12T10:00:00Z", "created_at": "2026-11-12T10:00:00Z",
        "client_id": "c1"
    })
}

fn msg_json(id: &str, ts: &str, from: &str, labels: &[&str]) -> Value {
    json!({
        "inbox_id": INBOX, "thread_id": "t1", "message_id": id, "labels": labels,
        "timestamp": ts, "from": from, "to": [INBOX], "size": 1200,
        "updated_at": ts, "created_at": ts, "subject": format!("subject {id}"),
        "preview": "hello"
    })
}

fn request_bodies(reqs: &[Request]) -> Vec<Value> {
    reqs.iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
        .collect()
}

// ---- create inbox -------------------------------------------------------

#[tokio::test]
async fn create_inbox_posts_the_documented_body_with_the_users_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(body_json(json!({
            "username": "my-research-bot",
            "domain": "mail.example.com",
            "client_id": "praxi-1",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json("my-research-bot@mail.example.com")))
        .expect(1)
        .mount(&server)
        .await;
    let created = unbound(&server)
        .create_inbox(&CreateInbox {
            name_hint: Some("My Research Bot!".into()),
            domain: Some("mail.example.com".into()),
            client_id: "praxi-1".into(),
        })
        .await
        .unwrap();
    assert_eq!(created.inbox_id, "my-research-bot@mail.example.com");
    assert_eq!(created.address, "my-research-bot@mail.example.com");
}

#[tokio::test]
async fn create_inbox_without_a_hint_lets_the_provider_pick_the_name() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .and(body_json(json!({ "client_id": "praxi-2" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json("x1y2@agentmail.to")))
        .expect(1)
        .mount(&server)
        .await;
    let created = unbound(&server)
        .create_inbox(&CreateInbox {
            name_hint: Some("!!!".into()),
            domain: None,
            client_id: "praxi-2".into(),
        })
        .await
        .unwrap();
    assert_eq!(created.address, "x1y2@agentmail.to");
}

#[tokio::test]
async fn create_inbox_retry_reuses_the_client_id_so_no_second_inbox_appears() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json("a@agentmail.to")))
        .mount(&server)
        .await;
    let created = unbound(&server)
        .create_inbox(&CreateInbox {
            name_hint: Some("a".into()),
            domain: None,
            client_id: "praxi-idem".into(),
        })
        .await
        .unwrap();
    assert_eq!(created.address, "a@agentmail.to");
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2, "one failure, one retry");
    let bodies = request_bodies(&reqs);
    assert_eq!(bodies[0]["client_id"], "praxi-idem");
    assert_eq!(bodies[0], bodies[1], "the retry is byte-for-byte the same request");
}

#[tokio::test]
async fn create_inbox_waits_for_retry_after_on_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json("b@agentmail.to")))
        .mount(&server)
        .await;
    let t0 = Instant::now();
    unbound(&server)
        .create_inbox(&CreateInbox {
            name_hint: None,
            domain: None,
            client_id: "praxi-429".into(),
        })
        .await
        .unwrap();
    assert!(
        t0.elapsed() >= Duration::from_millis(900),
        "Retry-After must be respected, took {:?}",
        t0.elapsed()
    );
}

#[tokio::test]
async fn create_inbox_rejects_a_client_id_with_an_at_sign_before_any_request() {
    let server = MockServer::start().await;
    let err = unbound(&server)
        .create_inbox(&CreateInbox {
            name_hint: None,
            domain: None,
            client_id: "me@example.com".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, EmailError::Input(_)), "{err}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_inbox_refuses_a_malformed_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "inbox_id": "x", "email": "not an address"
        })))
        .mount(&server)
        .await;
    let err = unbound(&server)
        .create_inbox(&CreateInbox {
            name_hint: None,
            domain: None,
            client_id: "praxi-bad".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, EmailError::Provider(ProviderError::BadResponse)),
        "{err}"
    );
}

// ---- delete / check -----------------------------------------------------

#[tokio::test]
async fn delete_inbox_sends_delete_and_accepts_202() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!("/v0/inboxes/{INBOX}")))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    bound(&server).delete_inbox().await.unwrap();
}

#[tokio::test]
async fn delete_without_a_bound_inbox_is_an_input_error() {
    let server = MockServer::start().await;
    let err = unbound(&server).delete_inbox().await.unwrap_err();
    assert!(matches!(err, EmailError::Input(_)), "{err}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn check_is_one_authenticated_get_of_the_inbox() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}")))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json(INBOX)))
        .expect(1)
        .mount(&server)
        .await;
    bound(&server).check().await.unwrap();
}

#[tokio::test]
async fn check_without_an_inbox_lists_inboxes_with_limit_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v0/inboxes"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"inboxes":[]})))
        .expect(1)
        .mount(&server)
        .await;
    unbound(&server).check().await.unwrap();
}

// ---- error mapping ------------------------------------------------------

async fn failing(status: u16, retry_after: Option<&str>, body: Value) -> EmailError {
    let server = MockServer::start().await;
    let mut resp = ResponseTemplate::new(status).set_body_json(body);
    if let Some(r) = retry_after {
        resp = resp.insert_header("retry-after", r);
    }
    Mock::given(method("GET"))
        .respond_with(resp)
        .mount(&server)
        .await;
    bound(&server).list_new(None, 10).await.unwrap_err()
}

#[tokio::test]
async fn provider_errors_map_to_coarse_classes_without_the_providers_text() {
    let leak = "LEAKY provider text with am_secret_in_message and /private/path";
    let body = |code: &str| json!({"name":"E","code":code,"message":leak,"fix":leak});

    let e = failing(401, None, body("unknown_api_key")).await;
    assert!(matches!(e, EmailError::Provider(ProviderError::Auth)), "{e}");

    let e = failing(403, None, body("missing_permission")).await;
    assert!(matches!(e, EmailError::Provider(ProviderError::Forbidden)), "{e}");

    let e = failing(403, None, body("limit_exceeded")).await;
    assert!(matches!(e, EmailError::Provider(ProviderError::Quota)), "{e}");

    let e = failing(409, None, body("limit_exceeded")).await;
    assert!(matches!(e, EmailError::Provider(ProviderError::Quota)), "{e}");

    let e = failing(404, None, body("not_found")).await;
    assert!(matches!(e, EmailError::NotFound(_)), "{e}");

    let e = failing(429, Some("7"), body("rate_limited")).await;
    assert!(
        matches!(
            e,
            EmailError::Provider(ProviderError::RateLimited { retry_after_secs: Some(7) })
        ),
        "{e}"
    );
    assert!(e.to_string().contains("retry after 7s"), "{e}");

    let e = failing(500, None, body("internal")).await;
    assert!(matches!(e, EmailError::Provider(ProviderError::Transient)), "{e}");

    let e = failing(503, None, json!("not an object")).await;
    assert!(matches!(e, EmailError::Provider(ProviderError::Transient)), "{e}");

    let e = failing(422, None, body("validation_error")).await;
    assert!(
        matches!(&e, EmailError::Provider(ProviderError::Rejected { code: Some(c) }) if c == "validation_error"),
        "{e}"
    );

    // A hostile `code` is not echoed.
    let e = failing(422, None, body("Bad Code <script>")).await;
    assert!(
        matches!(e, EmailError::Provider(ProviderError::Rejected { code: None })),
        "{e}"
    );

    for status in [401, 403, 404, 422, 429, 500] {
        let e = failing(status, Some("3"), body("x")).await;
        let text = format!("{e} {e:?}");
        assert!(!text.contains("LEAKY") && !text.contains("am_secret") && !text.contains("/private"), "{text}");
    }
}

#[tokio::test]
async fn retry_after_is_clamped() {
    let e = failing(429, Some("999999"), json!({})).await;
    assert!(
        matches!(
            e,
            EmailError::Provider(ProviderError::RateLimited { retry_after_secs: Some(3600) })
        ),
        "{e}"
    );
}

#[tokio::test]
async fn an_oversized_response_body_is_rejected() {
    let server = MockServer::start().await;
    let big = "a".repeat(5 * 1024 * 1024);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(big))
        .mount(&server)
        .await;
    let e = bound(&server).list_new(None, 10).await.unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::BadResponse)), "{e}");
}

#[tokio::test]
async fn a_redirect_to_another_host_is_refused_and_never_followed() {
    let elsewhere = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages":[]})))
        .mount(&elsewhere)
        .await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/steal", elsewhere.uri())),
        )
        .mount(&server)
        .await;
    let e = bound(&server).list_new(None, 10).await.unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::Redirect)), "{e}");
    assert!(
        elsewhere.received_requests().await.unwrap().is_empty(),
        "the redirect target must never be contacted (and never sees the key)"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

// ---- the key never leaks ------------------------------------------------

struct CaptureLog(Mutex<Vec<String>>);

impl log::Log for CaptureLog {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, r: &log::Record<'_>) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{} {}", r.target(), r.args()));
    }
    fn flush(&self) {}
}

fn capture() -> &'static CaptureLog {
    static L: OnceLock<&'static CaptureLog> = OnceLock::new();
    L.get_or_init(|| {
        let l: &'static CaptureLog = Box::leak(Box::new(CaptureLog(Mutex::new(Vec::new()))));
        let _ = log::set_logger(l);
        log::set_max_level(log::LevelFilter::Trace);
        l
    })
}

#[tokio::test]
async fn the_key_never_appears_in_urls_logs_errors_or_debug_output() {
    let log = capture();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"code":"unknown_api_key","message":"bad key"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(inbox_json(INBOX)))
        .mount(&server)
        .await;
    let p = bound(&server);
    let mut seen = vec![format!("{p:?}"), format!("{:?}", ApiKey::new(KEY).unwrap())];
    let e = p.list_new(None, 5).await.unwrap_err();
    seen.push(format!("{e} {e:?}"));
    let e = p.get_message("INBOX", "m1", 100).await.unwrap_err();
    seen.push(format!("{e} {e:?}"));
    let e = p.search(&SearchRequest { folder: "INBOX", query: &SearchQuery::default(), limit: 5 }).await.unwrap_err();
    seen.push(format!("{e} {e:?}"));
    p.send(&OutgoingMail { to: vec!["ann@example.com".into()], subject: "s".into(), body: "b".into(), ..Default::default() }).await.ok();
    for r in server.received_requests().await.unwrap() {
        assert!(!r.url.to_string().contains(KEY), "key in URL {}", r.url);
        let auth = r.headers.get("authorization").map(|v| v.to_str().unwrap().to_owned());
        assert_eq!(auth.as_deref(), Some(format!("Bearer {KEY}").as_str()));
        assert!(!String::from_utf8_lossy(&r.body).contains(KEY), "key in body");
    }
    for s in &seen {
        assert!(!s.contains(KEY), "key leaked: {s}");
    }
    let logs = log.0.lock().unwrap().join("\n");
    assert!(!logs.contains(KEY), "key leaked into logs");
    assert!(!logs.contains("Bearer"), "authorization header leaked into logs");
}

#[test]
fn bad_key_shapes_are_rejected_without_echoing_them() {
    for bad in ["", "short", "has space in it 12345", "line\nbreak12345", "ünïcode-key-12345"] {
        let e = ApiKey::new(bad).unwrap_err();
        assert!(!e.to_string().contains(bad) || bad.is_empty(), "{e}");
    }
    assert!(ApiKey::new("am_abcdefgh").is_ok());
}

// ---- pinning ------------------------------------------------------------

#[test]
fn the_production_endpoint_is_pinned_to_the_agentmail_host_over_https() {
    let c = AgentMailClient::production().unwrap();
    assert_eq!(c.host(), "api.agentmail.to");
    assert_eq!(AgentMailEndpoint::production().host(), "api.agentmail.to");
}

#[test]
fn a_test_endpoint_can_only_be_plain_http_on_loopback() {
    for bad in [
        "https://127.0.0.1:9",
        "http://example.com",
        "http://10.0.0.5:8080",
        "http://169.254.169.254",
        "ftp://127.0.0.1",
        "not a url",
    ] {
        assert!(AgentMailEndpoint::loopback_mock_for_tests(bad).is_err(), "{bad}");
    }
    assert!(AgentMailEndpoint::loopback_mock_for_tests("http://127.0.0.1:1234").is_ok());
    assert!(AgentMailEndpoint::loopback_mock_for_tests("http://localhost:1234").is_ok());
}

#[test]
fn private_and_metadata_addresses_are_blocked() {
    use std::net::IpAddr;
    for ip in ["10.0.0.1", "192.168.1.1", "172.16.0.9", "127.0.0.1", "169.254.169.254", "::1", "fd00::1"] {
        assert!(http::is_blocked_address(ip.parse::<IpAddr>().unwrap()), "{ip}");
    }
    assert!(!http::is_blocked_address("8.8.8.8".parse::<IpAddr>().unwrap()));
}

#[tokio::test]
async fn ids_that_could_escape_the_path_are_refused_before_any_request() {
    let server = MockServer::start().await;
    let p = bound(&server);
    for bad in ["..", ".", "a/b", "a b", "x?y=1", "a%2Fb", "m#1", ""] {
        let e = p.get_message("INBOX", bad, 100).await.unwrap_err();
        assert!(
            matches!(e, EmailError::Input(_) | EmailError::Config(_)),
            "{bad:?}: {e}"
        );
    }
    assert!(AgentMailProvider::new(client(&server), ApiKey::new(KEY).unwrap(), Some(("../x".into(), "a@b.co".into()))).is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

// ---- reading ------------------------------------------------------------

#[tokio::test]
async fn list_new_returns_inbound_mail_oldest_first_with_a_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages")))
        .and(query_param("ascending", "true"))
        .and(query_param("after", "2026-11-12T09:00:00Z"))
        .and(query_param("limit", "50"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 3, "limit": 50,
            "messages": [
                msg_json("m1", "2026-11-12T09:30:00Z", "Ann <ann@example.com>", &["received", "unread"]),
                msg_json("m2", "2026-11-12T09:45:00Z", INBOX, &["sent"]),
                msg_json("m3", "2026-11-12T10:15:00Z", "bob@example.com", &["received"]),
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let r = bound(&server)
        .list_new(Some("2026-11-12T09:00:00Z"), 50)
        .await
        .unwrap();
    let ids: Vec<_> = r.messages.iter().map(|m| m.id.clone().unwrap()).collect();
    assert_eq!(ids, ["m1", "m3"], "the sent message is not new inbound mail");
    assert!(r.messages[0].unread);
    assert!(!r.messages[1].unread);
    assert_eq!(r.messages[0].from, "Ann <ann@example.com>");
    assert_eq!(r.next_cursor.as_deref(), Some("2026-11-12T10:15:00Z"));
}

#[tokio::test]
async fn list_new_without_a_cursor_keeps_the_cursor_when_nothing_is_new() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count":0,"messages":[]})))
        .mount(&server)
        .await;
    let r = bound(&server).list_new(Some("c0"), 10).await;
    // a cursor that is not a timestamp is refused before any request
    assert!(matches!(r, Err(EmailError::Input(_))), "{r:?}");
    let r = bound(&server).list_new(Some("2026-11-12T09:00:00Z"), 10).await.unwrap();
    assert_eq!(r.messages.len(), 0);
    assert_eq!(r.next_cursor.as_deref(), Some("2026-11-12T09:00:00Z"));
}

#[tokio::test]
async fn search_maps_the_criteria_to_list_filters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages")))
        .and(query_param("from", "ann@"))
        .and(query_param("subject", "invoice"))
        .and(query_param("after", "2026-11-01T00:00:00Z"))
        .and(query_param("before", "2026-11-10T00:00:00Z"))
        .and(query_param("labels", "unread"))
        .and(query_param("limit", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "messages": [msg_json("m9", "2026-11-05T08:00:00Z", "ann@example.com", &["unread"])]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let q = SearchQuery {
        from: Some("ann@".into()),
        subject: Some("invoice".into()),
        since: Some("2026-11-01".into()),
        before: Some("2026-11-10".into()),
        unread: Some(true),
    };
    let found = bound(&server)
        .search(&SearchRequest { folder: "INBOX", query: &q, limit: 7 })
        .await
        .unwrap();
    assert_eq!(found.messages.len(), 1);
    assert_eq!(found.messages[0].id.as_deref(), Some("m9"));
    assert_eq!(found.total, 1);
}

#[tokio::test]
async fn search_for_read_only_messages_is_not_supported() {
    let server = MockServer::start().await;
    let q = SearchQuery { unread: Some(false), ..SearchQuery::default() };
    let e = bound(&server)
        .search(&SearchRequest { folder: "INBOX", query: &q, limit: 5 })
        .await
        .unwrap_err();
    assert!(matches!(e, EmailError::Unsupported(_)), "{e}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn get_message_returns_text_and_attachment_metadata_capped() {
    let server = MockServer::start().await;
    let mut m = msg_json("m1", "2026-11-12T09:30:00Z", "ann@example.com", &["unread"]);
    m["text"] = json!("Hello there, this is the body");
    m["attachments"] = json!([{
        "attachment_id": "att_1", "size": 2048, "filename": "report.pdf",
        "content_type": "application/pdf"
    }]);
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(m))
        .expect(1)
        .mount(&server)
        .await;
    let d = bound(&server).get_message("INBOX", "m1", 11).await.unwrap();
    assert_eq!(d.body_text, "Hello there");
    assert!(d.body_truncated);
    assert_eq!(d.summary.id.as_deref(), Some("m1"));
    assert_eq!(d.attachments.len(), 1);
    assert_eq!(d.attachments[0].id.as_deref(), Some("att_1"));
    assert_eq!(d.attachments[0].filename.as_deref(), Some("report.pdf"));
    assert_eq!(d.attachments[0].size, 2048);
}

#[tokio::test]
async fn get_message_falls_back_to_html_as_text() {
    let server = MockServer::start().await;
    let mut m = msg_json("m2", "2026-11-12T09:30:00Z", "ann@example.com", &[]);
    m["html"] = json!("<html><body><p>Merhaba <b>dunya</b></p><script>alert(1)</script></body></html>");
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m2")))
        .respond_with(ResponseTemplate::new(200).set_body_json(m))
        .mount(&server)
        .await;
    let d = bound(&server).get_message("INBOX", "m2", 1000).await.unwrap();
    assert!(d.body_text.contains("Merhaba"), "{}", d.body_text);
    assert!(!d.body_text.contains("<p>") && !d.body_text.contains("alert"), "{}", d.body_text);
}

#[tokio::test]
async fn mark_read_swaps_the_unread_label() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1")))
        .and(body_json(json!({"add_labels":["read"],"remove_labels":["unread"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message_id":"m1","labels":["read"]})))
        .expect(1)
        .mount(&server)
        .await;
    bound(&server).mark_read("INBOX", "m1", true).await.unwrap();
}

// ---- attachments ----------------------------------------------------------

#[tokio::test]
async fn get_attachment_downloads_from_the_signed_url_without_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1/attachments/att_1")))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment_id": "att_1", "size": 5,
            "download_url": format!("{}/cdn/att_1?sig=abc", server.uri()),
            "expires_at": "2026-11-12T11:00:00Z",
            "filename": "../../evil.exe", "content_type": "application/octet-stream"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/att_1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let a = bound(&server).get_attachment("m1", "att_1", 1024).await.unwrap();
    assert_eq!(a.bytes, b"hello");
    let reqs = server.received_requests().await.unwrap();
    let cdn = reqs.iter().find(|r| r.url.path() == "/cdn/att_1").unwrap();
    assert!(cdn.headers.get("authorization").is_none(), "no credentials to the CDN");
}

#[tokio::test]
async fn an_attachment_over_the_cap_is_refused_before_downloading() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1/attachments/att_big")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment_id": "att_big", "size": 10_000_000,
            "download_url": format!("{}/cdn/big", server.uri()),
            "expires_at": "2026-11-12T11:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 16]))
        .expect(0)
        .mount(&server)
        .await;
    let e = bound(&server).get_attachment("m1", "att_big", 1024).await.unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::BadResponse)), "{e}");
}

#[tokio::test]
async fn a_download_larger_than_declared_is_cut_off_at_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1/attachments/liar")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment_id": "liar", "size": 4,
            "download_url": format!("{}/cdn/liar", server.uri()),
            "expires_at": "2026-11-12T11:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/liar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8; 4096]))
        .mount(&server)
        .await;
    let e = bound(&server).get_attachment("m1", "liar", 1024).await.unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::BadResponse)), "{e}");
}

#[tokio::test]
async fn a_non_https_or_redirecting_download_url_is_refused() {
    let server = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1/attachments/plain")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment_id": "plain", "size": 4,
            "download_url": format!("{}/file", elsewhere.uri()),
            "expires_at": "2026-11-12T11:00:00Z"
        })))
        .mount(&server)
        .await;
    let e = bound(&server).get_attachment("m1", "plain", 1024).await.unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::BadResponse)), "{e}");
    assert!(elsewhere.received_requests().await.unwrap().is_empty());

    Mock::given(method("GET"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1/attachments/redir")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attachment_id": "redir", "size": 4,
            "download_url": format!("{}/cdn/redir", server.uri()),
            "expires_at": "2026-11-12T11:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/redir"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{}/file", elsewhere.uri())))
        .mount(&server)
        .await;
    let e = bound(&server).get_attachment("m1", "redir", 1024).await.unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::Redirect)), "{e}");
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

// ---- sending --------------------------------------------------------------

#[tokio::test]
async fn send_posts_the_message_with_an_idempotency_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/send")))
        .and(header_exists("idempotency-key"))
        .and(body_json(json!({
            "to": ["bob@example.com"], "cc": ["cy@example.com"],
            "subject": "Hi", "text": "Body"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message_id":"sent_1","thread_id":"th_1"})))
        .expect(1)
        .mount(&server)
        .await;
    let r = bound(&server)
        .send(&OutgoingMail {
            to: vec!["bob@example.com".into()],
            cc: vec!["cy@example.com".into()],
            bcc: vec![],
            subject: "Hi".into(),
            body: "Body".into(),
        })
        .await
        .unwrap();
    assert_eq!(r.id, "sent_1");
    assert_eq!(r.thread_id.as_deref(), Some("th_1"));
}

#[tokio::test]
async fn send_is_never_retried_automatically() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let e = bound(&server)
        .send(&OutgoingMail { to: vec!["bob@example.com".into()], subject: "s".into(), body: "b".into(), ..Default::default() })
        .await
        .unwrap_err();
    assert!(matches!(e, EmailError::Provider(ProviderError::Transient)), "{e}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn send_validates_recipients_before_the_request() {
    let server = MockServer::start().await;
    let e = bound(&server)
        .send(&OutgoingMail { to: vec!["not an address".into()], subject: "s".into(), body: "b".into(), ..Default::default() })
        .await
        .unwrap_err();
    assert!(matches!(e, EmailError::Input(_)), "{e}");
    let e = bound(&server)
        .send(&OutgoingMail { to: vec![], subject: "s".into(), body: "b".into(), ..Default::default() })
        .await
        .unwrap_err();
    assert!(matches!(e, EmailError::Input(_)), "{e}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn reply_posts_to_the_reply_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/v0/inboxes/{INBOX}/messages/m1/reply")))
        .and(header_exists("idempotency-key"))
        .and(body_json(json!({"text":"Thanks","reply_all":true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message_id":"r1","thread_id":"th_1"})))
        .expect(1)
        .mount(&server)
        .await;
    let r = bound(&server)
        .reply("m1", &OutgoingMail { body: "Thanks".into(), ..Default::default() }, true)
        .await
        .unwrap();
    assert_eq!(r.id, "r1");
}

// ---- capabilities ---------------------------------------------------------

#[tokio::test]
async fn unsupported_operations_say_so() {
    let server = MockServer::start().await;
    let p = bound(&server);
    assert_eq!(p.kind(), AccountKind::AgentMail);
    let caps = p.capabilities();
    assert!(caps.create_inbox && caps.list_new && caps.attachments && caps.reply);
    assert!(!caps.folders && !caps.drafts && !caps.push);
    let e = p.list_folders().await.unwrap_err();
    assert!(matches!(e, EmailError::Unsupported(_)), "{e}");
    assert!(e.to_string().contains("not supported by this mailbox"), "{e}");
    let e = p.save_draft(&OutgoingMail::default()).await.unwrap_err();
    assert!(matches!(e, EmailError::Unsupported(_)), "{e}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

//! Tests of [`super::CalendarHost`] against a fake `CalDAV` server: persistence,
//! the secret store, reload of the live tools and the connection test.

#![allow(clippy::assert_is_empty)]

use std::sync::Mutex;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const PW: &str = "app-pw-7-secret";
/// base64 of `me@example.com:app-pw-7-secret`, lower-cased like the server reads headers.
fn good_auth() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .encode(format!("me@example.com:{PW}"))
        .to_ascii_lowercase()
}

struct Dav {
    port: u16,
    /// `authorization` headers the server received.
    auth: std::sync::Arc<Mutex<Vec<String>>>,
}

fn calendar_xml(href: &str, name: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:displayname>{name}</d:displayname>\
<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set>\
</d:prop></d:propstat></d:response>"
    )
}

/// `/one/` holds a single calendar `/one/cal/`; `/two/` holds two; `/cal/` is a calendar.
async fn dav() -> Dav {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let auth: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::default();
    let seen = auth.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let path = head.split(' ').nth(1).unwrap_or("").to_owned();
                let depth0 = head.contains("depth: 0");
                let header = head
                    .lines()
                    .find(|l| l.starts_with("authorization:"))
                    .unwrap_or("")
                    .to_owned();
                seen.lock().unwrap().push(header.clone());
                let (status, body) = if header.ends_with(&good_auth()) {
                    let list = |name: &str, base: &str, items: &[(&str, &str)]| {
                        let mut x = format!(
                            "<d:response><d:href>{base}</d:href><d:propstat><d:prop><d:displayname>{name}</d:displayname>\
<d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>"
                        );
                        if !depth0 {
                            for (h, n) in items {
                                x.push_str(&calendar_xml(h, n));
                            }
                        }
                        x
                    };
                    let inner = match path.as_str() {
                        "/cal/" => Some(calendar_xml("/cal/", "Only one")),
                        "/one/" => Some(list("One", "/one/", &[("/one/cal/", "Family")])),
                        "/two/" => Some(list(
                            "Two",
                            "/two/",
                            &[("/two/a/", "Home"), ("/two/b/", "Work")],
                        )),
                        _ => None,
                    };
                    match inner {
                        Some(i) => (
                            "207 Multi-Status",
                            format!(
                                "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">{i}</d:multistatus>"
                            ),
                        ),
                        None => ("404 Not Found", String::new()),
                    }
                } else {
                    ("401 Unauthorized", String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Dav { port, auth }
}

struct Rig {
    host: Arc<CalendarHost>,
    registry: ToolRegistry,
    store: Arc<dyn SecretStore>,
    dir: tempfile::TempDir,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let registry = ToolRegistry::new();
    let host = start_calendar(dir.path(), &registry, Some(&store), None).unwrap();
    Rig {
        host,
        registry,
        store,
        dir,
    }
}

fn input(name: &str, port: u16, path: &str) -> CalendarAccountInput {
    serde_json::from_value(json!({
        "name": name,
        "url": format!("http://127.0.0.1:{port}{path}"),
        "username": "me@example.com",
        "password": PW,
    }))
    .unwrap()
}

fn file(r: &Rig) -> String {
    std::fs::read_to_string(r.dir.path().join(CALENDAR_FILE)).unwrap_or_default()
}

#[test]
fn calendar_toml_registers_the_tools_and_bad_entries_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    std::fs::write(
        dir.path().join(CALENDAR_FILE),
        r#"
[[calendar]]
name = "home"
url = "https://dav.example.com/cal/"
username = "me"
password = "calendar/home/password"

[[calendar]]
name = "plain"
url = "http://dav.example.com/cal/"
username = "me"
password = "calendar/plain/password"
"#,
    )
    .unwrap();
    let registry = ToolRegistry::new();
    let host = start_calendar(dir.path(), &registry, Some(&store), None).unwrap();
    // Five tools, and only the https calendar is usable.
    for t in connector_calendar::TOOL_NAMES {
        assert!(registry.get(t).is_some(), "{t}");
    }
    assert_eq!(host.connector().names(), ["home"]);
    assert!(start_calendar(dir.path(), &ToolRegistry::new(), None, None).is_none());
}

#[tokio::test]
async fn adding_an_account_checks_the_server_stores_the_password_apart_and_goes_live() {
    let r = rig();
    let d = dav().await;
    let tool = r.registry.get("calendar_list_events").unwrap();
    assert!(!tool.visible_to(None));

    let made = r.host.create(input("home", d.port, "/cal/")).await.unwrap();
    assert_eq!(made.name, "home");
    assert!(made.password_set);
    // The file names the secret; it never holds the password.
    let text = file(&r);
    assert!(text.contains("calendar/home/password"), "{text}");
    assert!(!text.contains(PW), "{text}");
    assert_eq!(
        r.store
            .get("calendar/home/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW
    );
    // Live without a restart.
    assert_eq!(r.host.connector().names(), ["home"]);
    assert!(tool.visible_to(None));
    assert_eq!(r.host.list().len(), 1);
    assert!(!format!("{:?}", r.host.list()).contains(PW));

    let dup = r
        .host
        .create(input("home", d.port, "/cal/"))
        .await
        .unwrap_err();
    assert!(matches!(dup, CalendarControlError::Conflict(_)), "{dup:?}");
}

#[tokio::test]
async fn a_refused_login_or_a_bad_name_saves_nothing() {
    let r = rig();
    let d = dav().await;
    let mut wrong = input("home", d.port, "/cal/");
    wrong.password = Some("not-the-password".into());
    let err = r.host.create(wrong).await.unwrap_err();
    let CalendarControlError::Invalid(m) = err else {
        panic!("{err:?}");
    };
    assert!(m.contains("refused the login"), "{m}");
    assert!(!m.contains("not-the-password"));
    assert!(file(&r).is_empty());
    assert!(r.store.get("calendar/home/password").unwrap().is_none());
    assert!(r.host.connector().names().is_empty());

    for bad in ["", "has space", "a/b", &"x".repeat(41)] {
        let err = r
            .host
            .create(input(bad, d.port, "/cal/"))
            .await
            .unwrap_err();
        assert!(matches!(err, CalendarControlError::Invalid(_)), "{bad:?}");
    }
    // Plain http to a remote host is refused before any request is made.
    let mut remote = input("remote", d.port, "/cal/");
    remote.url = "http://calendar.example.com/cal/".into();
    assert!(matches!(
        r.host.create(remote).await.unwrap_err(),
        CalendarControlError::Invalid(_)
    ));
    assert!(file(&r).is_empty());
}

#[tokio::test]
async fn a_server_address_resolves_to_its_only_calendar_and_several_must_be_picked() {
    let r = rig();
    let d = dav().await;
    let made = r
        .host
        .create(input("family", d.port, "/one/"))
        .await
        .unwrap();
    assert!(made.url.ends_with("/one/cal/"), "{}", made.url);
    let err = r
        .host
        .create(input("both", d.port, "/two/"))
        .await
        .unwrap_err();
    let CalendarControlError::Invalid(m) = err else {
        panic!("{err:?}");
    };
    assert!(
        m.contains("2 calendars") && m.contains("Home") && m.contains("Work"),
        "{m}"
    );
    assert_eq!(r.host.list().len(), 1);
}

#[tokio::test]
async fn changing_an_account_keeps_the_password_and_removing_it_drops_everything() {
    let r = rig();
    let d = dav().await;
    r.host.create(input("home", d.port, "/cal/")).await.unwrap();
    let mut change = input("ignored", d.port, "/cal/");
    change.password = None;
    change.email = Some("Me@Example.com".into());
    let changed = r.host.update("home", change).await.unwrap();
    assert_eq!(changed.email.as_deref(), Some("me@example.com"));
    assert_eq!(
        r.store
            .get("calendar/home/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW
    );
    assert!(file(&r).contains("me@example.com"));

    let missing = r
        .host
        .update("nope", input("nope", d.port, "/cal/"))
        .await
        .unwrap_err();
    assert!(matches!(missing, CalendarControlError::NotFound(_)));

    r.host.delete("home").await.unwrap();
    assert!(r.host.list().is_empty());
    assert!(r.host.connector().names().is_empty());
    assert!(r.store.get("calendar/home/password").unwrap().is_none());
    assert!(!file(&r).contains("home"));
    assert!(matches!(
        r.host.delete("home").await.unwrap_err(),
        CalendarControlError::NotFound(_)
    ));
}

#[tokio::test]
async fn the_connection_test_lists_calendars_and_explains_failures_in_plain_words() {
    let r = rig();
    let d = dav().await;
    let ok = r
        .host
        .test(CalendarTestInput {
            url: Some(format!("http://127.0.0.1:{}/two/", d.port)),
            username: Some("me@example.com".into()),
            password: Some(PW.into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap();
    assert!(ok.ok, "{ok:?}");
    let names: Vec<_> = ok.calendars.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Home", "Work"]);
    assert!(ok.calendars[1].url.ends_with("/two/b/"));

    let wrong = r
        .host
        .test(CalendarTestInput {
            url: Some(format!("http://127.0.0.1:{}/two/", d.port)),
            username: Some("me@example.com".into()),
            password: Some("nope".into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap();
    assert!(!wrong.ok);
    assert!(
        wrong
            .error
            .as_deref()
            .unwrap()
            .contains("refused the login")
    );

    let lost = r
        .host
        .test(CalendarTestInput {
            url: Some("http://127.0.0.1:1/cal/".into()),
            username: Some("me".into()),
            password: Some(PW.into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap();
    assert!(!lost.ok);
    assert!(
        lost.error.as_deref().unwrap().contains("Could not reach"),
        "{lost:?}"
    );

    let bad_url = r
        .host
        .test(CalendarTestInput {
            url: Some("http://calendar.example.com/".into()),
            username: Some("me".into()),
            password: Some(PW.into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap();
    assert!(!bad_url.ok);
    assert!(bad_url.error.as_deref().unwrap().contains("https"));
}

#[tokio::test]
async fn a_saved_account_is_tested_with_its_own_password_at_its_own_address_only() {
    let r = rig();
    let d = dav().await;
    r.host.create(input("home", d.port, "/cal/")).await.unwrap();
    d.auth.lock().unwrap().clear();
    let report = r
        .host
        .test(CalendarTestInput {
            name: Some("home".into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap();
    assert!(report.ok, "{report:?}");
    assert_eq!(report.calendars.len(), 1);
    assert!(
        d.auth
            .lock()
            .unwrap()
            .iter()
            .all(|h| h.ends_with(&good_auth()))
    );

    // The stored password must never be sent to an address the caller names.
    let mixed = r
        .host
        .test(CalendarTestInput {
            name: Some("home".into()),
            url: Some("https://attacker.example.com/".into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(mixed, CalendarControlError::Invalid(_)));
    let unknown = r
        .host
        .test(CalendarTestInput {
            name: Some("nope".into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(unknown, CalendarControlError::NotFound(_)));
    assert!(matches!(
        r.host.test(CalendarTestInput::default()).await.unwrap_err(),
        CalendarControlError::Invalid(_)
    ));
}

#[tokio::test]
async fn an_unreadable_calendar_file_is_never_overwritten() {
    let r = rig();
    let d = dav().await;
    let path = r.dir.path().join(CALENDAR_FILE);
    std::fs::write(&path, "this is = not [valid toml").unwrap();
    let err = r
        .host
        .create(input("home", d.port, "/cal/"))
        .await
        .unwrap_err();
    assert!(matches!(err, CalendarControlError::Internal(_)), "{err:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "this is = not [valid toml"
    );
    assert!(r.store.get("calendar/home/password").unwrap().is_none());
}

//! Tests of [`super::CalendarHost`] against a fake `CalDAV` server: persistence,
//! the secret store, reload of the live tools and the connection test.

#![allow(clippy::assert_is_empty)]

use std::sync::Mutex;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const PW: &str = "app-pw-7-secret";
/// The port the tests pretend the daemon API listens on.
const DAEMON_PORT: u16 = 47_821;
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
    let host = start_calendar(dir.path(), &registry, Some(&store), None, DAEMON_PORT).unwrap();
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
    let host = start_calendar(dir.path(), &registry, Some(&store), None, DAEMON_PORT).unwrap();
    // Five tools, and only the https calendar is usable.
    for t in connector_calendar::TOOL_NAMES {
        assert!(registry.get(t).is_some(), "{t}");
    }
    assert_eq!(host.connector().names(), ["home"]);
    assert!(start_calendar(dir.path(), &ToolRegistry::new(), None, None, DAEMON_PORT).is_none());
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
async fn testing_a_saved_account_obeys_the_address_policy_too() {
    // A hand-written entry that points at the daemon's own API: the connector refuses it at
    // load, and the connection test must not be a way around that.
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    store.set("calendar/loop/password", PW).unwrap();
    std::fs::write(
        dir.path().join(CALENDAR_FILE),
        format!(
            r#"
[[calendar]]
name = "loop"
url = "http://127.0.0.1:{DAEMON_PORT}/v1/"
username = "me@example.com"
password = "calendar/loop/password"
"#
        ),
    )
    .unwrap();
    let host = start_calendar(
        dir.path(),
        &ToolRegistry::new(),
        Some(&store),
        None,
        DAEMON_PORT,
    )
    .unwrap();
    assert!(host.connector().names().is_empty());
    let refused = host
        .test(CalendarTestInput {
            name: Some("loop".into()),
            ..CalendarTestInput::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(refused, CalendarControlError::Invalid(_)),
        "{refused:?}"
    );
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

// ---- the stored password stays where it was saved ----

fn without_password(port: u16, path: &str) -> CalendarAccountInput {
    let mut i = input("x", port, path);
    i.password = None;
    i
}

fn refused_for_the_password(e: &CalendarControlError) -> bool {
    matches!(e, CalendarControlError::Invalid(m) if m.contains("password"))
}

#[tokio::test]
async fn updating_without_a_password_never_sends_the_stored_one_to_a_new_address_or_login() {
    let r = rig();
    let home = dav().await;
    // Somewhere else, recording every login it is sent.
    let elsewhere = dav().await;
    r.host
        .create(input("home", home.port, "/cal/"))
        .await
        .unwrap();
    home.auth.lock().unwrap().clear();
    let saved_url = r.host.list()[0].url.clone();

    // A new address, a new path on the same server, a new login, or an empty password
    // (which is no password) all need the password typed again.
    let mut new_login = without_password(home.port, "/cal/");
    new_login.username = "someone@example.com".into();
    let mut empty = without_password(elsewhere.port, "/cal/");
    empty.password = Some(String::new());
    for (what, change) in [
        ("address", without_password(elsewhere.port, "/cal/")),
        ("path", without_password(home.port, "/one/")),
        ("login", new_login),
        ("empty", empty),
    ] {
        let err = r.host.update("home", change).await.unwrap_err();
        assert!(refused_for_the_password(&err), "{what}: {err:?}");
    }
    assert!(
        elsewhere.auth.lock().unwrap().is_empty(),
        "the other server was never contacted"
    );
    assert!(
        home.auth.lock().unwrap().is_empty(),
        "no login was attempted at all"
    );
    // Nothing changed: not the file, not the password, not the live calendar.
    assert_eq!(r.host.list()[0].url, saved_url);
    assert_eq!(
        r.store
            .get("calendar/home/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW
    );
    assert_eq!(r.host.connector().names(), ["home"]);

    // The same address written another way (no trailing slash) is the same target.
    let same = r
        .host
        .update("home", without_password(home.port, "/cal"))
        .await
        .unwrap();
    assert_eq!(same.name, "home");

    // With the password typed again, the new address is allowed - and only then is it dialled.
    let moved = r
        .host
        .update("home", input("x", elsewhere.port, "/cal/"))
        .await
        .unwrap();
    assert!(moved.url.contains(&elsewhere.port.to_string()));
    assert!(!elsewhere.auth.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_save_puts_the_old_password_back_and_a_failed_create_leaves_none() {
    let r = rig();
    let d = dav().await;
    r.host.create(input("home", d.port, "/cal/")).await.unwrap();
    // The old password is something else, so a restore can be told from a new value.
    r.store.set("calendar/home/password", "old-value").unwrap();
    // Saving writes `calendar.toml.tmp` first: a directory there makes every save fail.
    let tmp = r.dir.path().join("calendar.toml.tmp");
    std::fs::create_dir(&tmp).unwrap();

    let err = r
        .host
        .update("home", input("x", d.port, "/cal/"))
        .await
        .unwrap_err();
    assert!(matches!(err, CalendarControlError::Internal(_)), "{err:?}");
    assert_eq!(
        r.store
            .get("calendar/home/password")
            .unwrap()
            .unwrap()
            .expose(),
        "old-value",
        "the password in the store is what it was"
    );

    let err = r
        .host
        .create(input("work", d.port, "/cal/"))
        .await
        .unwrap_err();
    assert!(matches!(err, CalendarControlError::Internal(_)), "{err:?}");
    assert!(r.store.get("calendar/work/password").unwrap().is_none());
    assert_eq!(r.host.list().len(), 1);
}

#[tokio::test]
async fn deleting_a_calendar_keeps_a_secret_it_did_not_make_or_that_another_calendar_uses() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    for name in [
        "shared/dav-password",
        "email/work/api_key",
        "calendar/a/password",
        "calendar/solo/password",
    ] {
        store.set(name, PW).unwrap();
    }
    let entry = |name: &str, secret: &str| {
        format!(
            "[[calendar]]\nname = \"{name}\"\nurl = \"http://127.0.0.1:9/cal/\"\nusername = \"me@example.com\"\npassword = \"{secret}\"\n\n"
        )
    };
    std::fs::write(
        dir.path().join(CALENDAR_FILE),
        [
            entry("hand", "shared/dav-password"),
            entry("other", "shared/dav-password"),
            entry("mailbox", "email/work/api_key"),
            entry("a", "calendar/a/password"),
            entry("b", "calendar/a/password"),
            entry("solo", "calendar/solo/password"),
        ]
        .concat(),
    )
    .unwrap();
    let registry = ToolRegistry::new();
    let host = start_calendar(dir.path(), &registry, Some(&store), None, DAEMON_PORT).unwrap();
    let has = |n: &str| store.get(n).unwrap().is_some();

    // Hand-written references name secrets that something else may own: never deleted.
    host.delete("hand").await.unwrap();
    host.delete("other").await.unwrap();
    assert!(has("shared/dav-password"));
    host.delete("mailbox").await.unwrap();
    assert!(has("email/work/api_key"), "another connector's secret");
    // A secret this host made, while another calendar still points at it: kept.
    host.delete("a").await.unwrap();
    assert!(has("calendar/a/password"));
    // The one it made for a calendar that nothing else uses goes with it.
    host.delete("solo").await.unwrap();
    assert!(!has("calendar/solo/password"));
    host.delete("b").await.unwrap();
    assert!(host.list().is_empty());
}

// ---- invitations go through the owner's mail rules ----

const BOT: &str = "11111111-1111-4111-8111-111111111111";

/// How many mails reached the (mock) mail provider.
async fn reached_the_provider(server: &wiremock::MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

async fn mailer() -> (wiremock::MockServer, Arc<EmailConnector>) {
    use connector_email::{
        AccountConfig, AccountKind, AgentMailEndpoint, IdentitySettings, ImapSettings, SmtpSettings,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v0/inboxes/bot%40agentmail.to/messages/send"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"message_id":"sent1","thread_id":"t"})),
        )
        .mount(&server)
        .await;
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let account = AccountConfig {
        name: "work".into(),
        address: "bot@agentmail.to".into(),
        username: String::new(),
        password: SecretRef::new("email/work/api_key").unwrap(),
        imap: ImapSettings::default(),
        smtp: SmtpSettings::default(),
        drafts_folder: "Drafts".into(),
        extra_root_pem: None,
        timeout_secs: 30,
        bot: Some(BOT.into()),
        allowed_senders: Vec::new(),
        kind: AccountKind::AgentMail,
        inbox_id: Some("bot@agentmail.to".into()),
        identity: IdentitySettings::default(),
    };
    store
        .set("email/work/api_key", "am_calendar_test_KEY_5e5e")
        .unwrap();
    let email = EmailConnector::reloadable_with(
        store,
        AgentMailEndpoint::loopback_mock_for_tests(&server.uri()).unwrap(),
    );
    email.replace_accounts(vec![account]).unwrap();
    (server, Arc::new(email))
}

fn to(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn invitations_go_through_the_owners_mail_rules_and_are_logged_with_the_bot() {
    let (server, email) = mailer().await;
    let m = EmailInvites {
        email: email.clone(),
    };
    let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:u1\r\nDTSTAMP:20261120T090000Z\r\nDTSTART:20261125T140000Z\r\nDTEND:20261125T150000Z\r\nSUMMARY:Sync\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let invite = |who: &[&str]| {
        let who = to(who);
        let (m, ics) = (&m, ics);
        async move {
            m.send(Some(BOT), &who, "Invitation: Sync", "Hello", ics, "REQUEST")
                .await
        }
    };

    // An invitation carries no opt-out line, and that must not stop it: the mail rules let it
    // through to the provider (this one, an AgentMail inbox, cannot carry an invitation, and
    // says so itself - the old guard call refused every invitation before it got that far).
    let why = invite(&["bob@example.com"]).await.unwrap_err();
    assert!(why.contains("not supported by this mailbox"), "{why}");
    assert!(!why.contains("opt-out line"), "{why}");

    // The opt-out list is enforced before a mail is sent and before an event is written.
    email.guard().add_opt_out("ann@example.com", "").unwrap();
    let why = m
        .check(Some(BOT), &to(&["ann@example.com"]))
        .await
        .unwrap_err();
    assert!(why.contains("asked not to be written to"), "{why}");
    assert!(m.check(Some(BOT), &to(&["bob@example.com"])).await.is_ok());
    let why = invite(&["bob@example.com", "ann@example.com"])
        .await
        .unwrap_err();
    assert!(why.contains("asked not to be written to"), "{why}");
    assert_eq!(reached_the_provider(&server).await, 0, "nothing was sent");
    assert!(email.guard().log(10).is_empty());

    // A calendar server that delivers by itself is counted too, under the bot that acted.
    m.record_server_delivery(
        Some(BOT),
        "me@example.com",
        &to(&["carol@example.com", "dave@example.com"]),
        "Invitation: Standup",
    );
    let log = email.guard().log(10);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].run, "calendar-server-invite");
    assert_eq!(log[0].bot.as_deref(), Some(BOT));
    assert_eq!(log[0].account, "work");
    assert_eq!(log[0].recipients, ["carol@example.com", "dave@example.com"]);
    assert_eq!(log[0].subject, "Invitation: Standup");

    // The daily limit holds for invitations the server delivers: checked before the event is written.
    email
        .guard()
        .set_settings(connector_email::GuardSettings {
            daily_limit: 3,
            ..connector_email::GuardSettings::default()
        })
        .unwrap();
    let why = m
        .check(Some(BOT), &to(&["x@example.com", "y@example.com"]))
        .await
        .unwrap_err();
    assert!(why.contains("limit"), "{why}");
    assert!(m.check(Some(BOT), &to(&["x@example.com"])).await.is_ok());

    // A bot with no mail account of its own is still checked and logged (the server may deliver).
    let other = "22222222-2222-4222-8222-222222222222";
    m.record_server_delivery(
        Some(other),
        "me@example.com",
        &to(&["z@example.com"]),
        "Invitation: Y",
    );
    assert!(
        email
            .guard()
            .log(10)
            .iter()
            .any(|e| e.bot.as_deref() == Some(other) && e.account == "calendar")
    );
    assert!(
        m.check(Some(other), &to(&["ann@example.com"]))
            .await
            .is_err()
    );
    assert!(m.check(Some(other), &to(&["x@example.com"])).await.is_ok());
}

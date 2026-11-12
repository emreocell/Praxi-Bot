//! `bot email ...` against a live daemon with a real `EmailHost`.

use std::sync::Arc;

use agent_core::Providers;
use daemon_api::fake::FakeProvider;
use daemon_api::{AppState, DaemonConfig};
use secret_store::{MemoryStore, SecretStore};
use storage::Storage;

use super::*;
use crate::client::ExitError;

const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
const PROFILE: &str = "[bot]\nid = \"t\"\nname = \"T\"\n[models]\nexecutor = \"fake/echo\"\n";
const PW: &str = "cli-test-password-9";

fn code_of(e: &(dyn std::error::Error + 'static)) -> i32 {
    e.downcast_ref::<ExitError>().map_or(-1, |x| x.code)
}

struct Daemon {
    dir: tempfile::TempDir,
    store: Arc<dyn SecretStore>,
    client: Client,
}

async fn daemon() -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let agent = crate::serve::build_agent(
        &storage,
        Providers::single(Arc::new(FakeProvider)),
        dir.path(),
        None,
    );
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let (host, _) =
        crate::email_host::start_email(dir.path(), &agent.tool_registry(), Some(&store)).unwrap();
    let state = AppState::new(agent, storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_email(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
    Daemon {
        dir,
        store,
        client: Client::new(&url, TOKEN.to_owned()),
    }
}

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

#[test]
fn presets_fill_the_servers_and_explicit_flags_win() {
    let senders = vec!["boss@example.com".to_owned()];
    let add = EmailAdd {
        name: "work",
        address: "me@gmail.com",
        preset: Some("gmail"),
        imap_host: None,
        imap_port: None,
        smtp_host: None,
        smtp_port: None,
        smtp_security: None,
        username: None,
        bot: None,
        allowed_senders: &senders,
    };
    let body = add_body(&add, "pw").unwrap();
    assert_eq!(body["imap"], json!({"host": "imap.gmail.com", "port": 993}));
    assert_eq!(
        body["smtp"],
        json!({"host": "smtp.gmail.com", "port": 465, "security": "implicit"})
    );
    assert_eq!(body["allowed_senders"], json!(["boss@example.com"]));
    assert_eq!(body["password"], "pw");
    assert!(body["bot"].is_null());

    let icloud = EmailAdd {
        preset: Some("icloud"),
        smtp_host: Some("smtp.example.org"),
        ..add
    };
    let body = add_body(&icloud, "pw").unwrap();
    assert_eq!(body["smtp"]["host"], "smtp.example.org");
    assert_eq!(body["smtp"]["port"], 587);
    assert_eq!(body["smtp"]["security"], "start_tls");

    // No preset and no servers: refused with the "rejected" exit code.
    let none = EmailAdd {
        preset: None,
        imap_host: None,
        smtp_host: None,
        ..add
    };
    assert_eq!(
        code_of(&*add_body(&none, "pw").unwrap_err()),
        exit::REJECTED
    );
    let unknown = EmailAdd {
        preset: Some("aol"),
        ..add
    };
    assert_eq!(
        code_of(&*add_body(&unknown, "pw").unwrap_err()),
        exit::REJECTED
    );
    // start_tls without a port defaults to 587.
    let custom = EmailAdd {
        preset: None,
        imap_host: Some("imap.example.org"),
        smtp_host: Some("smtp.example.org"),
        smtp_security: Some("start_tls"),
        ..add
    };
    assert_eq!(add_body(&custom, "pw").unwrap()["smtp"]["port"], 587);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_list_test_rm_against_a_live_daemon_never_print_the_password() {
    let d = daemon().await;
    let c = &d.client;
    let mut out = Vec::new();

    list(c, false, &mut out).await.unwrap();
    assert!(String::from_utf8_lossy(&out).contains("no e-mail accounts"));

    let port = closed_port();
    let senders = vec!["Boss@Example.com".to_owned()];
    let add_args = EmailAdd {
        name: "work",
        address: "me@example.com",
        preset: None,
        imap_host: Some("127.0.0.1"),
        imap_port: Some(port),
        smtp_host: Some("127.0.0.1"),
        smtp_port: Some(port),
        smtp_security: None,
        username: None,
        bot: None,
        allowed_senders: &senders,
    };
    out.clear();
    add(c, &add_args, PW, true, &mut out).await.unwrap();
    let created: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(created["name"], "work");
    assert_eq!(created["password_set"], true);
    assert_eq!(created["allowed_senders"], json!(["boss@example.com"]));
    assert!(!String::from_utf8_lossy(&out).contains(PW));
    // The password reached the secret store, not the file.
    assert_eq!(
        d.store
            .get("email/work/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW
    );
    let file = std::fs::read_to_string(d.dir.path().join("email.toml")).unwrap();
    assert!(!file.contains(PW), "{file}");

    out.clear();
    list(c, false, &mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(
        text.contains("work") && text.contains("me@example.com") && text.contains("bot all"),
        "{text}"
    );
    assert!(
        text.contains("password set") && !text.contains(PW),
        "{text}"
    );
    out.clear();
    list(c, true, &mut out).await.unwrap();
    assert!(!String::from_utf8_lossy(&out).contains(PW));

    // Duplicate: rejected (6). Unknown bot: not found (5).
    let err = add(c, &add_args, PW, false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::REJECTED);
    let bound = EmailAdd {
        name: "bound",
        bot: Some("01900000-0000-7000-8000-000000000000"),
        ..add_args
    };
    let err = add(c, &bound, PW, false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::NOT_FOUND);

    // Nothing listens on that port: the test fails with exit 6 and says why,
    // in statuses only.
    out.clear();
    let err = test(c, "work", false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::REJECTED);
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(
        text.contains("imap: unreachable") && text.contains("smtp: unreachable"),
        "{text}"
    );
    let err = test(c, "nope", false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::NOT_FOUND);

    // A bot bound account: created, then removed together with the bot's
    // deletion is covered in the daemon tests; here: rm.
    out.clear();
    rm(c, "work", false, &mut out).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "deleted\n");
    assert!(d.store.get("email/work/password").unwrap().is_none());
    let err = rm(c, "work", false, &mut out).await.unwrap_err();
    assert_eq!(code_of(&*err), exit::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_bound_account_is_removed_when_its_bot_is_deleted() {
    let d = daemon().await;
    let c = &d.client;
    let file = d.dir.path().join("bot.toml");
    std::fs::write(&file, PROFILE).unwrap();
    let mut out = Vec::new();
    crate::remote::bots_add(c, &file, true, &mut out)
        .await
        .unwrap();
    let bot = serde_json::from_slice::<Value>(&out).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let port = closed_port();
    let args = EmailAdd {
        name: "mine",
        address: "me@example.com",
        preset: None,
        imap_host: Some("127.0.0.1"),
        imap_port: Some(port),
        smtp_host: Some("127.0.0.1"),
        smtp_port: Some(port),
        smtp_security: None,
        username: None,
        bot: Some(&bot),
        allowed_senders: &[],
    };
    out.clear();
    add(c, &args, PW, true, &mut out).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["bot"],
        bot.as_str()
    );

    // Deleting the bot removes the account; it does not become a shared one.
    c.request(Method::DELETE, &format!("/v1/bots/{bot}"), None)
        .await
        .unwrap();
    let accounts = c
        .request(Method::GET, "/v1/email/accounts", None)
        .await
        .unwrap();
    assert_eq!(accounts, json!([]));
    assert!(d.store.get("email/mine/password").unwrap().is_none());
}

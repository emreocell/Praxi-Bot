//! Tests of [`super::EmailHost`]: persistence, reload, bot binding, secrets.

use secret_store::MemoryStore;
use serde_json::{Value, json};
use tool_registry::{ContentPart, ToolContext, ToolError};

use super::*;

const BOT_A: &str = "11111111-1111-4111-8111-111111111111";
const BOT_B: &str = "22222222-2222-4222-8222-222222222222";
const PW: &str = "correct-horse-battery-staple-7";

/// A TCP port nothing listens on, on the IPv4 loopback.
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

fn input(name: &str, bot: Option<&str>) -> EmailAccountInput {
    let port = closed_port();
    serde_json::from_value(json!({
        "name": name,
        "address": format!("{name}@example.com"),
        "imap": {"host": "127.0.0.1", "port": port},
        "smtp": {"host": "127.0.0.1", "port": port, "security": "implicit"},
        "bot": bot,
        "allowed_senders": ["Boss@Example.com", "@corp.example", "boss@example.com"],
        "password": PW,
    }))
    .unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    store: Arc<dyn SecretStore>,
    registry: ToolRegistry,
    host: Arc<EmailHost>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let registry = ToolRegistry::new();
    let (host, added) = start_email(dir.path(), &registry, Some(&store)).unwrap();
    assert_eq!(added, 6, "tools are registered even with no accounts");
    Fixture {
        dir,
        store,
        registry,
        host,
    }
}

fn file(f: &Fixture) -> String {
    std::fs::read_to_string(f.dir.path().join(EMAIL_FILE)).unwrap()
}

async fn call(f: &Fixture, bot: Option<&str>, input: Value) -> Result<Value, ToolError> {
    let tool = f.registry.get("email_list_folders").unwrap();
    let ctx = ToolContext {
        bot_id: bot.map(str::to_owned),
        ..ToolContext::default()
    };
    let out = tool.call(&ctx, input).await?;
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("json")
    };
    Ok(data.clone())
}

fn err_text(e: ToolError) -> String {
    match e {
        ToolError::InvalidInput(m) | ToolError::Execution(m) => m,
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn accounts_changed_through_the_api_apply_to_the_next_tool_call_without_a_restart() {
    let f = fixture();
    let tool = f.registry.get("email_list_folders").unwrap();
    assert!(!tool.visible_to(Some(BOT_A)), "no account yet: tool hidden");
    let e = err_text(call(&f, Some(BOT_A), json!({})).await.unwrap_err());
    assert!(e.contains("no e-mail account"), "{e}");

    // Create an account bound to bot A.
    f.host.add(input("mine", Some(BOT_A))).await.unwrap();
    assert!(tool.visible_to(Some(BOT_A)) && !tool.visible_to(Some(BOT_B)));
    // Bot A now reaches it (and fails at the closed port, not at the lookup).
    let e = err_text(call(&f, Some(BOT_A), json!({})).await.unwrap_err());
    assert!(!e.contains("account"), "reached the server: {e}");
    // Bot B asking for it by name is told what it is told for a missing one.
    let denied = err_text(
        call(&f, Some(BOT_B), json!({"account": "mine"}))
            .await
            .unwrap_err(),
    );
    let missing = err_text(
        call(&f, Some(BOT_B), json!({"account": "nope"}))
            .await
            .unwrap_err(),
    );
    assert_eq!(denied.replace("mine", "X"), missing.replace("nope", "X"));

    // Unbind it: now every bot can use it.
    f.host.update("mine", json!({"bot": null})).await.unwrap();
    let e = err_text(call(&f, Some(BOT_B), json!({})).await.unwrap_err());
    assert!(!e.contains("account"), "{e}");

    // Delete it: gone for the next call.
    f.host.remove("mine").await.unwrap();
    let e = err_text(call(&f, Some(BOT_A), json!({})).await.unwrap_err());
    assert!(e.contains("no e-mail account"), "{e}");
    assert_eq!(f.host.list().len(), 0);
}

#[tokio::test]
async fn the_file_holds_the_reference_never_the_password_and_reloads() {
    let f = fixture();
    let dto = f.host.add(input("work", Some(BOT_A))).await.unwrap();
    assert!(dto.password_set);
    assert_eq!(dto.bot.as_deref(), Some(BOT_A));
    assert_eq!(
        dto.allowed_senders,
        ["boss@example.com", "@corp.example"],
        "normalised and de-duplicated"
    );
    let text = file(&f);
    assert!(!text.contains(PW), "password in email.toml: {text}");
    assert!(text.contains("email/work/password"), "{text}");
    assert!(text.contains(BOT_A));
    // The password is in the secret store, under the managed name.
    assert_eq!(
        f.store
            .get("email/work/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW
    );
    // No temp file is left behind.
    assert!(!f.dir.path().join("email.toml.tmp").exists());

    // A restart (fresh host over the same files) sees the same accounts.
    let again = EmailHost::open(f.dir.path(), Arc::clone(&f.store));
    assert_eq!(again.list(), f.host.list());
    // The list carries no secret field at all.
    let json = serde_json::to_string(&f.host.list()).unwrap();
    assert!(
        !json.contains(PW) && !json.contains("email/work/password"),
        "{json}"
    );
}

#[tokio::test]
async fn old_files_without_the_new_fields_still_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(EMAIL_FILE),
        r#"
[[accounts]]
name = "work"
address = "alice@example.com"
password = "email/work/password"

[accounts.imap]
host = "imap.example.com"

[accounts.smtp]
host = "smtp.example.com"
"#,
    )
    .unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let host = EmailHost::open(dir.path(), Arc::clone(&store));
    let list = host.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].bot, None);
    assert_eq!(list[0].allowed_senders.len(), 0);
    assert!(!list[0].password_set, "no secret stored yet");
}

#[tokio::test]
async fn a_file_that_cannot_be_parsed_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(EMAIL_FILE), "this is [not toml").unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let host = EmailHost::open(dir.path(), Arc::clone(&store));
    let e = host.add(input("work", None)).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Internal(_)), "{e:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join(EMAIL_FILE)).unwrap(),
        "this is [not toml"
    );
    assert!(
        store.get("email/work/password").unwrap().is_none(),
        "rolled back"
    );
}

#[tokio::test]
async fn deleting_a_bot_removes_its_accounts_and_never_widens_access() {
    let f = fixture();
    f.host.add(input("a-mail", Some(BOT_A))).await.unwrap();
    f.host.add(input("b-mail", Some(BOT_B))).await.unwrap();
    f.host.add(input("shared", None)).await.unwrap();
    assert_eq!(f.host.forget_bot(BOT_A).await.unwrap(), 1);
    let names: Vec<_> = f.host.list().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["b-mail", "shared"]);
    // The removed account is gone for good: it did not become an unbound one
    // that every bot could use, and its password went with it.
    let e = err_text(
        call(&f, Some(BOT_B), json!({"account": "a-mail"}))
            .await
            .unwrap_err(),
    );
    assert!(e.contains("unknown account"), "{e}");
    assert!(f.store.get("email/a-mail/password").unwrap().is_none());
    assert!(f.store.get("email/b-mail/password").unwrap().is_some());
    assert!(f.store.get("email/shared/password").unwrap().is_some());
    let text = file(&f);
    assert!(!text.contains("a-mail") && !text.contains(BOT_A), "{text}");
    assert!(text.contains(BOT_B));
    // Nothing to do for a bot without accounts.
    assert_eq!(f.host.forget_bot(BOT_A).await.unwrap(), 0);
}

#[tokio::test]
async fn a_hand_named_password_secret_is_not_deleted_with_the_account() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    store.set("my-shared-mail-password", "x").unwrap();
    std::fs::write(
        dir.path().join(EMAIL_FILE),
        r#"
[[accounts]]
name = "a"
address = "a@example.com"
password = "my-shared-mail-password"
bot = "11111111-1111-4111-8111-111111111111"
[accounts.imap]
host = "imap.example.com"
[accounts.smtp]
host = "smtp.example.com"
"#,
    )
    .unwrap();
    let host = EmailHost::open(dir.path(), Arc::clone(&store));
    assert_eq!(host.forget_bot(BOT_A).await.unwrap(), 1);
    assert!(store.get("my-shared-mail-password").unwrap().is_some());
}

#[tokio::test]
async fn changing_servers_or_login_needs_the_password_again() {
    let f = fixture();
    f.host.add(input("work", None)).await.unwrap();
    // Redirecting the stored password to another server is refused.
    for patch in [
        json!({"imap": {"host": "evil.example.net", "port": 993}}),
        json!({"smtp": {"host": "evil.example.net", "port": 465}}),
        json!({"username": "someone-else"}),
        json!({"address": "other@example.com"}),
    ] {
        let e = f.host.update("work", patch.clone()).await.unwrap_err();
        assert!(matches!(e, EmailControlError::Invalid(_)), "{patch}: {e:?}");
    }
    assert_eq!(f.host.list()[0].imap.host, "127.0.0.1", "nothing changed");
    // With the password given, the change goes through and the secret updates.
    let dto = f
        .host
        .update(
            "work",
            json!({"imap": {"host": "imap.example.net", "port": 993}, "password": "new-pw-1"}),
        )
        .await
        .unwrap();
    assert_eq!(dto.imap.host, "imap.example.net");
    assert_eq!(
        f.store
            .get("email/work/password")
            .unwrap()
            .unwrap()
            .expose(),
        "new-pw-1"
    );
    // Non-credential edits need no password.
    let dto = f
        .host
        .update(
            "work",
            json!({"allowed_senders": ["me@example.com"], "drafts_folder": "Entwuerfe"}),
        )
        .await
        .unwrap();
    assert_eq!(dto.allowed_senders, ["me@example.com"]);
    assert!(!file(&f).contains("new-pw-1"));
}

#[tokio::test]
async fn validation_failures_leave_state_and_secrets_untouched() {
    let f = fixture();
    let mut bad = input("work", None);
    bad.imap.host = "imap.example.com\r\nA1 LOGIN".into();
    assert!(matches!(
        f.host.add(bad).await.unwrap_err(),
        EmailControlError::Invalid(_)
    ));
    let mut bad = input("work", None);
    bad.smtp.port = 0;
    assert!(f.host.add(bad).await.is_err());
    let mut bad = input("work", None);
    bad.allowed_senders = vec!["not an address".into()];
    assert!(f.host.add(bad).await.is_err());
    for name in ["", "a/b", "a b", "../x"] {
        assert!(f.host.add(input(name, None)).await.is_err(), "{name:?}");
    }
    let mut bad = input("work", None);
    bad.password = None;
    assert!(f.host.add(bad).await.is_err());
    assert_eq!(f.host.list().len(), 0);
    assert_eq!(f.store.list_names().unwrap().len(), 0);
    assert!(!f.dir.path().join(EMAIL_FILE).exists());
    // Duplicate names conflict.
    f.host.add(input("work", None)).await.unwrap();
    assert!(matches!(
        f.host.add(input("work", None)).await.unwrap_err(),
        EmailControlError::Conflict(_)
    ));
}

#[tokio::test]
async fn connection_tests_never_reuse_a_stored_password_for_other_servers() {
    let f = fixture();
    let created = input("work", None);
    f.host.add(created.clone()).await.unwrap();
    // Saved test: nothing listens, so both servers are unreachable, and the
    // report holds statuses only.
    let r = f.host.test_saved("work").await.unwrap();
    assert!(!r.ok);
    assert_eq!(r.imap, EmailProbeStatus::Unreachable);
    let text = serde_json::to_string(&r).unwrap();
    assert!(!text.contains(PW) && !text.contains("127.0.0.1"), "{text}");
    assert!(matches!(
        f.host.test_saved("nope").await.unwrap_err(),
        EmailControlError::NotFound(_)
    ));
    // A draft test with the same servers may use the stored password ...
    let mut same = created.clone();
    same.password = None;
    assert!(f.host.test_draft(same.clone()).await.is_ok());
    // ... but not with a different server, and not for an unknown name.
    let mut moved = same.clone();
    moved.imap.host = "evil.example.net".into();
    assert!(matches!(
        f.host.test_draft(moved).await.unwrap_err(),
        EmailControlError::Invalid(_)
    ));
    let mut stranger = same;
    stranger.name = "other".into();
    assert!(f.host.test_draft(stranger).await.is_err());
    // A draft test stores nothing.
    let mut draft = input("draft", None);
    draft.password = Some("draft-pw".into());
    assert!(f.host.test_draft(draft).await.is_ok());
    assert!(f.store.get("email/draft/password").unwrap().is_none());
    assert_eq!(f.host.list().len(), 1);
}

/// A store whose writes and deletes can be made to fail.
struct FlakyStore {
    inner: MemoryStore,
    fail_set: std::sync::atomic::AtomicBool,
    fail_delete: std::sync::atomic::AtomicBool,
}

impl FlakyStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryStore::new(),
            fail_set: std::sync::atomic::AtomicBool::new(false),
            fail_delete: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

impl SecretStore for FlakyStore {
    fn get(&self, n: &str) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        self.inner.get(n)
    }
    fn set(&self, n: &str, v: &str) -> Result<(), secret_store::SecretError> {
        if self.fail_set.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(secret_store::SecretError::InvalidName("injected".into()));
        }
        self.inner.set(n, v)
    }
    fn delete(&self, n: &str) -> Result<bool, secret_store::SecretError> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(secret_store::SecretError::InvalidName("injected".into()));
        }
        self.inner.delete(n)
    }
    fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
        self.inner.list_names()
    }
}

fn flaky_host() -> (tempfile::TempDir, Arc<FlakyStore>, Arc<EmailHost>) {
    let dir = tempfile::tempdir().unwrap();
    let store = FlakyStore::new();
    let dynstore: Arc<dyn SecretStore> = store.clone();
    let host = EmailHost::open(dir.path(), dynstore);
    (dir, store, host)
}

fn moved_patch() -> Value {
    json!({"imap": {"host": "imap.example.net", "port": 993}, "password": "new-pw-2"})
}

#[tokio::test]
async fn a_failed_password_write_leaves_servers_and_password_as_they_were() {
    let (dir, store, host) = flaky_host();
    host.add(input("work", None)).await.unwrap();
    let file_before = std::fs::read_to_string(dir.path().join(EMAIL_FILE)).unwrap();
    store
        .fail_set
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let e = host.update("work", moved_patch()).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Internal(_)), "{e:?}");
    // Neither the live account, nor the file, nor the stored password moved.
    assert_eq!(host.list()[0].imap.host, "127.0.0.1");
    assert_eq!(
        std::fs::read_to_string(dir.path().join(EMAIL_FILE)).unwrap(),
        file_before
    );
    assert_eq!(
        store
            .inner
            .get("email/work/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW
    );
}

#[tokio::test]
async fn a_failed_commit_puts_the_old_password_back() {
    let (dir, store, host) = flaky_host();
    host.add(input("work", None)).await.unwrap();
    // Make saving email.toml fail: a directory sits where the file is.
    let path = dir.path().join(EMAIL_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let e = host.update("work", moved_patch()).await.unwrap_err();
    assert!(matches!(e, EmailControlError::Internal(_)), "{e:?}");
    assert_eq!(
        host.list()[0].imap.host,
        "127.0.0.1",
        "live account unchanged"
    );
    assert_eq!(
        store
            .inner
            .get("email/work/password")
            .unwrap()
            .unwrap()
            .expose(),
        PW,
        "the old password was restored"
    );
}

#[tokio::test]
async fn a_password_that_could_not_be_deleted_is_retried_on_the_next_start() {
    let (dir, store, host) = flaky_host();
    host.add(input("work", None)).await.unwrap();
    store
        .fail_delete
        .store(true, std::sync::atomic::Ordering::SeqCst);
    host.remove("work").await.unwrap();
    assert!(store.inner.get("email/work/password").unwrap().is_some());
    let cleanup = dir.path().join("email-secret-cleanup.json");
    let text = std::fs::read_to_string(&cleanup).unwrap();
    assert!(
        text.contains("email/work/password") && !text.contains(PW),
        "{text}"
    );

    // Still failing at the next start: stays recorded.
    let dynstore: Arc<dyn SecretStore> = store.clone();
    let _again = EmailHost::open(dir.path(), Arc::clone(&dynstore));
    assert!(cleanup.exists());
    // Working again: deleted and the record is gone.
    store
        .fail_delete
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let _third = EmailHost::open(dir.path(), dynstore);
    assert!(store.inner.get("email/work/password").unwrap().is_none());
    assert!(!cleanup.exists());
}

#[tokio::test]
async fn a_retry_never_deletes_the_password_of_an_account_created_again() {
    let (dir, store, host) = flaky_host();
    host.add(input("work", None)).await.unwrap();
    store
        .fail_delete
        .store(true, std::sync::atomic::Ordering::SeqCst);
    host.remove("work").await.unwrap();
    store
        .fail_delete
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // Same name again, new password.
    let mut again = input("work", None);
    again.password = Some("second-pw".into());
    host.add(again).await.unwrap();
    let dynstore: Arc<dyn SecretStore> = store.clone();
    let reopened = EmailHost::open(dir.path(), dynstore);
    assert_eq!(reopened.list().len(), 1);
    assert_eq!(
        store
            .inner
            .get("email/work/password")
            .unwrap()
            .unwrap()
            .expose(),
        "second-pw"
    );
}

#[tokio::test]
async fn a_draft_test_trusts_the_private_ca_of_the_saved_account_only_for_the_same_servers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(EMAIL_FILE),
        r#"
[[accounts]]
name = "lan"
address = "me@lan.example"
password = "email/lan/password"
extra_root_pem = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
[accounts.imap]
host = "mail.lan.example"
[accounts.smtp]
host = "mail.lan.example"
"#,
    )
    .unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let host = EmailHost::open(dir.path(), store);
    let same: EmailAccountInput = serde_json::from_value(json!({
        "name": "lan", "address": "me@lan.example",
        "imap": {"host": "mail.lan.example", "port": 993},
        "smtp": {"host": "mail.lan.example", "port": 465},
    }))
    .unwrap();
    assert!(host.draft_account(&same).unwrap().extra_root_pem.is_some());
    let mut moved = same.clone();
    moved.imap.host = "elsewhere.example".into();
    assert!(host.draft_account(&moved).unwrap().extra_root_pem.is_none());
    let mut stranger = same;
    stranger.name = "other".into();
    assert!(
        host.draft_account(&stranger)
            .unwrap()
            .extra_root_pem
            .is_none()
    );
}

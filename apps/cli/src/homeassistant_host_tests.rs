//! Tests of [`super::HomeAssistantHost`] against a fake Home Assistant: persistence, the secret
//! store, reload of the live tools, per-bot access and the connection test.

use std::sync::Mutex;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tool_registry::ToolContext;

use super::*;

const TOKEN: &str = "ha-long-lived-token-xyz";
/// The port the tests pretend the daemon API listens on.
const DAEMON_PORT: u16 = 47_822;

struct Hub {
    port: u16,
    /// `authorization` headers the hub received.
    auth: std::sync::Arc<Mutex<Vec<String>>>,
}

/// A hub that accepts one token and reports three entities.
async fn hub() -> Hub {
    hub_with(TOKEN).await
}

/// The same hub for another token.
async fn hub_with(token: &'static str) -> Hub {
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
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let line = head.lines().next().unwrap_or("").to_owned();
                let header = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_owned();
                seen.lock().unwrap().push(header.clone());
                let (status, body) = if header.ends_with(&format!("Bearer {token}")) {
                    if line.starts_with("GET /api/states") {
                        (
                            "200 OK",
                            json!([
                                {"entity_id":"light.kitchen","state":"off","attributes":{"friendly_name":"Kitchen"}},
                                {"entity_id":"sensor.temp","state":"21","attributes":{}},
                                {"entity_id":"cover.garage","state":"closed","attributes":{}}
                            ])
                            .to_string(),
                        )
                    } else {
                        ("200 OK", json!({"message": "API running."}).to_string())
                    }
                } else {
                    ("401 Unauthorized", json!({"message": "no"}).to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Hub { port, auth }
}

struct Fx {
    dir: tempfile::TempDir,
    store: Arc<dyn SecretStore>,
    registry: ToolRegistry,
    host: Arc<HomeAssistantHost>,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    start(dir)
}

fn start(dir: tempfile::TempDir) -> Fx {
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    start_with(dir, store)
}

fn start_with(dir: tempfile::TempDir, store: Arc<dyn SecretStore>) -> Fx {
    let registry = ToolRegistry::new();
    let host = start_homeassistant(dir.path(), &registry, Some(&store), DAEMON_PORT).unwrap();
    Fx {
        dir,
        store,
        registry,
        host,
    }
}

impl Fx {
    fn file(&self) -> std::path::PathBuf {
        self.dir.path().join(HOMEASSISTANT_FILE)
    }

    fn visible(&self, bot: Option<&str>) -> bool {
        self.registry.get("ha_get_state").unwrap().visible_to(bot)
    }

    fn asks(&self, input: &Value) -> bool {
        self.registry
            .get("ha_call_service")
            .unwrap()
            .always_ask_for(input)
    }
}

fn input(port: u16, token: Option<&str>) -> HomeAssistantInput {
    HomeAssistantInput {
        url: format!("http://127.0.0.1:{port}"),
        token: token.map(str::to_owned),
        trusted_services: Vec::new(),
    }
}

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

fn is_invalid(e: &HomeAssistantControlError, words: &str) -> bool {
    matches!(e, HomeAssistantControlError::Invalid(m) if m.contains(words))
}

#[tokio::test]
async fn saving_checks_the_hub_keeps_the_token_apart_and_reloads_the_tools() {
    let hub = hub().await;
    let f = fx();
    assert!(!f.visible(None), "no hub yet: no tool is offered");
    assert!(!f.host.get().configured);

    let saved = f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    assert_eq!(saved.entities, 3);
    assert!(saved.settings.configured && saved.settings.token_set);
    assert_eq!(
        saved.settings.url.as_deref(),
        Some(format!("http://127.0.0.1:{}", hub.port).as_str())
    );

    // The file names the secret, never holds it; the dto never carries it.
    let file = std::fs::read_to_string(f.file()).unwrap();
    assert!(
        file.contains("homeassistant/token") && !file.contains(TOKEN),
        "{file}"
    );
    assert_eq!(
        f.store
            .get("homeassistant/token")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN
    );
    assert!(
        !serde_json::to_string(&f.host.get().url)
            .unwrap()
            .contains(TOKEN)
    );

    // No restart: the tools registered at start now work with that token.
    assert!(f.visible(Some("any-bot")));
    f.registry
        .get("ha_list_entities")
        .unwrap()
        .call(&ToolContext::default(), json!({"domain": "light"}))
        .await
        .unwrap();
    let seen = hub.auth.lock().unwrap().clone();
    assert!(!seen.is_empty() && seen.iter().all(|h| h.ends_with(&format!("Bearer {TOKEN}"))));
}

#[tokio::test]
async fn a_refused_token_or_an_unreachable_hub_saves_nothing() {
    let hub = hub().await;
    let f = fx();
    let err = f
        .host
        .save(input(hub.port, Some("wrong-token")))
        .await
        .unwrap_err();
    assert!(is_invalid(&err, "refused the access key"), "{err:?}");
    assert!(!format!("{err:?}").contains("wrong-token"));
    let err = f
        .host
        .save(input(closed_port(), Some(TOKEN)))
        .await
        .unwrap_err();
    assert!(is_invalid(&err, "Could not reach"), "{err:?}");
    assert!(!f.file().exists());
    assert!(f.store.get("homeassistant/token").unwrap().is_none());
    assert!(!f.visible(None));
}

#[tokio::test]
async fn the_stored_token_is_kept_for_the_same_address_only() {
    let hub = hub().await;
    let other = self::hub().await;
    let f = fx();
    let err = f.host.save(input(hub.port, None)).await.unwrap_err();
    assert!(is_invalid(&err, "key is needed"), "{err:?}");

    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    // Same address, no token, another list: fine, and the token is still the old one.
    let mut again = input(hub.port, None);
    again.trusted_services = vec!["cover.open_cover".into()];
    let saved = f.host.save(again).await.unwrap();
    assert_eq!(saved.settings.trusted_services, vec!["cover.open_cover"]);
    assert_eq!(
        f.store
            .get("homeassistant/token")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN
    );

    // Another address needs the token again: the stored one is never sent to a new place.
    let before = std::fs::read_to_string(f.file()).unwrap();
    let err = f.host.save(input(other.port, None)).await.unwrap_err();
    assert!(is_invalid(&err, "enter"), "{err:?}");
    assert_eq!(std::fs::read_to_string(f.file()).unwrap(), before);
    assert!(
        other.auth.lock().unwrap().is_empty(),
        "the other hub saw nothing"
    );
}

#[tokio::test]
async fn a_failed_replacement_keeps_the_working_setup() {
    let hub = hub().await;
    let f = fx();
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    let before = std::fs::read_to_string(f.file()).unwrap();
    assert!(f.host.save(input(hub.port, Some("bad"))).await.is_err());
    assert_eq!(std::fs::read_to_string(f.file()).unwrap(), before);
    assert_eq!(
        f.store
            .get("homeassistant/token")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN
    );
    assert!(f.visible(None), "the tools still work");
}

#[tokio::test]
async fn the_owners_list_is_cleaned_and_drives_the_card() {
    let hub = hub().await;
    let f = fx();
    let open = json!({"domain":"cover","service":"open_cover","entity_id":"cover.garage"});
    let close = json!({"domain":"cover","service":"close_cover","entity_id":"cover.garage"});

    let mut i = input(hub.port, Some(TOKEN));
    i.trusted_services = vec![
        " Cover.Open_Cover ".into(),
        "cover.open_cover".into(),
        " ".into(),
    ];
    let saved = f.host.save(i).await.unwrap();
    assert_eq!(saved.settings.trusted_services, vec!["cover.open_cover"]);
    assert!(!f.asks(&open), "the owner allowed this one");
    assert!(f.asks(&close), "and only this one");
    assert!(f.asks(&json!({"domain":"lock","service":"unlock"})));

    for bad in [
        "homeassistant.turn_on",
        "lock",
        "cover.",
        "Cover open",
        "a.b.c",
    ] {
        let mut i = input(hub.port, Some(TOKEN));
        i.trusted_services = vec![bad.into()];
        let err = f.host.save(i).await.unwrap_err();
        assert!(
            matches!(err, HomeAssistantControlError::Invalid(_)),
            "{bad}: {err:?}"
        );
    }
    // The refused attempts changed nothing.
    assert_eq!(f.host.get().trusted_services, vec!["cover.open_cover"]);
    // Emptying the list takes the allowance away at once.
    f.host.save(input(hub.port, None)).await.unwrap();
    assert!(f.asks(&open));
}

#[tokio::test]
async fn bots_are_limited_per_bot_and_a_new_save_keeps_the_choice() {
    let hub = hub().await;
    let f = fx();
    let err = f.host.set_bots(Some(vec!["a".into()])).await.unwrap_err();
    assert!(
        matches!(err, HomeAssistantControlError::NotFound(_)),
        "{err:?}"
    );

    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    assert!(
        f.visible(None) && f.visible(Some("bot-b")),
        "no list: every bot"
    );

    let dto = f
        .host
        .set_bots(Some(vec!["bot-a".into(), "bot-a".into(), " bot-c ".into()]))
        .await
        .unwrap();
    assert_eq!(dto.bots, Some(vec!["bot-a".to_owned(), "bot-c".to_owned()]));
    assert!(f.visible(Some("bot-a")) && f.visible(Some("bot-c")));
    assert!(!f.visible(Some("bot-b")) && !f.visible(None));
    assert!(std::fs::read_to_string(f.file()).unwrap().contains("bot-a"));

    // Saving the address again does not widen the access.
    f.host.save(input(hub.port, None)).await.unwrap();
    assert_eq!(
        f.host.get().bots,
        Some(vec!["bot-a".to_owned(), "bot-c".to_owned()])
    );
    assert!(!f.visible(Some("bot-b")));

    f.host.set_bots(Some(vec![])).await.unwrap();
    assert!(!f.visible(Some("bot-a")), "an empty list is nobody");
    f.host.set_bots(None).await.unwrap();
    assert!(f.visible(Some("bot-b")));
    assert!(!std::fs::read_to_string(f.file()).unwrap().contains("bots"));

    let err = f
        .host
        .set_bots(Some(vec![String::new()]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, HomeAssistantControlError::Invalid(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn removing_deletes_the_file_and_the_token_and_hides_the_tools() {
    let hub = hub().await;
    let f = fx();
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    f.host.remove().await.unwrap();
    assert!(!f.file().exists());
    assert!(f.store.get("homeassistant/token").unwrap().is_none());
    assert!(!f.visible(None));
    assert!(!f.host.get().configured);
    assert!(matches!(
        f.host.remove().await.unwrap_err(),
        HomeAssistantControlError::NotFound(_)
    ));
    // And it can be set up again.
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    assert!(f.visible(None));
}

#[tokio::test]
async fn removing_leaves_a_hand_written_secret_that_may_be_shared() {
    let hub = hub().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(HOMEASSISTANT_FILE),
        format!(
            "url = \"http://127.0.0.1:{}\"\ntoken = \"my/own/secret\"\nallowed_domains = [\"light\"]\n",
            hub.port
        ),
    )
    .unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    store.set("my/own/secret", TOKEN).unwrap();
    let f = start_with(dir, store);
    assert!(f.visible(None), "a hand-written file works as it did");
    assert!(f.host.get().token_set);

    // Saving from the screen keeps what the screen does not show.
    f.host.save(input(hub.port, None)).await.unwrap();
    let file = std::fs::read_to_string(f.file()).unwrap();
    assert!(
        file.contains("my/own/secret") && file.contains("allowed_domains"),
        "{file}"
    );

    f.host.remove().await.unwrap();
    assert_eq!(
        f.store.get("my/own/secret").unwrap().unwrap().expose(),
        TOKEN
    );
}

#[tokio::test]
async fn a_broken_file_is_never_overwritten() {
    let hub = hub().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(HOMEASSISTANT_FILE), "url = [oops").unwrap();
    let f = start(dir);
    assert!(!f.visible(None));
    let err = f.host.save(input(hub.port, Some(TOKEN))).await.unwrap_err();
    assert!(
        matches!(err, HomeAssistantControlError::Internal(_)),
        "{err:?}"
    );
    assert_eq!(std::fs::read_to_string(f.file()).unwrap(), "url = [oops");
    assert!(f.store.get("homeassistant/token").unwrap().is_none());
}

#[tokio::test]
async fn a_restart_reads_the_saved_setup_back() {
    let hub = hub().await;
    let f = fx();
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    f.host.set_bots(Some(vec!["bot-a".into()])).await.unwrap();
    let Fx { dir, store, .. } = f;
    let again = start_with(dir, store);
    assert!(again.visible(Some("bot-a")) && !again.visible(Some("bot-b")));
    assert_eq!(again.host.get().bots, Some(vec!["bot-a".to_owned()]));
}

#[tokio::test]
async fn the_connection_test_counts_entities_for_the_saved_hub_or_an_unsaved_one() {
    let hub = hub().await;
    let f = fx();
    let err = f
        .host
        .test(HomeAssistantTestInput::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, HomeAssistantControlError::NotFound(_)),
        "{err:?}"
    );

    let unsaved = HomeAssistantTestInput {
        url: Some(format!("http://127.0.0.1:{}", hub.port)),
        token: Some(TOKEN.into()),
    };
    let r = f.host.test(unsaved).await.unwrap();
    assert_eq!((r.ok, r.entities, r.error), (true, Some(3), None));
    // An unsaved test leaves nothing behind.
    assert!(!f.file().exists() && f.store.get("homeassistant/token").unwrap().is_none());

    let wrong = HomeAssistantTestInput {
        url: Some(format!("http://127.0.0.1:{}", hub.port)),
        token: Some("wrong-token".into()),
    };
    let r = f.host.test(wrong).await.unwrap();
    assert!(!r.ok);
    let text = r.error.unwrap();
    assert!(
        text.contains("refused the access key") && !text.contains("wrong-token"),
        "{text}"
    );

    let dead = HomeAssistantTestInput {
        url: Some(format!("http://127.0.0.1:{}", closed_port())),
        token: Some(TOKEN.into()),
    };
    let r = f.host.test(dead).await.unwrap();
    assert!(!r.ok && r.error.unwrap().contains("Could not reach"));

    // An address without a token (or the other way round) is not tried: the stored token never
    // goes to an address the caller chose.
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    let seen_before = hub.auth.lock().unwrap().len();
    let other = self::hub().await;
    for bad in [
        HomeAssistantTestInput {
            url: Some(format!("http://127.0.0.1:{}", other.port)),
            token: None,
        },
        HomeAssistantTestInput {
            url: None,
            token: Some(TOKEN.into()),
        },
    ] {
        let err = f.host.test(bad).await.unwrap_err();
        assert!(
            matches!(err, HomeAssistantControlError::Invalid(_)),
            "{err:?}"
        );
    }
    assert!(other.auth.lock().unwrap().is_empty());
    assert_eq!(hub.auth.lock().unwrap().len(), seen_before);

    // The saved hub, with its own token.
    let r = f
        .host
        .test(HomeAssistantTestInput::default())
        .await
        .unwrap();
    assert_eq!((r.ok, r.entities), (true, Some(3)));
}

#[tokio::test]
async fn only_an_address_the_owner_gave_and_a_safe_one_is_ever_dialled() {
    let f = fx();
    // The daemon's own API, cloud metadata and link-local space, a public plain-http host and
    // credentials in the address: refused before anything is sent.
    for url in [
        format!("http://127.0.0.1:{DAEMON_PORT}"),
        "http://169.254.169.254/latest".to_owned(),
        "http://[fe80::1]:8123".to_owned(),
        "http://ha.example.com:8123".to_owned(),
        "https://user:pw@ha.example.com".to_owned(),
        "file:///etc/passwd".to_owned(),
    ] {
        let i = HomeAssistantInput {
            url: url.clone(),
            token: Some(TOKEN.into()),
            trusted_services: Vec::new(),
        };
        let err = f.host.save(i).await.unwrap_err();
        assert!(
            matches!(err, HomeAssistantControlError::Invalid(_)),
            "{url}: {err:?}"
        );
        let r = f
            .host
            .test(HomeAssistantTestInput {
                url: Some(url.clone()),
                token: Some(TOKEN.into()),
            })
            .await
            .unwrap();
        assert!(!r.ok, "{url}");
    }
    assert!(!f.file().exists());
}

/// A store that remembers every write and delete, in order.
struct SpyStore {
    inner: MemoryStore,
    log: Mutex<Vec<String>>,
}

impl SecretStore for SpyStore {
    fn get(
        &self,
        name: &str,
    ) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        self.inner.get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), secret_store::SecretError> {
        self.log.lock().unwrap().push(format!("set {name}"));
        self.inner.set(name, value)
    }
    fn delete(&self, name: &str) -> Result<bool, secret_store::SecretError> {
        self.log.lock().unwrap().push(format!("delete {name}"));
        self.inner.delete(name)
    }
    fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
        self.inner.list_names()
    }
}

fn names(store: &Arc<dyn SecretStore>) -> Vec<String> {
    let mut n = store.list_names().unwrap();
    n.sort();
    n
}

fn token_ref(f: &Fx) -> String {
    let text = std::fs::read_to_string(f.file()).unwrap();
    let cfg: HomeAssistantConfig = toml::from_str(&text).unwrap();
    String::from(cfg.token)
}

#[tokio::test]
async fn a_new_address_gets_its_token_under_a_new_name_and_the_old_host_never_sees_it() {
    const TOKEN2: &str = "second-hub-token";
    let a = hub_with(TOKEN).await;
    let b = hub_with(TOKEN2).await;
    let spy = Arc::new(SpyStore {
        inner: MemoryStore::new(),
        log: Mutex::new(Vec::new()),
    });
    let f = start_with(tempfile::tempdir().unwrap(), spy.clone());
    f.host.save(input(a.port, Some(TOKEN))).await.unwrap();
    assert_eq!(token_ref(&f), "homeassistant/token");
    spy.log.lock().unwrap().clear();
    let calls_before = a.auth.lock().unwrap().len();

    f.host.save(input(b.port, Some(TOKEN2))).await.unwrap();
    // The name the live client of hub A reads was never written: the new token went to a new
    // name, and the old one was deleted only after the swap.
    let new_name = token_ref(&f);
    assert!(new_name.starts_with("homeassistant/token-"), "{new_name}");
    assert_eq!(
        *spy.log.lock().unwrap(),
        vec![
            format!("set {new_name}"),
            "delete homeassistant/token".to_owned()
        ]
    );
    assert_eq!(names(&f.store), vec![new_name.clone()]);
    // Hub A saw nothing of the new token; hub B got only the new one.
    assert_eq!(a.auth.lock().unwrap().len(), calls_before);
    assert!(
        b.auth
            .lock()
            .unwrap()
            .iter()
            .all(|h| h.ends_with(&format!("Bearer {TOKEN2}")))
    );
    // The live tools talk to hub B with the new token.
    f.registry
        .get("ha_list_entities")
        .unwrap()
        .call(&ToolContext::default(), json!({}))
        .await
        .unwrap();
    assert_eq!(a.auth.lock().unwrap().len(), calls_before);

    // A new token for the same address replaces it the same way, and the file follows.
    let third = hub_with("third-token").await;
    let mut again = input(third.port, Some("third-token"));
    again.trusted_services = vec!["cover.open_cover".into()];
    f.host.save(again).await.unwrap();
    let third_name = token_ref(&f);
    assert_ne!(third_name, new_name);
    assert_eq!(names(&f.store), vec![third_name]);
}

#[tokio::test]
async fn a_failed_save_of_a_new_address_leaves_the_old_secret_and_hub_alone() {
    const TOKEN2: &str = "second-hub-token";
    let a = hub_with(TOKEN).await;
    let b = hub_with(TOKEN2).await;
    let f = fx();
    f.host.save(input(a.port, Some(TOKEN))).await.unwrap();
    let before = std::fs::read_to_string(f.file()).unwrap();
    // The file cannot be written: its temporary name is taken by a directory.
    std::fs::create_dir(f.dir.path().join("homeassistant.toml.tmp")).unwrap();
    let err = f.host.save(input(b.port, Some(TOKEN2))).await.unwrap_err();
    assert!(
        matches!(err, HomeAssistantControlError::Internal(_)),
        "{err:?}"
    );
    assert_eq!(std::fs::read_to_string(f.file()).unwrap(), before);
    assert_eq!(names(&f.store), vec!["homeassistant/token".to_owned()]);
    assert_eq!(
        f.store
            .get("homeassistant/token")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN
    );
    assert!(f.visible(None), "hub A is still the live one");

    // The very first save fails the same way: no secret is left behind.
    let g = fx();
    std::fs::create_dir(g.dir.path().join("homeassistant.toml.tmp")).unwrap();
    assert!(g.host.save(input(a.port, Some(TOKEN))).await.is_err());
    assert_eq!(names(&g.store), Vec::<String>::new());
    assert!(!g.visible(None));
}

#[tokio::test]
async fn removing_keeps_the_secret_and_the_hub_when_the_file_cannot_be_removed() {
    use std::os::unix::fs::PermissionsExt;
    let hub = hub().await;
    let f = fx();
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    let dir = f.dir.path().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let restore = || std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    // A superuser can still delete: nothing to prove here.
    if std::fs::File::create(dir.join("probe")).is_ok() {
        restore().unwrap();
        return;
    }
    let err = f.host.remove().await.unwrap_err();
    restore().unwrap();
    assert!(
        matches!(err, HomeAssistantControlError::Internal(_)),
        "{err:?}"
    );
    assert!(f.file().exists());
    assert_eq!(
        f.store
            .get("homeassistant/token")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN
    );
    assert!(f.visible(None) && f.host.get().configured);
    // Nothing was lost, so the owner can simply try again.
    f.host.remove().await.unwrap();
    assert!(!f.file().exists() && f.store.list_names().unwrap().is_empty() && !f.visible(None));
}

#[tokio::test]
async fn removing_deletes_the_file_before_the_secret_and_the_secret_before_the_reload() {
    let hub = hub().await;
    let spy = Arc::new(SpyStore {
        inner: MemoryStore::new(),
        log: Mutex::new(Vec::new()),
    });
    let f = start_with(tempfile::tempdir().unwrap(), spy.clone());
    f.host.save(input(hub.port, Some(TOKEN))).await.unwrap();
    spy.log.lock().unwrap().clear();
    f.host.remove().await.unwrap();
    assert_eq!(*spy.log.lock().unwrap(), vec!["delete homeassistant/token"]);
    assert!(!f.file().exists() && !f.visible(None));
}

fn hand_written(body: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(HOMEASSISTANT_FILE), body).unwrap();
    start(dir)
}

#[tokio::test]
async fn a_hand_edited_file_is_cleaned_like_the_api_cleans_its_input() {
    let hub = hub().await;
    let head = format!(
        "url = \"http://127.0.0.1:{}\"\ntoken = \"my/own/secret\"\n",
        hub.port
    );
    // Mixed case and padding are lowercased and trimmed, as the API does.
    let f = hand_written(&format!(
        "{head}trusted_services = [\" Cover.Open_Cover \", \"cover.open_cover\"]\nallowed_domains = [\"Light\", \" cover \"]\nbots = [\" bot-a \"]\ntimeout_secs = 9999\n"
    ));
    f.store.set("my/own/secret", TOKEN).unwrap();
    assert!(f.visible(Some("bot-a")) && !f.visible(Some("bot-b")));
    let shown = f.host.get();
    assert_eq!(shown.trusted_services, vec!["cover.open_cover"]);
    assert_eq!(shown.bots, Some(vec!["bot-a".to_owned()]));
    let open = json!({"domain":"cover","service":"open_cover","entity_id":"cover.garage"});
    assert!(!f.asks(&open), "the lowercased entry trusts the service");
    // The domain list works in any case: `Light` allows `light`.
    let ctx = ToolContext {
        bot_id: Some("bot-a".into()),
        ..ToolContext::default()
    };
    f.registry
        .get("ha_call_service")
        .unwrap()
        .call(
            &ctx,
            json!({"domain":"light","service":"turn_on","entity_id":"light.kitchen"}),
        )
        .await
        .unwrap();
    let err = f
        .registry
        .get("ha_call_service")
        .unwrap()
        .call(
            &ctx,
            json!({"domain":"switch","service":"turn_on","entity_id":"switch.a"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("allowed_domains"), "{err}");
    // Saving from the screen writes the cleaned form back (and the timeout within range).
    f.host.save(input(hub.port, None)).await.unwrap();
    let file = std::fs::read_to_string(f.file()).unwrap();
    assert!(file.contains("timeout_secs = 60"), "{file}");
    assert!(
        file.contains("\"light\"") && !file.contains("Light"),
        "{file}"
    );
}

#[tokio::test]
async fn a_hand_edited_file_the_api_would_refuse_is_not_loaded_and_not_overwritten() {
    let hub = hub().await;
    let head = format!(
        "url = \"http://127.0.0.1:{}\"\ntoken = \"my/own/secret\"\n",
        hub.port
    );
    let many = |n: usize, what: &str| {
        (0..n)
            .map(|i| format!("\"{what}{i}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let long = "a".repeat(65);
    for (why, extra) in [
        (
            "generic service",
            "trusted_services = [\"homeassistant.turn_on\"]".to_owned(),
        ),
        (
            "generic service, cased",
            "trusted_services = [\" HomeAssistant.Turn_Off \"]".to_owned(),
        ),
        (
            "long name",
            format!("trusted_services = [\"cover.{long}\"]"),
        ),
        ("no dot", "trusted_services = [\"cover\"]".to_owned()),
        ("wildcard", "trusted_services = [\"cover.*\"]".to_owned()),
        (
            "too many services",
            format!("trusted_services = [{}]", many(101, "cover.s")),
        ),
        ("blank domain", "allowed_domains = [\"\"]".to_owned()),
        ("odd domain", "allowed_domains = [\"li ght\"]".to_owned()),
        (
            "too many domains",
            format!("allowed_domains = [{}]", many(101, "d")),
        ),
        ("too many bots", format!("bots = [{}]", many(501, "bot"))),
        ("long bot id", format!("bots = [\"{}\"]", "b".repeat(81))),
        ("blank bot id", "bots = [\"\"]".to_owned()),
    ] {
        let body = format!("{head}{extra}\n");
        let f = hand_written(&body);
        f.store.set("my/own/secret", TOKEN).unwrap();
        assert!(!f.visible(None), "{why}: must not load");
        assert!(!f.host.get().configured, "{why}");
        let err = f.host.save(input(hub.port, Some(TOKEN))).await.unwrap_err();
        assert!(
            matches!(err, HomeAssistantControlError::Internal(_)),
            "{why}: {err:?}"
        );
        assert_eq!(std::fs::read_to_string(f.file()).unwrap(), body, "{why}");
    }
}

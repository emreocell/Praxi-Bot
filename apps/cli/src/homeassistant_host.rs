//! Home Assistant of `bot serve`: `<data dir>/homeassistant.toml`, the `ha_*` tools and the
//! `/v1/homeassistant*` setup API (`daemon_api::HomeAssistantControl`).
//!
//! The access token lives in the secret store only; the file holds the reference
//! (`token = "homeassistant/token"`), never the value. The app, `bot ha setup` and a hand-edited
//! file are the same thing: the file is read at start and rewritten (comments are not kept) when
//! the setup is saved, limited to some bots or removed, and the live connector the tools use is
//! replaced at the same moment, so no restart is needed.
//!
//! ```toml
//! url = "http://homeassistant.local:8123"
//! token = "homeassistant/token"
//! allowed_domains = ["light", "switch", "climate"]   # optional: only these may be called
//! trusted_services = ["cover.open_cover"]            # optional: no card for these
//! bots = ["<bot id>"]                                 # optional: absent = every bot
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use connector_homeassistant::{
    HaError, HomeAssistantClient, HomeAssistantConfig, HomeAssistantConnector, hub_policy,
};
use daemon_api::homeassistant::{
    HomeAssistantControl, HomeAssistantControlError, HomeAssistantDto, HomeAssistantInput,
    HomeAssistantSaved, HomeAssistantTestInput, HomeAssistantTestReport,
};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use tool_registry::ToolRegistry;

/// Filename of the Home Assistant configuration in `<data_dir>`.
pub const HOMEASSISTANT_FILE: &str = "homeassistant.toml";

/// The secret this host stores the access token under.
const TOKEN_SECRET: &str = "homeassistant/token";

const FILE_HEADER: &str = "# Home Assistant - edited via Apps > Home Assistant, `bot ha` or by hand.\n# The access token lives in the secret store; only its name belongs here.\n\n";

/// Why the file could not be read.
enum Loaded {
    Ok(Option<HomeAssistantConfig>),
    Broken(String),
}

fn load(path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<HomeAssistantConfig>(&text) {
            Ok(cfg) => Loaded::Ok(Some(cfg)),
            // The parser's own text quotes the offending line, which may hold a pasted key.
            Err(e) => Loaded::Broken(format!("{}: not valid ({})", path.display(), e.message())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::Ok(None),
        Err(e) => Loaded::Broken(format!("cannot read {}: {e}", path.display())),
    }
}

fn warn(message: &str) {
    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {message}");
}

/// Manages `homeassistant.toml`, the token in the secret store and the live
/// [`HomeAssistantConnector`] the registered tools use.
pub struct HomeAssistantHost {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    connector: Arc<HomeAssistantConnector>,
    /// Serialises changes (read-modify-write of the file and the connector).
    ops: tokio::sync::Mutex<()>,
}

/// Register the Home Assistant tools (hidden from bots while no hub is usable) and return the
/// setup host. `None` without a secret store. `daemon_port` is the daemon's own API port: a hub
/// address may never point at it.
pub fn start_homeassistant(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
    daemon_port: u16,
) -> Option<Arc<HomeAssistantHost>> {
    let store = store?;
    let path = data_dir.join(HOMEASSISTANT_FILE);
    let config = match load(&path) {
        Loaded::Ok(c) => c,
        Loaded::Broken(why) => {
            warn(&why);
            None
        }
    };
    let connector = Arc::new(HomeAssistantConnector::new(hub_policy(
        &crate::serve::daemon_protected_sockets(daemon_port),
    )));
    if let Some(p) = connector.replace(config, store) {
        warn(&format!("home assistant: {p}"));
    }
    let added = connector_homeassistant::register(registry, &connector);
    warn(&format!("home assistant: {added} tool(s) registered"));
    Some(Arc::new(HomeAssistantHost {
        path,
        secrets: Arc::clone(store),
        connector,
        ops: tokio::sync::Mutex::new(()),
    }))
}

fn invalid(m: impl Into<String>) -> HomeAssistantControlError {
    HomeAssistantControlError::Invalid(m.into())
}

fn internal(m: impl Into<String>) -> HomeAssistantControlError {
    HomeAssistantControlError::Internal(m.into())
}

fn not_set_up() -> HomeAssistantControlError {
    HomeAssistantControlError::NotFound("Home Assistant is not set up".into())
}

/// The plain-words version of a failed hub call.
fn friendly(e: &HaError) -> String {
    let text = e.to_string();
    if text.contains("refused the access token") {
        "Home Assistant refused the access key. Create a new long-lived access key in your Home Assistant profile (Security) and try again.".to_owned()
    } else if text.contains("redirect") {
        "Home Assistant answered with a redirect. Use its final address (for example the https one).".to_owned()
    } else if text.contains("not found") || text.contains("does not look like") {
        "That address does not look like Home Assistant. Check it, including the port (usually 8123).".to_owned()
    } else if text.contains("error sending request") || text.contains("timed out") {
        "Could not reach Home Assistant. Check the address, and that this computer can reach it."
            .to_owned()
    } else {
        text
    }
}

/// An owner-listed service: exactly `domain.service`, lowercase, never the generic
/// `homeassistant.*` (that one fans out to entities nobody can see from here).
fn clean_service(raw: &str) -> Result<String, HomeAssistantControlError> {
    let s = raw.trim().to_ascii_lowercase();
    let name_ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 64
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    match s.split_once('.') {
        Some((d, v)) if name_ok(d) && name_ok(v) => {
            if d == "homeassistant" {
                Err(invalid(
                    "`homeassistant.*` services cannot be allowed: they act on whatever they are pointed at",
                ))
            } else {
                Ok(s)
            }
        }
        _ => Err(invalid(format!(
            "`{}` is not a service name: write it as domain.service, for example cover.open_cover",
            raw.trim()
        ))),
    }
}

fn clean_services(raw: &[String]) -> Result<Vec<String>, HomeAssistantControlError> {
    let mut out: Vec<String> = Vec::new();
    for r in raw.iter().filter(|r| !r.trim().is_empty()) {
        let s = clean_service(r)?;
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

fn clean_bots(raw: Option<Vec<String>>) -> Result<Option<Vec<String>>, HomeAssistantControlError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    for id in raw {
        let id = id.trim().to_owned();
        if id.is_empty() || id.len() > 80 || id.chars().any(char::is_control) {
            return Err(invalid("a bot id is 1 to 80 characters"));
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(Some(out))
}

/// Everything the token is sent to: scheme, host, port and path. If any changes, the stored
/// token must not be reused (that would let an API caller point the write-only token at a
/// server of their choosing).
fn same_target(a: &str, b: &str) -> bool {
    let key = |x: &str| {
        let url = url::Url::parse(x.trim()).ok()?;
        Some((
            url.scheme().to_owned(),
            url.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
            url.port_or_known_default(),
            url.path().trim_end_matches('/').to_owned(),
        ))
    };
    matches!((key(a), key(b)), (Some(x), Some(y)) if x == y)
}

fn dto(cfg: Option<&HomeAssistantConfig>, store: &Arc<dyn SecretStore>) -> HomeAssistantDto {
    match cfg {
        None => HomeAssistantDto {
            configured: false,
            url: None,
            token_set: false,
            trusted_services: Vec::new(),
            bots: None,
        },
        Some(c) => {
            let name: String = c.token.clone().into();
            HomeAssistantDto {
                configured: true,
                url: Some(c.url.clone()),
                token_set: store.get(&name).ok().flatten().is_some(),
                trusted_services: c.trusted_services.clone(),
                bots: c.bots.clone(),
            }
        }
    }
}

impl HomeAssistantHost {
    fn config(&self) -> Result<Option<HomeAssistantConfig>, HomeAssistantControlError> {
        match load(&self.path) {
            Loaded::Ok(c) => Ok(c),
            // Never overwrite a file the owner edited and we cannot read.
            Loaded::Broken(why) => Err(internal(format!(
                "{why}; fix or remove the file, then try again"
            ))),
        }
    }

    fn persist(&self, cfg: &HomeAssistantConfig) -> Result<(), HomeAssistantControlError> {
        let body = toml::to_string_pretty(cfg)
            .map_err(|e| internal(format!("cannot serialize {HOMEASSISTANT_FILE}: {e}")))?;
        let tmp = self.path.with_extension("toml.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(FILE_HEADER.as_bytes())?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            internal(format!("cannot save {}: {e}", self.path.display()))
        })
    }

    fn reload(&self, cfg: Option<HomeAssistantConfig>) {
        if let Some(p) = self.connector.replace(cfg, &self.secrets) {
            warn(&format!("home assistant: {p}"));
        }
    }

    async fn set_secret(
        &self,
        name: String,
        value: String,
    ) -> Result<(), HomeAssistantControlError> {
        let store = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || store.set(&name, &value))
            .await
            .map_err(|e| internal(format!("secret task failed: {e}")))?
            .map_err(|e| internal(format!("cannot store the token: {e}")))
    }

    async fn drop_secret(&self, name: String) -> Result<(), HomeAssistantControlError> {
        let store = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || store.delete(&name))
            .await
            .map_err(|e| internal(format!("secret task failed: {e}")))?
            .map(|_| ())
            .map_err(|e| internal(format!("cannot delete the stored key: {e}")))
    }

    /// The value of a secret: `Ok(None)` when there is none, an error when the store cannot be
    /// read (which is not the same thing: a restore must not delete what it never saw).
    async fn read_secret(&self, name: String) -> Result<Option<String>, HomeAssistantControlError> {
        let store = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || store.get(&name))
            .await
            .map_err(|e| internal(format!("secret task failed: {e}")))?
            .map(|v| v.map(|v| v.expose().to_string()))
            .map_err(|e| internal(format!("cannot read the stored key: {e}")))
    }

    /// Put a secret back as it was before a change that did not complete.
    async fn restore_secret(&self, name: String, previous: Option<String>) {
        let done = match previous {
            Some(old) => self.set_secret(name, old).await,
            None => self.drop_secret(name).await,
        };
        if let Err(HomeAssistantControlError::Internal(why)) = done {
            warn(&format!("home assistant: {why}"));
        }
    }

    /// Ask the hub, with a token that is only in memory (a test, or a candidate not saved yet).
    async fn probe(&self, mut cfg: HomeAssistantConfig, token: &str) -> Result<u32, HaError> {
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        cfg.token = SecretRef::new(TOKEN_SECRET).map_err(|e| HaError::Invalid(e.to_string()))?;
        store
            .set(TOKEN_SECRET, token)
            .map_err(|e| HaError::Credential(e.to_string()))?;
        let client = HomeAssistantClient::with_policy(cfg, store, self.connector.policy())?;
        let probe = client.probe().await?;
        Ok(u32::try_from(probe.entities).unwrap_or(u32::MAX))
    }
}

fn candidate(url: &str) -> HomeAssistantConfig {
    HomeAssistantConfig {
        url: url.trim().to_owned(),
        token: SecretRef::new(TOKEN_SECRET).unwrap_or_else(|_| unreachable!("a valid name")),
        allowed_domains: Vec::new(),
        timeout_secs: 15,
        trusted_services: Vec::new(),
        bots: None,
    }
}

#[async_trait]
impl HomeAssistantControl for HomeAssistantHost {
    fn get(&self) -> HomeAssistantDto {
        match load(&self.path) {
            Loaded::Ok(c) => dto(c.as_ref(), &self.secrets),
            Loaded::Broken(why) => {
                warn(&why);
                dto(None, &self.secrets)
            }
        }
    }

    async fn save(
        &self,
        input: HomeAssistantInput,
    ) -> Result<HomeAssistantSaved, HomeAssistantControlError> {
        let _guard = self.ops.lock().await;
        let saved = self.config()?;
        let mut cfg = candidate(&input.url);
        cfg.trusted_services = clean_services(&input.trusted_services)?;
        // What the owner wrote by hand and the screen does not show stays as it was.
        if let Some(old) = &saved {
            cfg.allowed_domains.clone_from(&old.allowed_domains);
            cfg.timeout_secs = old.timeout_secs;
            cfg.bots.clone_from(&old.bots);
        }
        let new_token = input.token.clone().filter(|t| !t.is_empty());
        // The token is write-only: without a new one the stored one is only ever sent to the
        // address it was saved with.
        let token = if let Some(t) = &new_token {
            t.clone()
        } else {
            let old = saved
                .as_ref()
                .filter(|o| same_target(&o.url, &cfg.url))
                .ok_or_else(|| {
                    invalid(if saved.is_some() {
                        "changing the address requires entering the access key again"
                    } else {
                        "an access key is needed"
                    })
                })?;
            cfg.token = old.token.clone();
            self.read_secret(String::from(old.token.clone()))
                .await?
                .ok_or_else(|| invalid("no access key is stored; enter it again"))?
        };
        // Nothing is saved until the hub accepted the address and the token.
        let entities = self
            .probe(cfg.clone(), &token)
            .await
            .map_err(|e| invalid(friendly(&e)))?;
        // A new token always goes to this host's own secret; without one the saved reference
        // is kept. New token first: if storing it fails nothing has changed, and if saving the
        // file fails the old value is put back.
        let previous = if new_token.is_some() {
            let before = self.read_secret(TOKEN_SECRET.to_owned()).await?;
            self.set_secret(TOKEN_SECRET.to_owned(), token).await?;
            cfg.token = SecretRef::new(TOKEN_SECRET).map_err(|e| internal(e.to_string()))?;
            Some(before)
        } else {
            None
        };
        if let Err(e) = self.persist(&cfg) {
            if let Some(before) = previous {
                self.restore_secret(TOKEN_SECRET.to_owned(), before).await;
            }
            return Err(e);
        }
        self.reload(Some(cfg.clone()));
        Ok(HomeAssistantSaved {
            settings: dto(Some(&cfg), &self.secrets),
            entities,
        })
    }

    async fn set_bots(
        &self,
        bots: Option<Vec<String>>,
    ) -> Result<HomeAssistantDto, HomeAssistantControlError> {
        let _guard = self.ops.lock().await;
        let mut cfg = self.config()?.ok_or_else(not_set_up)?;
        cfg.bots = clean_bots(bots)?;
        self.persist(&cfg)?;
        self.reload(Some(cfg.clone()));
        Ok(dto(Some(&cfg), &self.secrets))
    }

    async fn remove(&self) -> Result<(), HomeAssistantControlError> {
        let _guard = self.ops.lock().await;
        let gone = self.config()?.ok_or_else(not_set_up)?;
        // The stored key first: if it cannot be deleted nothing has changed and the owner can
        // try again, instead of a key left behind in the secure store with no setup naming it.
        // Only the secret this host made goes: a hand-written reference may name one that
        // something else still uses.
        if String::from(gone.token.clone()) == TOKEN_SECRET {
            self.drop_secret(TOKEN_SECRET.to_owned()).await?;
        }
        std::fs::remove_file(&self.path)
            .map_err(|e| internal(format!("cannot remove {}: {e}", self.path.display())))?;
        self.reload(None);
        Ok(())
    }

    async fn test(
        &self,
        input: HomeAssistantTestInput,
    ) -> Result<HomeAssistantTestReport, HomeAssistantControlError> {
        let fail = |error: String| HomeAssistantTestReport {
            ok: false,
            entities: None,
            error: Some(error),
        };
        let result = match (&input.url, &input.token) {
            // The saved hub is always tried at its own address with its own token: a stored
            // token never goes to an address the caller supplies.
            (None, None) => {
                let cfg = self.config()?.ok_or_else(not_set_up)?;
                let client = HomeAssistantClient::with_policy(
                    cfg,
                    Arc::clone(&self.secrets),
                    self.connector.policy(),
                );
                match client {
                    Ok(c) => c.probe().await.map(|p| p.entities),
                    Err(e) => Err(e),
                }
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
            }
            (Some(url), Some(token)) => self.probe(candidate(url), token).await,
            _ => {
                return Err(invalid(
                    "give the address and the access key together, or neither to test the saved hub",
                ));
            }
        };
        Ok(match result {
            Ok(entities) => HomeAssistantTestReport {
                ok: true,
                entities: Some(entities),
                error: None,
            },
            Err(e) => fail(friendly(&e)),
        })
    }
}

#[cfg(test)]
#[path = "homeassistant_host_tests.rs"]
mod tests;

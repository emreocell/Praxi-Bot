//! `bot channels telegram ...` and the Telegram channel the daemon runs.
//!
//! The bot token lives in the secret store (never in a file, never printed);
//! `<data dir>/channels.toml` only holds the on/off switch.  `bot serve` starts
//! the channel in-process when a token is stored and the channel is enabled; it
//! talks to the daemon through the [`channel_telegram::DaemonClient`] trait
//! ([`channel_telegram::HttpDaemonClient`] over the daemon's own loopback
//! address and bearer token).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use channel_telegram::{
    HttpDaemonClient, PAIR_CODE_TTL, PairingStore, TELEGRAM_BOT_TOKEN_SECRET, TelegramChannel,
    TelegramConfig, store_bot_token,
};
use daemon_api::ChannelControl;
use daemon_api::dto::{ChannelDto, PairCodeDto};
use reqwest::Method;
use secret_store::{SecretStore, SecretValue};
use serde_json::{Value, json};

use crate::Result;
use crate::client::Client;

/// Name of the channel settings file in the data dir.
pub const CHANNELS_FILE: &str = "channels.toml";
/// Name of the Telegram state file (paired users, chat mapping, offset).
pub const TELEGRAM_STATE_FILE: &str = "telegram_state.json";

fn channels_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CHANNELS_FILE)
}

/// Whether the Telegram channel is switched on (default: on, so that a token
/// stored by other means is picked up).
#[must_use]
pub fn telegram_enabled(data_dir: &Path) -> bool {
    std::fs::read_to_string(channels_path(data_dir))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .and_then(|t| t.get("telegram")?.get("enabled")?.as_bool())
        .unwrap_or(true)
}

/// Switch the Telegram channel on or off.
///
/// # Errors
/// The file cannot be written.
pub fn set_telegram_enabled(data_dir: &Path, enabled: bool) -> Result<()> {
    let mut telegram = toml::Table::new();
    telegram.insert("enabled".into(), toml::Value::Boolean(enabled));
    let mut root = toml::Table::new();
    root.insert("telegram".into(), toml::Value::Table(telegram));
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(channels_path(data_dir), toml::to_string(&root)?)?;
    Ok(())
}

/// Whether a (non-empty) bot token is stored.
///
/// # Errors
/// Store errors.
pub fn telegram_configured(store: &dyn SecretStore) -> Result<bool> {
    Ok(store
        .get(TELEGRAM_BOT_TOKEN_SECRET)?
        .is_some_and(|t| !t.expose().trim().is_empty()))
}

/// Number of paired Telegram users recorded on disk.
#[must_use]
pub fn paired_users_on_disk(data_dir: &Path) -> usize {
    let path = data_dir.join(TELEGRAM_STATE_FILE);
    if !path.exists() {
        return 0;
    }
    PairingStore::open(path).map_or(0, |p| p.paired_users().len())
}

/// `bot channels telegram setup`: store the token (shown nowhere) and enable.
///
/// # Errors
/// Empty token, store or file errors.
pub fn telegram_setup(
    store: &dyn SecretStore,
    data_dir: &Path,
    token: &SecretValue,
    out: &mut dyn FnMut(&str),
) -> Result<()> {
    store_bot_token(store, token.expose())?;
    set_telegram_enabled(data_dir, true)?;
    out("Telegram bot token stored; the channel is enabled.");
    out("Restart `bot serve`, then run `bot channels telegram pair`.");
    Ok(())
}

/// `bot channels telegram disable`.
///
/// # Errors
/// The settings file cannot be written.
pub fn telegram_disable(data_dir: &Path, out: &mut dyn FnMut(&str)) -> Result<()> {
    set_telegram_enabled(data_dir, false)?;
    out("Telegram channel disabled (the token stays stored); restart `bot serve` to stop it.");
    Ok(())
}

/// `bot channels telegram pair`: ask the running daemon for a one-time code.
///
/// # Errors
/// Daemon errors (not running, channel not running: exit codes as elsewhere).
pub async fn telegram_pair(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let v = c
        .request(Method::POST, "/v1/channels/telegram/pair-code", None)
        .await?;
    let code = v["code"].as_str().unwrap_or("?");
    let minutes = v["expires_in_secs"].as_u64().unwrap_or(0) / 60;
    if json_out {
        writeln!(out, "{}", serde_json::to_string(&v)?)?;
        return Ok(());
    }
    writeln!(
        out,
        "Pairing code: {code}  (valid for {minutes} minutes, one use)"
    )?;
    writeln!(out, "In Telegram, open your bot and send:  /pair {code}")?;
    Ok(())
}

/// `bot channels telegram status`: `daemon` is the daemon's view of the
/// channel (`None` when the daemon is not reachable).
///
/// # Errors
/// Output errors.
pub fn telegram_status(
    configured: bool,
    enabled: bool,
    daemon: Option<&Value>,
    paired_on_disk: usize,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let running = daemon.is_some_and(|d| d["running"] == true);
    let paired = daemon
        .and_then(|d| d["paired_users"].as_u64())
        .map_or(paired_on_disk, |n| usize::try_from(n).unwrap_or(usize::MAX));
    if json_out {
        let v = json!({
            "configured": configured, "enabled": enabled,
            "daemon_reachable": daemon.is_some(),
            "running": running, "paired_users": paired,
        });
        writeln!(out, "{}", serde_json::to_string(&v)?)?;
        return Ok(());
    }
    let yn = |b: bool| if b { "yes" } else { "no" };
    writeln!(out, "configured: {}", yn(configured))?;
    writeln!(out, "enabled: {}", yn(enabled))?;
    writeln!(
        out,
        "running: {}",
        if running {
            "yes"
        } else if daemon.is_some() {
            "no (the daemon is up but the channel is not running; restart `bot serve`)"
        } else {
            "no (daemon not reachable)"
        }
    )?;
    writeln!(out, "paired users: {paired}")?;
    Ok(())
}

/// The Telegram channel inside the daemon (see the module docs).
pub struct TelegramRuntime {
    configured: bool,
    enabled: bool,
    data_dir: PathBuf,
    channel: parking_lot::Mutex<Option<TelegramChannel>>,
    config: Option<TelegramConfig>,
    client: Arc<HttpDaemonClient>,
}

impl TelegramRuntime {
    /// Stop polling and wait for the task to end.
    pub async fn shutdown(&self) {
        let channel = self.channel.lock().take();
        if let Some(c) = channel {
            c.shutdown().await;
        }
    }
}

#[async_trait::async_trait]
impl ChannelControl for TelegramRuntime {
    fn start(&self) {
        let mut channel = self.channel.lock();
        if channel
            .as_ref()
            .is_none_or(|c| c.status() != channel_telegram::ChannelStatus::Running)
            && let Some(config) = &self.config
        {
            match TelegramChannel::start(config.clone(), self.client.clone()) {
                Ok(c) => *channel = Some(c),
                Err(e) => eprintln!("warning: telegram channel not started: {e}"),
            }
        }
    }
    async fn stop(&self) {
        self.shutdown().await;
    }

    fn info(&self) -> ChannelDto {
        let channel = self.channel.lock();
        ChannelDto {
            status: Some(
                match channel.as_ref().map(TelegramChannel::status) {
                    Some(channel_telegram::ChannelStatus::Running) => "running",
                    Some(channel_telegram::ChannelStatus::Conflict) => "conflict",
                    _ => "stopped",
                }
                .into(),
            ),
            id: "telegram".into(),
            configured: self.configured,
            enabled: self.enabled,
            running: channel
                .as_ref()
                .is_some_and(|c| c.status() == channel_telegram::ChannelStatus::Running),
            paired_users: channel.as_ref().map_or_else(
                || paired_users_on_disk(&self.data_dir),
                |c| c.pairing().paired_users().len(),
            ),
        }
    }

    fn create_pair_code(&self) -> std::result::Result<PairCodeDto, String> {
        match &*self.channel.lock() {
            Some(c) => Ok(PairCodeDto {
                code: c.create_pair_code(),
                expires_in_secs: PAIR_CODE_TTL.as_secs(),
            }),
            None if !self.configured => {
                Err("telegram is not configured; run `bot channels telegram setup`".into())
            }
            None if !self.enabled => {
                Err("telegram is disabled; enable it with `bot channels telegram setup`".into())
            }
            None => Err("the telegram channel is not running".into()),
        }
    }
}

/// Start the channel when a token is stored and the channel is enabled.
/// Must run inside a Tokio runtime.  `api_base_url` overrides the Telegram
/// endpoint (tests only).  A start failure is reported on stderr and leaves the
/// channel not running; the daemon carries on.
#[cfg(test)]
#[must_use]
pub fn start_telegram(
    data_dir: &Path,
    store: Arc<dyn SecretStore>,
    daemon_url: &str,
    daemon_token: &str,
    api_base_url: Option<&str>,
) -> Arc<TelegramRuntime> {
    let runtime = telegram_runtime(
        data_dir,
        store,
        daemon_url,
        daemon_token,
        api_base_url,
        None,
    );
    runtime.start();
    runtime
}

pub fn telegram_runtime(
    data_dir: &Path,
    store: Arc<dyn SecretStore>,
    daemon_url: &str,
    daemon_token: &str,
    api_base_url: Option<&str>,
    leader: Option<daemon_api::host::LeaderControl>,
) -> Arc<TelegramRuntime> {
    let configured = telegram_configured(store.as_ref()).unwrap_or(false);
    let enabled = telegram_enabled(data_dir);
    let config = if configured && enabled {
        let mut cfg =
            TelegramConfig::new(store).with_state_path(data_dir.join(TELEGRAM_STATE_FILE));
        if let Some(url) = api_base_url {
            cfg = cfg.with_api_base_url(url);
        }
        if let Some(leader) = leader {
            cfg.on_conflict = Some(Arc::new(move || {
                let leader = leader.clone();
                tokio::spawn(async move {
                    if let Err(e) = leader.telegram_conflict().await {
                        eprintln!("host conflict: {e}");
                    }
                });
            }));
        }
        Some(cfg)
    } else {
        None
    };
    Arc::new(TelegramRuntime {
        configured,
        enabled,
        data_dir: data_dir.to_path_buf(),
        channel: parking_lot::Mutex::new(None),
        config,
        client: Arc::new(HttpDaemonClient::new(
            daemon_url,
            SecretValue::new(daemon_token),
        )),
    })
}

#[cfg(test)]
mod conflict_status_tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    #[tokio::test]
    async fn terminal_conflict_is_visible_through_runtime_info_and_can_restart() {
        let server = MockServer::start().await;
        let polls = Arc::new(AtomicUsize::new(0));
        let count = polls.clone();
        Mock::given(method("POST"))
            .respond_with(move |_: &wiremock::Request| {
                if count.fetch_add(1, Ordering::SeqCst) < 4 {
                    ResponseTemplate::new(409).set_body_json(json!({"ok":false,"error_code":409}))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({"ok":true,"result":[]}))
                }
            })
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let mut config =
            TelegramConfig::with_in_memory_token("test-token").with_api_base_url(server.uri());
        config.conflict_backoff = Duration::from_millis(1);
        let runtime = TelegramRuntime {
            configured: true,
            enabled: true,
            data_dir: dir.path().into(),
            channel: parking_lot::Mutex::new(None),
            config: Some(config),
            client: Arc::new(HttpDaemonClient::new(
                "http://127.0.0.1:1",
                SecretValue::new("local-token"),
            )),
        };
        runtime.start();
        tokio::time::timeout(Duration::from_secs(3), async {
            while runtime.info().status.as_deref() != Some("conflict") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!runtime.info().running);
        runtime.start();
        assert_eq!(runtime.info().status.as_deref(), Some("running"));
        runtime.shutdown().await;
    }
}

//! `bot channels discord ...` and the Discord channel the daemon runs.
//!
//! Same shape as Telegram: the bot token lives in the secret store, the on/off
//! switch in `<data dir>/channels.toml`, the paired owner and allowed server
//! channels in `<data dir>/discord_state.json`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use channel_discord::rest::BotUser;
use channel_discord::{
    DISCORD_BOT_TOKEN_SECRET, DiscordChannel, DiscordConfig, DiscordError, DiscordRest,
    DiscordState, Lang, store_bot_token,
};
use channel_telegram::HttpDaemonClient;
use daemon_api::ChannelControl;
use daemon_api::connection_test::{ConnectionOutcome as O, ConnectionTestReport as R};
use daemon_api::dto::{ChannelDto, PairCodeDto};
use reqwest::Method;
use secret_store::{SecretStore, SecretValue};
use serde_json::{Value, json};

use crate::Result;
use crate::channels::{channels_enabled, set_channel_enabled};
use crate::client::Client;

/// Name of the Discord state file (owner, allowed channels, chat mapping).
pub const DISCORD_STATE_FILE: &str = "discord_state.json";
const CHANNEL: &str = "discord";

/// Is the Discord channel switched on (default: on once a token is stored)?
#[must_use]
pub fn discord_enabled(data_dir: &Path) -> bool {
    channels_enabled(data_dir, CHANNEL)
}

/// Is a (non-empty) bot token stored?
///
/// # Errors
/// Store errors.
pub fn discord_configured(store: &dyn SecretStore) -> Result<bool> {
    Ok(store
        .get(DISCORD_BOT_TOKEN_SECRET)?
        .is_some_and(|t| !t.expose().trim().is_empty()))
}

/// Paired accounts recorded on disk (0 or 1).
#[must_use]
pub fn discord_paired_on_disk(data_dir: &Path) -> usize {
    let path = data_dir.join(DISCORD_STATE_FILE);
    if !path.exists() {
        return 0;
    }
    DiscordState::open(path).map_or(0, |s| s.paired_count())
}

/// The plain-words steps shown before the token is asked for.
pub const SETUP_STEPS: &str = "\
How to connect Discord (about 5 minutes):
  1. Open https://discord.com/developers/applications and press \"New Application\".
  2. Open \"Bot\" in the left menu, press \"Reset Token\" and copy the token.
  3. On the same page, under \"Privileged Gateway Intents\", switch ON \"Message Content Intent\".
  4. Paste the token below. It is stored in your secret store and never shown again.";

fn localized<'a>(lang: Lang, en: &'a str, tr: &'a str, de: &'a str) -> &'a str {
    match lang {
        Lang::En => en,
        Lang::Tr => tr,
        Lang::De => de,
    }
}

pub fn setup_steps(lang: Lang) -> &'static str {
    localized(
        lang,
        SETUP_STEPS,
        "Discord'u bağlama (yaklaşık 5 dakika):\n  1. https://discord.com/developers/applications adresini açıp New Application düğmesine basın.\n  2. Bot bölümünü açıp Reset Token düğmesine basın ve jetonu kopyalayın.\n  3. Aynı sayfada Privileged Gateway Intents altında Message Content Intent seçeneğini açın.\n  4. Jetonu aşağıya yapıştırın. Gizli bilgi kasasına kaydedilir ve tekrar gösterilmez.",
        "Discord verbinden (etwa 5 Minuten):\n  1. Öffnen Sie https://discord.com/developers/applications und wählen Sie New Application.\n  2. Öffnen Sie Bot, wählen Sie Reset Token und kopieren Sie den Token.\n  3. Aktivieren Sie dort unter Privileged Gateway Intents die Option Message Content Intent.\n  4. Fügen Sie den Token unten ein. Er wird im Geheimnisspeicher abgelegt und nicht erneut angezeigt.",
    )
}

pub fn saved_language(data_dir: &Path) -> Lang {
    std::fs::read_to_string(data_dir.join("channels.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get(CHANNEL)?
                .get("language")?
                .as_str()
                .and_then(Lang::from_code)
        })
        .unwrap_or_default()
}

pub fn save_language(data_dir: &Path, lang: Lang) -> Result<()> {
    let path = data_dir.join("channels.toml");
    let mut table = std::fs::read_to_string(&path)?.parse::<toml::Table>()?;
    table
        .get_mut(CHANNEL)
        .and_then(toml::Value::as_table_mut)
        .ok_or("Discord settings are missing")?
        .insert("language".into(), lang.code().into());
    std::fs::write(path, toml::to_string(&table)?)?;
    Ok(())
}

/// Steps shown after a successful test.
fn next_steps(invite: &str, lang: Lang) -> String {
    if lang != Lang::En {
        return format!(
            "{}\n{invite}\n{}",
            localized(
                lang,
                "",
                "Botu yönettiğiniz bir sunucuya ekleyin; ardından üye listesinden botun profilini açıp Mesaj seçeneğini seçin:",
                "Fügen Sie den Bot über diesen Link einem eigenen Server hinzu. Öffnen Sie dann sein Profil in der Mitgliederliste und wählen Sie Nachricht:"
            ),
            localized(
                lang,
                "",
                "Ardından `bot serve` hizmetini yeniden başlatın ve `bot channels discord pair` komutunu çalıştırın. Kodu botla ÖZEL sohbette `!pair <kod>` yazarak gönderin.",
                "Starten Sie danach `bot serve` neu und führen Sie `bot channels discord pair` aus. Senden Sie den Code dem Bot PRIVAT mit `!pair <Code>`."
            )
        );
    }
    format!(
        "Next:\n  1. Add the bot to a server you manage, then open its profile in the member list and choose Message: {invite}\n  \
         2. Restart `bot serve`, then run `bot channels discord pair`.\n  \
         3. Send the code to the bot in a private message: `!pair <code>`."
    )
}

/// Store the token and enable the channel.
///
/// # Errors
/// Bad token, store or file errors.
pub fn discord_setup(
    store: &dyn SecretStore,
    data_dir: &Path,
    token: &SecretValue,
    json_out: bool,
    out: &mut dyn FnMut(&str),
) -> Result<()> {
    store_bot_token(store, token.expose())?;
    set_channel_enabled(data_dir, CHANNEL, true)?;
    if json_out {
        out(&json!({
            "channel": CHANNEL, "configured": true, "enabled": true, "restart_required": true,
        })
        .to_string());
    } else {
        out("Discord bot token stored; the channel is enabled.");
    }
    Ok(())
}

/// Switch Discord off, keeping the token.
///
/// # Errors
/// The settings file cannot be written.
pub fn discord_disable(data_dir: &Path, json_out: bool, out: &mut dyn FnMut(&str)) -> Result<()> {
    set_channel_enabled(data_dir, CHANNEL, false)?;
    if json_out {
        out(&json!({"channel": CHANNEL, "enabled": false, "restart_required": true}).to_string());
    } else {
        out("Discord channel disabled (the token stays stored); restart `bot serve` to stop it.");
    }
    Ok(())
}

/// Ask the running daemon for a pairing code.
///
/// # Errors
/// Daemon errors.
pub async fn discord_pair(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let v = c
        .request(Method::POST, "/v1/channels/discord/pair-code", None)
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
    writeln!(
        out,
        "In Discord, open a PRIVATE chat with your bot and send:  !pair {code}"
    )?;
    writeln!(
        out,
        "Never post the code in a server. Only the account that sends it becomes the owner."
    )?;
    Ok(())
}

/// Print what is known about the channel.
///
/// # Errors
/// Output errors.
pub fn discord_status(
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
    writeln!(out, "paired owner: {}", yn(paired > 0))?;
    Ok(())
}

/// Check the stored token against Discord, in plain words.
pub async fn check_token(store: &dyn SecretStore, api_base_url: &str) -> (R, Option<BotUser>) {
    let token = match store.get(DISCORD_BOT_TOKEN_SECRET) {
        Ok(Some(t)) if !t.expose().trim().is_empty() => t,
        Ok(_) => {
            return (
                R::failed(
                    "channel",
                    O::BadKey,
                    None,
                    None,
                    "no Discord bot token is saved",
                ),
                None,
            );
        }
        Err(e) => {
            return (
                R::failed("channel", O::Unknown, None, None, e.to_string()),
                None,
            );
        }
    };
    let rest = DiscordRest::new(SecretValue::new(token.expose()), api_base_url);
    let me = match rest.get_me().await {
        Ok(me) => me,
        Err(DiscordError::Api { status, message }) => {
            let outcome = match status {
                401 | 403 => O::BadKey,
                s => daemon_api::connection_test::classify_http_status(s),
            };
            return (
                R::failed("channel", outcome, Some(status), None, message),
                None,
            );
        }
        Err(DiscordError::RateLimited { retry_after_ms }) => {
            return (
                R::failed(
                    "channel",
                    O::QuotaExceeded,
                    Some(429),
                    None,
                    format!("Discord asked to wait {} s", retry_after_ms.div_ceil(1000)),
                ),
                None,
            );
        }
        Err(e) => {
            return (
                R::failed("channel", O::NoNetwork, None, None, e.to_string()),
                None,
            );
        }
    };
    // The privileged switch cannot be seen from the token alone, but Discord reports it.
    let Ok(enabled) = rest.message_content_enabled().await else {
        return (
            R::failed(
                "channel",
                O::Unknown,
                None,
                None,
                "the token works, but Discord's Message Content setting could not be checked; try again",
            ),
            Some(me),
        );
    };
    if !enabled {
        return (
            R::failed(
                "channel",
                O::Unknown,
                None,
                Some("message_content_intent_off".into()),
                format!(
                    "the token belongs to the bot {}, but \"Message Content Intent\" is switched off. \
                     Turn it on in the Discord Developer Portal (Bot, Privileged Gateway Intents), then try again.",
                    me.username
                ),
            ),
            Some(me),
        );
    }
    (
        R::ok(
            "channel",
            format!("token belongs to the bot {}", me.username),
        ),
        Some(me),
    )
}

/// The setup flow after the token is stored: test it and print the next steps.
pub async fn discord_setup_check(
    store: &dyn SecretStore,
    json_out: bool,
    lang: Lang,
    out: &mut dyn FnMut(&str),
) -> Result<()> {
    let (report, me) = check_token(store, channel_discord::DEFAULT_API_URL).await;
    if json_out {
        out(&serde_json::to_string(&report).unwrap_or_default());
        return if report.outcome == O::Ok {
            Ok(())
        } else {
            Err("Discord connection test failed; see the report".into())
        };
    }
    if report.outcome == O::Ok {
        out(localized(
            lang,
            "Connection test: OK.",
            "Bağlantı testi başarılı.",
            "Verbindungstest erfolgreich.",
        ));
        let id = me.map_or_else(String::new, |m| m.id);
        let invite = format!(
            "https://discord.com/oauth2/authorize?client_id={id}&scope=bot&permissions=3072"
        );
        out(&next_steps(&invite, lang));
    } else {
        out(localized(
            lang,
            "Connection test failed.",
            "Bağlantı testi başarısız.",
            "Verbindungstest fehlgeschlagen.",
        ));
        out(&report.details);
        out(localized(
            lang,
            "The token is saved; run `bot channels discord setup` again to replace it.",
            "Jeton kaydedildi; değiştirmek için `bot channels discord setup` komutunu tekrar çalıştırın.",
            "Der Token ist gespeichert; zum Ersetzen führen Sie `bot channels discord setup` erneut aus.",
        ));
        return Err("Discord connection test failed; see the report".into());
    }
    Ok(())
}

/// The Discord channel inside the daemon.
pub struct DiscordRuntime {
    configured: bool,
    enabled: bool,
    data_dir: PathBuf,
    channel: parking_lot::Mutex<Option<DiscordChannel>>,
    config: Option<DiscordConfig>,
    client: Arc<HttpDaemonClient>,
    store: Arc<dyn SecretStore>,
    api_base_url: String,
}

#[async_trait::async_trait]
impl ChannelControl for DiscordRuntime {
    fn start(&self) {
        let mut channel = self.channel.lock();
        if channel.as_ref().is_some_and(DiscordChannel::is_running) {
            return;
        }
        if let Some(existing) = channel.as_ref() {
            existing.stop();
        }
        if let Some(config) = &self.config {
            match DiscordChannel::start(config.clone(), self.client.clone()) {
                Ok(c) => *channel = Some(c),
                Err(e) => {
                    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: discord channel not started: {e}");
                }
            }
        }
    }

    async fn stop(&self) {
        let channel = self.channel.lock().take();
        if let Some(c) = channel {
            c.shutdown().await;
        }
    }

    async fn notify_secret_request(&self, chat: protocol::ChatId) {
        let channel = self.channel.lock().clone();
        if let Some(channel) = channel {
            channel.notify_secret_request(chat).await;
        }
    }

    async fn notify_elicitation(&self, chat: protocol::ChatId) {
        let channel = self.channel.lock().clone();
        if let Some(channel) = channel {
            channel.notify_elicitation(chat).await;
        }
    }

    fn chats_using_bot(&self, bot: protocol::BotId) -> usize {
        let channel = self.channel.lock().clone();
        channel.map_or(0, |c| c.channels_using(bot))
    }

    async fn forget_bot(&self, bot: protocol::BotId) -> usize {
        let channel = self.channel.lock().clone();
        channel.map_or(0, |c| c.forget_bot(bot).unwrap_or(0))
    }

    fn info(&self) -> ChannelDto {
        let channel = self.channel.lock();
        let running = channel.as_ref().is_some_and(DiscordChannel::is_running);
        ChannelDto {
            status: Some(if running { "running" } else { "stopped" }.into()),
            id: CHANNEL.into(),
            configured: self.configured,
            enabled: self.enabled,
            running,
            paired_users: channel.as_ref().map_or_else(
                || discord_paired_on_disk(&self.data_dir),
                DiscordChannel::paired_count,
            ),
        }
    }

    async fn test_connection(&self) -> Option<R> {
        Some(check_token(self.store.as_ref(), &self.api_base_url).await.0)
    }

    fn create_pair_code(&self) -> std::result::Result<PairCodeDto, String> {
        match &*self.channel.lock() {
            Some(c) if c.is_running() => Ok(PairCodeDto {
                code: c.create_pair_code(),
                expires_in_secs: channel_telegram::PAIR_CODE_TTL.as_secs(),
            }),
            _ if !self.configured => {
                Err("discord is not configured; run `bot channels discord setup`".into())
            }
            _ if !self.enabled => {
                Err("discord is disabled; enable it with `bot channels discord setup`".into())
            }
            _ => Err("the discord channel is not running".into()),
        }
    }
}

/// Build the Discord runtime without starting it.
pub fn discord_runtime(
    data_dir: &Path,
    store: Arc<dyn SecretStore>,
    daemon_url: &str,
    daemon_token: &str,
    owner_key: Option<&str>,
    api_base_url: Option<&str>,
) -> Arc<DiscordRuntime> {
    let configured = discord_configured(store.as_ref()).unwrap_or(false);
    let enabled = discord_enabled(data_dir);
    let config = (configured && enabled).then(|| {
        let mut cfg = DiscordConfig::new(Arc::clone(&store));
        cfg.language = saved_language(data_dir);
        cfg.extra_redactor_secrets.push(daemon_token.to_owned());
        if let Some(key) = owner_key {
            cfg.extra_redactor_secrets.push(key.to_owned());
        }
        cfg.state_path = Some(data_dir.join(DISCORD_STATE_FILE));
        if let Some(url) = api_base_url {
            url.clone_into(&mut cfg.api_base_url);
        }
        cfg
    });
    Arc::new(DiscordRuntime {
        configured,
        enabled,
        data_dir: data_dir.to_path_buf(),
        channel: parking_lot::Mutex::new(None),
        config,
        store,
        api_base_url: api_base_url
            .unwrap_or(channel_discord::DEFAULT_API_URL)
            .to_owned(),
        // Like the desktop app and Telegram: the owner's authenticated chat decides
        // approvals, so the channel carries the owner key.
        client: Arc::new(
            HttpDaemonClient::new(daemon_url, SecretValue::new(daemon_token))
                .with_owner_key(owner_key.map(SecretValue::new))
                .with_message_source("discord"),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "discord_cli_hermetic_token_value";

    #[test]
    fn discord_setup_uses_the_secret_store_and_keeps_telegram_settings() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new();
        set_channel_enabled(dir.path(), "telegram", false).unwrap();
        let mut output = Vec::new();
        discord_setup(
            &store,
            dir.path(),
            &SecretValue::new(TOKEN),
            false,
            &mut |s| output.push(s.to_owned()),
        )
        .unwrap();
        assert!(discord_configured(&store).unwrap());
        assert!(discord_enabled(dir.path()));
        assert!(!channels_enabled(dir.path(), "telegram"));
        assert!(output.iter().all(|s| !s.contains(TOKEN)));
        assert!(
            !std::fs::read_to_string(dir.path().join("channels.toml"))
                .unwrap()
                .contains(TOKEN)
        );
        discord_disable(dir.path(), false, &mut |_| {}).unwrap();
        assert!(!discord_enabled(dir.path()));
        assert!(discord_configured(&store).unwrap());
        let runtime = discord_runtime(
            dir.path(),
            Arc::new(store),
            "http://127.0.0.1:1",
            "daemon",
            Some("owner"),
            None,
        );
        runtime.start();
        assert!(!runtime.info().running);
        assert!(runtime.create_pair_code().is_err());
    }

    #[tokio::test]
    async fn discord_connection_test_requires_token_and_content_permission() {
        let server = MockServer::start().await;
        let store = MemoryStore::new();
        store_bot_token(&store, TOKEN).unwrap();
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id":"1", "username":"test bot"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/applications/@me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"flags":1 << 19})))
            .mount(&server)
            .await;
        assert_eq!(check_token(&store, &server.uri()).await.0.outcome, O::Ok);
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/users/@me"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id":"1", "username":"test bot"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/applications/@me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"flags":0})))
            .mount(&server)
            .await;
        let (report, _) = check_token(&store, &server.uri()).await;
        assert_eq!(
            report.provider_code.as_deref(),
            Some("message_content_intent_off")
        );
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({"message":TOKEN})))
            .mount(&server)
            .await;
        let (report, _) = check_token(&store, &server.uri()).await;
        assert_eq!(report.outcome, O::BadKey);
        assert!(!format!("{report:?}").contains(TOKEN));
    }
}

//! The e-mail channel inside `bot serve`: reads `<data dir>/email-channel.toml`,
//! polls the chosen folder and hands each new mail to the chosen bot as an
//! untrusted chat message (`connector_email::channel`), and implements the
//! owner-only `/v1/email/channel*` API ([`daemon_api::EmailChannelControl`]).
//!
//! The channel talks to the daemon with its API token only, never the owner key:
//! mail text is never "from the owner", so runs it starts do not count as
//! owner-started. A reply a bot proposes is only stored; it is sent when the
//! owner approves it through the owner-only route.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use channel_api::{Channel, IncomingKind};
use channel_telegram::{DaemonClient, HttpDaemonClient};
use connector_email::channel::{ChannelMode, EmailChannel, EmailChannelSettings};
use connector_email::{AccountKind, ImapSmtpProvider};
use daemon_api::ChannelControl;
use daemon_api::dto::{ChannelDto, PairCodeDto};
use daemon_api::email::EmailControlError;
use daemon_api::email_channel::{
    EmailChannelControl, EmailChannelDto, EmailChannelMode, EmailChannelSettingsDto,
    EmailPendingReplyDto,
};
use futures_util::StreamExt;
use protocol::{BotId, ChatId};
use secret_store::SecretValue;

use crate::email_host::EmailHost;

/// Settings file in the data dir.
pub const SETTINGS_FILE: &str = "email-channel.toml";
const STATE_FILE: &str = "email-channel-state.json";
const CHAT_FILE: &str = "email-channel-chat.txt";

struct Running {
    channel: EmailChannel,
    task: tokio::task::JoinHandle<()>,
}

/// Runtime and control of the e-mail channel.
pub struct EmailChannelHost {
    data_dir: PathBuf,
    email: Arc<EmailHost>,
    client: Arc<HttpDaemonClient>,
    settings: parking_lot::Mutex<EmailChannelSettings>,
    run: parking_lot::Mutex<Option<Running>>,
    /// This installation is the active one (channels may run).
    active: std::sync::atomic::AtomicBool,
}

fn load(path: &Path) -> EmailChannelSettings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return EmailChannelSettings::default();
    };
    match toml::from_str::<EmailChannelSettings>(&text) {
        Ok(s) if s.validate().is_ok() => s,
        _ => {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: email-channel.toml is not valid; the e-mail channel stays off");
            EmailChannelSettings::default()
        }
    }
}

fn save(path: &Path, s: &EmailChannelSettings) -> std::io::Result<()> {
    let text = toml::to_string(s).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn dto_of(s: &EmailChannelSettings) -> EmailChannelSettingsDto {
    EmailChannelSettingsDto {
        enabled: s.enabled,
        account: s.account.clone(),
        folder: s.folder.clone(),
        bot: s.bot.clone(),
        poll_secs: s.poll_secs,
        mode: match s.mode {
            ChannelMode::Summary => EmailChannelMode::Summary,
            ChannelMode::Full => EmailChannelMode::Full,
        },
        max_per_poll: s.max_per_poll,
        replies_enabled: s.replies_enabled,
    }
}

impl EmailChannelHost {
    /// Build the host over the running e-mail accounts. `daemon_url` and
    /// `token` are the daemon's own loopback address and API token.
    pub fn new(data_dir: &Path, email: Arc<EmailHost>, daemon_url: &str, token: &str) -> Arc<Self> {
        Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            settings: parking_lot::Mutex::new(load(&data_dir.join(SETTINGS_FILE))),
            email,
            client: Arc::new(HttpDaemonClient::new(daemon_url, SecretValue::new(token))),
            run: parking_lot::Mutex::new(None),
            active: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn build(&self, s: &EmailChannelSettings) -> Result<EmailChannel, EmailControlError> {
        let acct = self
            .email
            .connector()
            .accounts()
            .into_iter()
            .find(|a| a.name == s.account)
            .ok_or_else(|| {
                EmailControlError::Invalid("that e-mail account does not exist".into())
            })?;
        if acct.kind != AccountKind::ImapSmtp {
            return Err(EmailControlError::Invalid(
                "this kind of account cannot be watched yet; use a normal mailbox".into(),
            ));
        }
        let provider = Arc::new(ImapSmtpProvider::new(acct.clone(), self.email.secrets()));
        EmailChannel::new(
            provider,
            &acct.address,
            &acct.allowed_senders,
            s.clone(),
            Some(self.data_dir.join(STATE_FILE)),
        )
        .map_err(|e| EmailControlError::Invalid(e.to_string()))
    }

    fn halt(&self) {
        if let Some(r) = self.run.lock().take() {
            r.task.abort();
        }
    }

    /// (Re)start polling according to the current settings and activity.
    fn apply(&self) {
        self.halt();
        let s = self.settings.lock().clone();
        if !s.enabled || !self.active.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let channel = match self.build(&s) {
            Ok(c) => c,
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: e-mail channel not started: {e:?}");
                return;
            }
        };
        let bot = s.bot.as_deref().and_then(|b| b.parse().ok()).map(BotId);
        let client = Arc::clone(&self.client);
        let chat_file = self.data_dir.join(CHAT_FILE);
        let poller = channel.clone();
        let task = tokio::spawn(async move {
            let Ok(mut stream) = poller.receive().await else {
                return;
            };
            let mut chat: Option<ChatId> = std::fs::read_to_string(&chat_file)
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .map(ChatId);
            while let Some(msg) = stream.next().await {
                let (Some(bot), IncomingKind::Text(text)) = (bot, msg.kind) else {
                    continue;
                };
                if chat.is_none() {
                    match client.create_chat(bot, Some("E-mail")).await {
                        Ok(c) => {
                            let _ = std::fs::write(&chat_file, c.id.0.to_string());
                            chat = Some(c.id);
                        }
                        Err(_) => continue,
                    }
                }
                if let Some(id) = chat
                    && client
                        .send_message_idempotent(id, &text, &msg.event_id)
                        .await
                        .is_err()
                {
                    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: could not hand a mail to the bot");
                }
            }
        });
        *self.run.lock() = Some(Running { channel, task });
    }

    fn channel(&self) -> Option<EmailChannel> {
        self.run.lock().as_ref().map(|r| r.channel.clone())
    }
}

#[async_trait::async_trait]
impl ChannelControl for EmailChannelHost {
    fn start(&self) {
        self.active.store(true, std::sync::atomic::Ordering::SeqCst);
        self.apply();
    }
    async fn stop(&self) {
        self.active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.halt();
    }
    fn info(&self) -> ChannelDto {
        let s = self.settings.lock();
        let running = self.run.lock().is_some();
        ChannelDto {
            id: "email".into(),
            configured: !s.account.is_empty(),
            enabled: s.enabled,
            running,
            paired_users: 0,
            status: Some(if running { "running" } else { "stopped" }.into()),
        }
    }
    fn create_pair_code(&self) -> Result<PairCodeDto, String> {
        Err("the e-mail channel needs no pairing code".into())
    }
}

#[async_trait::async_trait]
impl EmailChannelControl for EmailChannelHost {
    fn get(&self) -> EmailChannelDto {
        let settings = dto_of(&self.settings.lock());
        EmailChannelDto {
            settings,
            running: self.run.lock().is_some(),
            pending_replies: self.channel().map_or(0, |c| c.pending_replies().len()),
        }
    }

    async fn put(
        &self,
        input: EmailChannelSettingsDto,
    ) -> Result<EmailChannelDto, EmailControlError> {
        let next = EmailChannelSettings {
            enabled: input.enabled,
            account: input.account.trim().to_owned(),
            folder: input.folder,
            bot: input.bot,
            poll_secs: input.poll_secs,
            mode: match input.mode {
                EmailChannelMode::Summary => ChannelMode::Summary,
                EmailChannelMode::Full => ChannelMode::Full,
            },
            max_per_poll: input.max_per_poll,
            replies_enabled: input.replies_enabled,
            ..self.settings.lock().clone()
        };
        next.validate()
            .map_err(|e| EmailControlError::Invalid(e.to_string()))?;
        if next.enabled {
            if next.bot.is_none() {
                return Err(EmailControlError::Invalid(
                    "choose the bot that should receive the mail".into(),
                ));
            }
            self.build(&next)?;
        }
        save(&self.data_dir.join(SETTINGS_FILE), &next)
            .map_err(|_| EmailControlError::Internal("cannot save the settings".into()))?;
        *self.settings.lock() = next;
        self.apply();
        Ok(self.get())
    }

    fn replies(&self) -> Vec<EmailPendingReplyDto> {
        self.channel()
            .map(|c| c.pending_replies())
            .unwrap_or_default()
            .into_iter()
            .map(|p| EmailPendingReplyDto {
                id: p.id,
                to: p.to,
                subject: p.subject,
                body: p.body,
                created_ms: p.created_ms,
            })
            .collect()
    }

    async fn approve(&self, id: &str) -> Result<(), EmailControlError> {
        let channel = self
            .channel()
            .ok_or_else(|| EmailControlError::NotFound("that reply is no longer waiting".into()))?;
        channel
            .approve_reply(id)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                connector_email::EmailError::NotFound(m) => EmailControlError::NotFound(m),
                connector_email::EmailError::Input(m) => EmailControlError::Invalid(m),
                _ => {
                    EmailControlError::Upstream("the mail server did not accept the e-mail".into())
                }
            })
    }

    fn deny(&self, id: &str) -> Result<(), EmailControlError> {
        match self.channel() {
            Some(c) if c.deny_reply(id) => Ok(()),
            _ => Err(EmailControlError::NotFound(
                "that reply is no longer waiting".into(),
            )),
        }
    }
}

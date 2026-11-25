//! The e-mail channel: new mail in one folder becomes *untrusted* inbound
//! messages for a bot ([`channel_api::Channel`]).
//!
//! - Read only by default: a short snippet of every new mail, never the HTML,
//!   never an attachment (names and sizes only). Mail is fetched with
//!   `BODY.PEEK`, nothing is marked read.
//! - Only mail that arrives after the channel is switched on is delivered.
//! - Sending is a separate opt-in ([`EmailChannelSettings::replies_enabled`]).
//!   [`Channel::send`] never sends: it files a [`PendingReply`] that the owner
//!   approves ([`EmailChannel::approve_reply`]) after seeing every recipient
//!   and the whole text. Replies can only go to people who wrote to the mailbox.
//! - Rate limits: at most `max_per_poll` mails per check (the rest become one
//!   notice), at most 10 approved sends per hour, at most 20 waiting replies.
//!
//! Transport (TLS, certificate checks, host rules) is the connector's job; the
//! channel only talks to a [`MailboxProvider`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use channel_api::{
    Channel, ChannelCaps, ChannelError, ChatRef, IncomingKind, IncomingMessage, OutgoingMessage,
    SenderIdentity,
};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::config::normalize_sender;
use crate::error::EmailError;
use crate::imap::SearchQuery;
use crate::message::MAX_MESSAGE_BYTES;
use crate::provider::{MailboxProvider, Receipt, SearchRequest};

mod render;
mod replies;
mod settings;

pub use render::{UNTRUSTED_BANNER, address_of, clean_body, clean_line, render_mail};
pub use replies::{MAX_PENDING, MAX_SENDS_PER_HOUR, PendingReply, ReplyGate};
pub use settings::{ChannelMode, EmailChannelSettings};

/// Most unread mails looked at per poll.
const SEARCH_LIMIT: usize = 100;
/// Most sender addresses remembered for the reply rule.
const KNOWN_SENDERS: usize = 200;

/// The UID an event carries (`message_id` is the decimal UID).
#[must_use]
pub fn uid_of(m: &IncomingMessage) -> Option<u32> {
    m.message_id.as_deref()?.parse().ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Highest UID already handled; `None` before the first poll.
    last_uid: Option<u32>,
    /// UIDVALIDITY the folder had when `last_uid` was recorded.
    #[serde(default)]
    uid_validity: Option<u32>,
    folder: String,
    known: VecDeque<String>,
}

struct Core {
    provider: Arc<dyn MailboxProvider>,
    own_address: String,
    settings: EmailChannelSettings,
    allowed: Vec<String>,
    state: Mutex<State>,
    state_path: Option<PathBuf>,
    gate: Mutex<ReplyGate>,
}

impl Core {
    /// Build the channel. `allowed_senders` come from the account (empty =
    /// anyone; the mail is untrusted either way). `state_path` keeps the
    /// "already seen" position across restarts.
    ///
    /// # Errors
    /// Invalid settings or sender entries.
    pub fn new(
        provider: Arc<dyn MailboxProvider>,
        own_address: &str,
        allowed_senders: &[String],
        settings: EmailChannelSettings,
        state_path: Option<PathBuf>,
    ) -> Result<Self, EmailError> {
        settings.validate()?;
        let allowed = allowed_senders
            .iter()
            .map(|s| normalize_sender(s))
            .collect::<Result<Vec<_>, _>>()?;
        let mut state = state_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<State>(&s).ok())
            .unwrap_or_default();
        if state.folder != settings.folder {
            state = State {
                folder: settings.folder.clone(),
                ..State::default()
            };
        }
        Ok(Self {
            provider,
            own_address: own_address.trim().to_lowercase(),
            settings,
            allowed,
            state: Mutex::new(state),
            state_path,
            gate: Mutex::new(ReplyGate::default()),
        })
    }

    fn sender_allowed(&self, addr: &str) -> bool {
        self.allowed.is_empty()
            || self.allowed.iter().any(|a| match a.strip_prefix('@') {
                Some(domain) => addr.rsplit_once('@').is_some_and(|(_, d)| d == domain),
                None => a == addr,
            })
    }

    fn save(&self, state: &State) {
        if let Some(p) = &self.state_path
            && let Ok(json) = serde_json::to_string(state)
        {
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, p);
            }
        }
    }

    fn event(&self, uid: u32, sender: SenderIdentity, text: String) -> IncomingMessage {
        let folder = &self.settings.folder;
        IncomingMessage {
            event_id: format!("email:{folder}:{uid}"),
            chat: ChatRef(format!("email:{folder}")),
            sender,
            message_id: Some(uid.to_string()),
            kind: IncomingKind::Text(text),
        }
    }

    /// Look once for new mail, hand every mail back and treat it as delivered
    /// (the position moves to the newest). For a caller that must not lose a
    /// mail when handing it over fails, use [`Self::poll_pending`] and
    /// [`Self::commit`].
    ///
    /// # Errors
    /// The mailbox could not be searched.
    pub async fn poll_once(&self) -> Result<Vec<IncomingMessage>, EmailError> {
        let out = self.poll_pending().await?;
        if let Some(uid) = out.iter().filter_map(uid_of).max() {
            self.commit(uid);
        }
        Ok(out)
    }

    /// Record that every mail up to `uid` was handed to the bot. Until then a
    /// mail is offered again by the next [`Self::poll_pending`].
    pub fn commit(&self, uid: u32) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.last_uid.is_none_or(|l| uid > l) {
            st.last_uid = Some(uid);
        }
        self.save(&st);
    }

    /// Look once for new mail and return what to hand to the bot, oldest
    /// first, WITHOUT moving the position (see [`Self::commit`]). The first
    /// call, and the first one after the folder's UIDVALIDITY changed, only
    /// remember where the mailbox stands: old UIDs mean other mail now, so
    /// nothing is replayed and nothing old is delivered again.
    ///
    /// # Errors
    /// The mailbox could not be searched (nothing is lost: the position only
    /// moves on a successful search or a commit).
    pub async fn poll_pending(&self) -> Result<Vec<IncomingMessage>, EmailError> {
        let query = SearchQuery {
            unread: Some(true),
            ..SearchQuery::default()
        };
        let found = self
            .provider
            .search(&SearchRequest {
                folder: &self.settings.folder,
                query: &query,
                limit: SEARCH_LIMIT,
            })
            .await?;
        let last = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let changed =
                matches!((st.uid_validity, found.uid_validity), (Some(a), Some(b)) if a != b);
            if found.uid_validity.is_some() {
                st.uid_validity = found.uid_validity;
            }
            if st.last_uid.is_none() || changed {
                st.last_uid = Some(found.messages.iter().map(|m| m.uid).max().unwrap_or(0));
                self.save(&st);
                return Ok(Vec::new());
            }
            st.last_uid.unwrap_or(0)
        };
        let mut fresh: Vec<_> = found
            .messages
            .into_iter()
            .filter(|m| m.uid > last)
            .collect();
        fresh.sort_by_key(|m| m.uid);
        let Some(newest) = fresh.last().map(|m| m.uid) else {
            return Ok(Vec::new());
        };
        fresh.retain(|s| {
            let addr = address_of(&s.from);
            addr != self.own_address && self.sender_allowed(&addr)
        });
        let skipped = fresh.len().saturating_sub(self.settings.max_per_poll);
        fresh.truncate(self.settings.max_per_poll);
        let mut out = Vec::new();
        for s in fresh {
            let addr = address_of(&s.from);
            let text = if s.size.is_some_and(|n| n > MAX_MESSAGE_BYTES) {
                render::render_unopened(&s, "This mail is too large; it was not opened.")
            } else {
                match self
                    .provider
                    .get_message(
                        &self.settings.folder,
                        &s.uid.to_string(),
                        self.settings.fetch_chars(),
                    )
                    .await
                {
                    Ok(d) => render_mail(&d, self.settings.fetch_chars()),
                    Err(_) => render::render_unopened(&s, "This mail could not be read."),
                }
            };
            {
                let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.known.retain(|k| *k != addr);
                st.known.push_back(addr.clone());
                while st.known.len() > KNOWN_SENDERS {
                    st.known.pop_front();
                }
            }
            let sender = SenderIdentity {
                id: addr,
                display_name: render::sender_name(&s.from),
            };
            out.push(self.event(s.uid, sender, text));
        }
        if skipped > 0 {
            out.push(self.event(
                newest,
                SenderIdentity {
                    id: "email-channel".into(),
                    display_name: None,
                },
                format!(
                    "{UNTRUSTED_BANNER}\n---\n[{skipped} more new mails arrived at the same time and were not shown. Open the mailbox to see them.]"
                ),
            ));
        }
        if out.is_empty() {
            // Only mail that is not ours to deliver (own address, not an
            // allowed sender): nothing to hand over, so move on.
            self.commit(newest);
        }
        Ok(out)
    }

    fn propose_reply(&self, to: &ChatRef, msg: &OutgoingMessage) -> channel_api::Result<String> {
        if !self.settings.replies_enabled {
            return Err(ChannelError::InvalidInput(
                "replies are switched off for the e-mail channel".into(),
            ));
        }
        let (subject, body) = match msg.text.split_once('\n') {
            Some((first, rest)) if first.starts_with("Subject:") => (
                first["Subject:".len()..].trim().to_owned(),
                rest.trim_start().to_owned(),
            ),
            _ => ("Re:".to_owned(), msg.text.clone()),
        };
        let id = uuid::Uuid::now_v7().to_string();
        let recipient = to.0.trim().to_lowercase();
        let known = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .known
            .clone();
        let reply = self
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .propose(now_ms(), id, &[recipient], &subject, &body, |a| {
                known.iter().any(|k| k == a)
            })
            .map_err(|e| ChannelError::InvalidInput(e.to_string()))?;
        Ok(reply.id)
    }

    /// Replies waiting for the owner.
    #[must_use]
    pub fn pending_replies(&self) -> Vec<PendingReply> {
        self.gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .list(now_ms())
    }

    /// Send a waiting reply, exactly as the owner saw it. Only the owner-only
    /// approval route may call this.
    ///
    /// # Errors
    /// Unknown id, rate limit, or the mail server refusing the mail (the
    /// reply is not retried).
    pub async fn approve_reply(&self, id: &str) -> Result<Receipt, EmailError> {
        if !self.settings.replies_enabled {
            return Err(EmailError::Input("replies are switched off".into()));
        }
        let reply = self
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_for_send(now_ms(), id)?;
        self.provider.send(&reply.mail()).await
    }

    /// Drop a waiting reply. `true` when it existed.
    #[must_use]
    pub fn deny_reply(&self, id: &str) -> bool {
        self.gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .deny(now_ms(), id)
    }
}

/// The e-mail channel for one account and folder. Cheap to clone.
#[derive(Clone)]
pub struct EmailChannel {
    core: Arc<Core>,
}

impl EmailChannel {
    /// Build the channel. `allowed_senders` come from the account (empty =
    /// anyone; the mail is untrusted either way). `state_path` keeps the
    /// "already seen" position across restarts.
    ///
    /// # Errors
    /// Invalid settings or sender entries.
    pub fn new(
        provider: Arc<dyn MailboxProvider>,
        own_address: &str,
        allowed_senders: &[String],
        settings: EmailChannelSettings,
        state_path: Option<PathBuf>,
    ) -> Result<Self, EmailError> {
        Ok(Self {
            core: Arc::new(Core::new(
                provider,
                own_address,
                allowed_senders,
                settings,
                state_path,
            )?),
        })
    }

    /// Look once for new mail (see the module docs); oldest first.
    ///
    /// # Errors
    /// The mailbox could not be searched.
    pub async fn poll_once(&self) -> Result<Vec<IncomingMessage>, EmailError> {
        self.core.poll_once().await
    }

    /// Like [`Self::poll_once`] but the position only moves when the caller
    /// confirms each mail with [`Self::commit`] after handing it over.
    ///
    /// # Errors
    /// The mailbox could not be searched.
    pub async fn poll_pending(&self) -> Result<Vec<IncomingMessage>, EmailError> {
        self.core.poll_pending().await
    }

    /// Mark every mail up to `uid` as handed over.
    pub fn commit(&self, uid: u32) {
        self.core.commit(uid);
    }

    /// Replies waiting for the owner.
    #[must_use]
    pub fn pending_replies(&self) -> Vec<PendingReply> {
        self.core.pending_replies()
    }

    /// Send a waiting reply exactly as the owner saw it. Only the owner-only
    /// approval route may call this.
    ///
    /// # Errors
    /// Unknown id, rate limit, or the mail server refusing the mail.
    pub async fn approve_reply(&self, id: &str) -> Result<Receipt, EmailError> {
        self.core.approve_reply(id).await
    }

    /// Drop a waiting reply. `true` when it existed.
    #[must_use]
    pub fn deny_reply(&self, id: &str) -> bool {
        self.core.deny_reply(id)
    }
}

#[async_trait]
impl Channel for EmailChannel {
    /// Endless stream of new mail: polls every `poll_secs`, ends when dropped.
    /// A failed poll is skipped and tried again on the next round.
    async fn receive(&self) -> channel_api::Result<BoxStream<'static, IncomingMessage>> {
        let this = Arc::clone(&self.core);
        let every = Duration::from_secs(this.settings.poll_secs);
        Ok(
            futures_util::stream::unfold((this, true), move |(this, first)| async move {
                if !first {
                    tokio::time::sleep(every).await;
                }
                let batch = this.poll_once().await.unwrap_or_default();
                Some((futures_util::stream::iter(batch), (this, false)))
            })
            .flatten()
            .boxed(),
        )
    }

    /// Never sends: files a reply for the owner's approval and returns its id.
    async fn send(&self, to: ChatRef, msg: OutgoingMessage) -> channel_api::Result<String> {
        self.core.propose_reply(&to, &msg)
    }

    async fn answer_callback(
        &self,
        _query_id: &str,
        _text: Option<&str>,
    ) -> channel_api::Result<()> {
        Err(ChannelError::Unsupported("e-mail has no buttons"))
    }

    fn capabilities(&self) -> ChannelCaps {
        ChannelCaps {
            inline_buttons: false,
            message_editing: false,
            file_upload: false,
            voice_messages: false,
        }
    }
}

#[cfg(test)]
mod tests;

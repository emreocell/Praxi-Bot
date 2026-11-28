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
//! - Only mail from an allowed sender is delivered. The allowed-senders list of
//!   the account is the gate: while it is empty NOTHING is delivered (mail from
//!   anybody else is ignored and counted, never opened or logged with a body).
//! - Rate limits: at most `max_per_poll` mails per check (the rest stay unread
//!   for the next check and are announced in one notice), at most 10 approved
//!   sends per hour, at most 20 waiting replies.
//! - A mail whose hand-over keeps failing is retried [`MAX_DELIVERY_ATTEMPTS`]
//!   times and then skipped (counted), so it cannot block the mails behind it.
//!
//! Transport (TLS, certificate checks, host rules) is the connector's job; the
//! channel only talks to a [`MailboxProvider`].

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use channel_api::{
    Channel, ChannelCaps, ChannelError, ChatRef, IncomingKind, IncomingMessage, OutgoingMessage,
    SenderIdentity,
};
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::config::{AccountConfig, SendMode, normalize_sender};
use crate::error::EmailError;
use crate::guard::{self, MailFacts, MailGuard, SendLogEntry};
use crate::imap::SearchQuery;
use crate::message::{MAX_MESSAGE_BYTES, MessageSummary};
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
/// How often handing one mail to the bot may fail before the mail is skipped.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 3;

/// What to do with a mail whose hand-over just failed
/// ([`EmailChannel::delivery_failed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Keep the order: this mail and the rest are offered again at the next check.
    RetryLater,
    /// Failed [`MAX_DELIVERY_ATTEMPTS`] times: given up and committed, go on with the next mail.
    Skipped,
}

/// What the model is told when it proposes a reply from a draft-only account.
const DRAFT_ONLY_REPLY: &str = "this mail account is set to draft only by the owner; replies are not sent from it. Tell the user you cannot reply from this address";

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
    /// Failed hand-overs per UID since the channel started.
    #[serde(skip)]
    failures: HashMap<u32, u32>,
    /// Mails given up after [`MAX_DELIVERY_ATTEMPTS`] failed hand-overs.
    #[serde(skip)]
    skipped: usize,
    /// Mails from senders that are not allowed (ignored, never delivered).
    #[serde(skip)]
    ignored: usize,
    /// Highest UID already counted as ignored (so a mail is counted once).
    #[serde(skip)]
    ignored_through: u32,
}

struct Core {
    provider: Arc<dyn MailboxProvider>,
    own_address: String,
    settings: EmailChannelSettings,
    /// Allowed senders; the host refreshes them every poll
    /// ([`EmailChannel::set_allowed_senders`]).
    allowed: Mutex<Vec<String>>,
    state: Mutex<State>,
    state_path: Option<PathBuf>,
    gate: Mutex<ReplyGate>,
    /// The account is limited to drafts by the owner: nothing is queued or
    /// sent from this channel.
    draft_only: bool,
    /// The outgoing-mail guard and the account name its counts use (set by the host).
    guard: OnceLock<(Arc<MailGuard>, String)>,
}

impl Core {
    /// Build the channel. `allowed_senders` come from the account: only mail
    /// from these addresses or `@domains` is delivered, and an EMPTY list
    /// delivers nothing (mail is untrusted, so there is no "anyone"). `state_path`
    /// keeps the "already seen" position across restarts.
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
            allowed: Mutex::new(allowed),
            state: Mutex::new(state),
            state_path,
            gate: Mutex::new(ReplyGate::default()),
            draft_only: false,
            guard: OnceLock::new(),
        })
    }

    fn sender_allowed(&self, addr: &str) -> bool {
        // An empty list allows nobody: the owner must name who may start a run.
        // An empty address (not exactly one sender) matches nothing.
        if addr.is_empty() {
            return false;
        }
        let allowed = self.allowed.lock().unwrap_or_else(PoisonError::into_inner);
        allowed
            .iter()
            .any(|a| protocol::sender::sender_matches(addr, a))
    }

    /// Replace the allowed senders (the account's list may change while the
    /// channel runs). A bad entry empties the list: closed, never open.
    fn set_allowed(&self, senders: &[String]) {
        let list = senders
            .iter()
            .map(|s| normalize_sender(s))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_default();
        *self.allowed.lock().unwrap_or_else(PoisonError::into_inner) = list;
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
            forwarded: false,
            edited: false,
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
        st.failures.retain(|u, _| *u > uid);
        self.save(&st);
    }

    /// Record that handing the mail `uid` to the bot failed. The first
    /// `MAX_DELIVERY_ATTEMPTS - 1` failures keep the mail pending; the last
    /// one skips it (it is committed and counted) so mails behind it flow.
    #[must_use]
    pub fn delivery_failed(&self, uid: u32) -> DeliveryOutcome {
        let attempts = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let n = st.failures.entry(uid).or_insert(0);
            *n += 1;
            *n
        };
        if attempts < MAX_DELIVERY_ATTEMPTS {
            return DeliveryOutcome::RetryLater;
        }
        self.commit(uid);
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .skipped += 1;
        log::warn!(
            "e-mail channel: skipped mail uid {uid} after {MAX_DELIVERY_ATTEMPTS} failed hand-overs"
        );
        DeliveryOutcome::Skipped
    }

    /// Mails given up since the channel started (hand-over kept failing).
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .skipped
    }

    /// Mails from senders that are not allowed, since the channel started.
    #[must_use]
    pub fn ignored_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ignored
    }

    /// Keep only mail the channel may deliver: not our own address, and from an
    /// allowed sender (the rest is counted and logged, see [`Self::note_ignored`]).
    fn drop_undeliverable(&self, fresh: &mut Vec<MessageSummary>) {
        let mut ignored = Vec::new();
        fresh.retain(|s| {
            let addr = s.from_addr.clone();
            if addr == self.own_address {
                return false;
            }
            if self.sender_allowed(&addr) {
                return true;
            }
            ignored.push((s.uid, addr));
            false
        });
        self.note_ignored(&ignored);
    }

    /// Count (once) and log, without any text of the mail, mails from senders
    /// that are not allowed.
    fn note_ignored(&self, ignored: &[(u32, String)]) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let before = st.ignored_through;
        for (uid, addr) in ignored {
            if *uid > before {
                st.ignored += 1;
                log::info!(
                    "e-mail channel: ignored mail uid {uid} from {addr}: not an allowed sender"
                );
            }
        }
        if let Some(top) = ignored.iter().map(|(u, _)| *u).max() {
            st.ignored_through = before.max(top);
        }
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
                st.ignored_through = 0;
                st.failures.clear();
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
        self.drop_undeliverable(&mut fresh);
        let skipped = fresh.len().saturating_sub(self.settings.max_per_poll);
        fresh.truncate(self.settings.max_per_poll);
        // The notice carries the UID of the last mail shown, so committing it
        // moves the position exactly that far: the rest stays for the next check.
        let last_shown = fresh.last().map_or(newest, |m| m.uid);
        let mut out = Vec::new();
        for s in fresh {
            let addr = s.from_addr.clone();
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
            let mut notice = self.event(
                last_shown,
                SenderIdentity {
                    id: "email-channel".into(),
                    display_name: None,
                },
                format!(
                    "{UNTRUSTED_BANNER}\n---\n[{skipped} more new mails arrived at the same time. They are still unread in the mailbox and will be passed on at the next check.]"
                ),
            );
            notice.event_id.push_str(":more");
            out.push(notice);
        }
        if out.is_empty() {
            // Only mail that is not ours to deliver (own address, not an
            // allowed sender, or no allowed sender named yet): nothing to hand
            // over, so move on.
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
        if self.draft_only {
            return Err(ChannelError::InvalidInput(DRAFT_ONLY_REPLY.into()));
        }
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
        if self.draft_only {
            return Err(EmailError::Input("draft only".into()));
        }
        // The reply goes through the same guard and log as `email_send`. The owner approves a
        // reply from a list that shows no guard warnings, so here a hit is refused outright
        // (the reply stays waiting): an opt-out entry, an unreadable guard file or the daily limit.
        let guarded = self.guard.get().map(|(g, account)| {
            let recipients = self
                .pending_replies()
                .into_iter()
                .find(|p| p.id == id)
                .map(|p| reply_recipients(&p.to))
                .unwrap_or_default();
            (g, account, recipients)
        });
        if let Some((g, account, recipients)) = &guarded {
            let report = g.check(&MailFacts {
                bot: None,
                account,
                from: &self.own_address,
                recipients,
                opt_out_line: true,
            });
            let blocking: Vec<&str> = report
                .warnings
                .iter()
                .filter(|w| {
                    matches!(
                        w.code.as_str(),
                        "opt_out_listed" | "guard_unreadable" | "daily_limit"
                    )
                })
                .map(|w| w.text.as_str())
                .collect();
            if !blocking.is_empty() {
                return Err(EmailError::Input(format!(
                    "Not sent. {} Change the opt-out list or the limit in Settings, or write to                      this person yourself.",
                    blocking.join(" ")
                )));
            }
        }
        let reply = self
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_for_send(now_ms(), id)?;
        let receipt = self.provider.send(&reply.mail()).await?;
        if let Some((g, account, recipients)) = guarded {
            g.record(SendLogEntry {
                at: guard::now_secs(),
                at_utc: String::new(),
                bot: None,
                run: "email-channel-reply".into(),
                account: account.clone(),
                from: self.own_address.clone(),
                recipients,
                subject: reply.subject.clone(),
                message_id: receipt.id.clone(),
                thread_id: receipt.thread_id.clone(),
                mission_id: None,
                opt_out_line: false,
                overridden: Vec::new(),
            });
        }
        Ok(receipt)
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
    /// Build the channel. `allowed_senders` come from the account; an empty
    /// list delivers nothing (only named senders may start a run).
    /// `state_path` keeps the "already seen" position across restarts.
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

    /// Like [`Self::new`] for a configured account: replies name the
    /// account's identity and mode, and an account the owner limited to
    /// drafts never queues or sends a reply.
    ///
    /// # Errors
    /// Invalid settings or sender entries.
    pub fn for_account(
        provider: Arc<dyn MailboxProvider>,
        account: &AccountConfig,
        settings: EmailChannelSettings,
        state_path: Option<PathBuf>,
    ) -> Result<Self, EmailError> {
        let mut core = Core::new(
            provider,
            &account.address,
            &account.allowed_senders,
            settings,
            state_path,
        )?;
        core.draft_only = account.identity.send_mode == SendMode::DraftOnly;
        core.gate = Mutex::new(ReplyGate::for_sender(format!(
            "{} (account: {})",
            account.address.trim(),
            account.name
        )));
        Ok(Self {
            core: Arc::new(core),
        })
    }

    /// Route approved replies through the outgoing-mail guard and send log (`account` is the
    /// name the guard's daily count uses). Set once; later calls are ignored.
    pub fn attach_guard(&self, guard: Arc<MailGuard>, account: &str) {
        let _ = self.core.guard.set((guard, account.to_owned()));
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

    /// Replace the allowed senders now (the host calls this every poll with the
    /// account's current list, so an edit or an emptied list applies at once).
    /// An invalid entry empties the list.
    pub fn set_allowed_senders(&self, senders: &[String]) {
        self.core.set_allowed(senders);
    }

    /// Report that handing the mail `uid` over failed: it is retried until
    /// [`MAX_DELIVERY_ATTEMPTS`], then skipped so later mails are not blocked.
    #[must_use]
    pub fn delivery_failed(&self, uid: u32) -> DeliveryOutcome {
        self.core.delivery_failed(uid)
    }

    /// Mails skipped because their hand-over kept failing.
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.core.skipped_count()
    }

    /// Mails ignored because the sender is not allowed (or none is named yet).
    #[must_use]
    pub fn ignored_count(&self) -> usize {
        self.core.ignored_count()
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

/// Recipients of a reply in the guard's canonical form (the same parse the envelope uses).
fn reply_recipients(to: &[String]) -> Vec<String> {
    to.iter()
        .map(|a| {
            crate::smtp::envelope_address(a)
                .ok()
                .and_then(|e| guard::canonical_address(&e))
                .map_or_else(|| a.to_lowercase(), |k| k.full)
        })
        .collect()
}

//! Replies a bot proposes. Nothing is sent from here: a proposal waits as a
//! [`PendingReply`] until the owner approves exactly that text and those
//! recipients. The stored reply can not be edited, so what the owner saw is
//! what is sent.

use std::collections::VecDeque;

use serde::Serialize;

use super::render::clean_line;
use crate::error::EmailError;
use crate::smtp::OutgoingMail;

/// Most replies waiting for the owner at once.
pub const MAX_PENDING: usize = 20;
/// Most recipients of one reply.
pub const MAX_REPLY_RECIPIENTS: usize = 5;
/// Longest reply body, in characters.
pub const MAX_REPLY_CHARS: usize = 8000;
/// Most approved sends per hour.
pub const MAX_SENDS_PER_HOUR: usize = 10;
/// A proposal nobody approved is dropped after this long.
pub const REPLY_TTL_MS: u64 = 24 * 3600 * 1000;
const HOUR_MS: u64 = 3600 * 1000;

/// A reply waiting for the owner's OK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingReply {
    /// Opaque id used to approve or deny it.
    pub id: String,
    /// Exact recipients (To only; there is no Cc or Bcc).
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body, exactly as it will be sent.
    pub body: String,
    /// When it was proposed (Unix ms).
    pub created_ms: u64,
    /// The identity it is sent from: `address (account: name)`. Empty when
    /// the channel does not know it.
    pub from: String,
    /// Send mode of that identity as the owner set it (`ask` here: a draft
    /// only identity never gets a reply queued).
    pub send_mode: String,
}

impl PendingReply {
    /// The approval card as plain text: every recipient and the whole body.
    #[must_use]
    pub fn card(&self) -> String {
        let from = if self.from.is_empty() {
            String::new()
        } else {
            format!(
                "From: {} (send mode: {})
",
                self.from, self.send_mode
            )
        };
        format!(
            "Send this e-mail?\n{from}To: {}\nSubject: {}\n---\n{}\n---\nIt is sent exactly as shown, only after you approve.",
            self.to.join(", "),
            self.subject,
            self.body
        )
    }

    /// The mail that goes out on approval.
    #[must_use]
    pub fn mail(&self) -> OutgoingMail {
        OutgoingMail {
            to: self.to.clone(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            ..OutgoingMail::default()
        }
    }
}

/// Waiting replies and the send rate limit.
#[derive(Debug, Default)]
pub struct ReplyGate {
    from: String,
    pending: Vec<PendingReply>,
    sent: VecDeque<u64>,
}

fn input(m: &str) -> EmailError {
    EmailError::Input(m.to_owned())
}

impl ReplyGate {
    /// A gate whose replies name the identity they go out from.
    #[must_use]
    pub fn for_sender(from: String) -> Self {
        Self {
            from,
            ..Self::default()
        }
    }

    fn expire(&mut self, now_ms: u64) {
        self.pending
            .retain(|p| now_ms.saturating_sub(p.created_ms) < REPLY_TTL_MS);
    }

    /// Queue a proposal. `known` says whether an address has written to this
    /// mailbox: a bot can answer people who mailed it, not invent new targets.
    ///
    /// # Errors
    /// [`EmailError::Input`] in plain words.
    pub fn propose(
        &mut self,
        now_ms: u64,
        id: String,
        recipients: &[String],
        subject: &str,
        body: &str,
        known: impl Fn(&str) -> bool,
    ) -> Result<PendingReply, EmailError> {
        self.expire(now_ms);
        if recipients.is_empty() || recipients.len() > MAX_REPLY_RECIPIENTS {
            return Err(input("a reply needs one to five recipients"));
        }
        let mut to = Vec::new();
        for r in recipients {
            if crate::address::has_hidden_char(r) || r.contains(['<', '>', ',', ';', ' ']) {
                return Err(input("recipients must be plain e-mail addresses"));
            }
            let addr = r.trim().to_lowercase();
            if !crate::address::is_valid_address(&addr) {
                return Err(input("recipients must be plain e-mail addresses"));
            }
            if !known(&addr) {
                return Err(input(
                    "replies can only go to people who wrote to this mailbox",
                ));
            }
            if !to.contains(&addr) {
                to.push(addr);
            }
        }
        if body.trim().is_empty() || body.chars().count() > MAX_REPLY_CHARS {
            return Err(input(
                "the reply must have text and at most 8000 characters",
            ));
        }
        if self.pending.len() >= MAX_PENDING {
            return Err(input(
                "too many replies are waiting for approval; decide on some first",
            ));
        }
        let reply = PendingReply {
            id,
            to,
            subject: clean_line(subject, 200),
            body: body.replace('\0', ""),
            created_ms: now_ms,
            from: self.from.clone(),
            send_mode: "ask".to_owned(),
        };
        self.pending.push(reply.clone());
        Ok(reply)
    }

    /// Replies still waiting, oldest first.
    pub fn list(&mut self, now_ms: u64) -> Vec<PendingReply> {
        self.expire(now_ms);
        self.pending.clone()
    }

    /// Remove a proposal for sending. It is consumed even if the send later
    /// fails (there are no automatic retries).
    ///
    /// # Errors
    /// Unknown or expired id; the hourly send limit is used up.
    pub fn take_for_send(&mut self, now_ms: u64, id: &str) -> Result<PendingReply, EmailError> {
        self.expire(now_ms);
        while self
            .sent
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= HOUR_MS)
        {
            self.sent.pop_front();
        }
        if self.sent.len() >= MAX_SENDS_PER_HOUR {
            return Err(input(
                "too many e-mails were sent this hour; try again later",
            ));
        }
        let at = self
            .pending
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| EmailError::NotFound("that reply is no longer waiting".into()))?;
        self.sent.push_back(now_ms);
        Ok(self.pending.remove(at))
    }

    /// Drop a proposal without sending. `true` when it existed.
    pub fn deny(&mut self, now_ms: u64, id: &str) -> bool {
        self.expire(now_ms);
        let before = self.pending.len();
        self.pending.retain(|p| p.id != id);
        self.pending.len() != before
    }
}

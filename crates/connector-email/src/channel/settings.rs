//! Settings of the e-mail channel. The safe defaults are: switched off, read
//! only, a short snippet of every mail, replies off.

use serde::{Deserialize, Serialize};

use crate::error::EmailError;
use crate::message::MAX_BODY_CHARS;

/// Shortest allowed gap between two polls.
pub const MIN_POLL_SECS: u64 = 60;
/// Longest allowed gap between two polls.
pub const MAX_POLL_SECS: u64 = 3600;
/// Most mails handed to the bot per poll; the rest become one notice.
pub const MAX_PER_POLL_CEILING: usize = 50;

/// How much of a mail the bot gets to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelMode {
    /// Sender, subject, date and the first few hundred characters (default).
    #[default]
    Summary,
    /// The whole plain-text body, up to `body_chars`.
    Full,
}

/// What the owner chose for the e-mail channel (`<data dir>/email-channel.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EmailChannelSettings {
    /// Off until the owner turns it on.
    pub enabled: bool,
    /// Name of a saved e-mail account (the password stays in the secret store).
    pub account: String,
    /// The mailbox folder to watch.
    pub folder: String,
    /// The bot that receives the mails (its id); `None` = no bot yet, nothing is delivered.
    pub bot: Option<String>,
    /// Seconds between polls.
    pub poll_secs: u64,
    /// Summary (default) or full text.
    pub mode: ChannelMode,
    /// Characters of the snippet in summary mode.
    pub summary_chars: usize,
    /// Characters of the body in full mode.
    pub body_chars: usize,
    /// Most mails delivered per poll.
    pub max_per_poll: usize,
    /// Allow the bot to *propose* replies (each one still needs the owner's OK).
    pub replies_enabled: bool,
}

impl Default for EmailChannelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            account: String::new(),
            folder: "INBOX".into(),
            bot: None,
            poll_secs: 120,
            mode: ChannelMode::Summary,
            summary_chars: 300,
            body_chars: 4000,
            max_per_poll: 10,
            replies_enabled: false,
        }
    }
}

impl EmailChannelSettings {
    /// Check every field; the message is shown to the owner as is.
    ///
    /// # Errors
    /// [`EmailError::Input`] naming the first bad field in plain words.
    pub fn validate(&self) -> Result<(), EmailError> {
        let bad = |m: &str| Err(EmailError::Input(m.to_owned()));
        if self.enabled && self.account.trim().is_empty() {
            return bad("choose an e-mail account first");
        }
        if self.account.len() > 64 || self.account.chars().any(char::is_control) {
            return bad("the account name is not valid");
        }
        if self.folder.trim().is_empty() || self.folder.len() > 200 {
            return bad("choose a folder to watch");
        }
        crate::imap::check_folder(&self.folder)?;
        if !(MIN_POLL_SECS..=MAX_POLL_SECS).contains(&self.poll_secs) {
            return bad("check for new mail every 1 to 60 minutes");
        }
        if !(50..=2000).contains(&self.summary_chars) {
            return bad("the summary length must be between 50 and 2000 characters");
        }
        if !(200..=MAX_BODY_CHARS).contains(&self.body_chars) {
            return bad("the full-text length must be between 200 and 20000 characters");
        }
        if !(1..=MAX_PER_POLL_CEILING).contains(&self.max_per_poll) {
            return bad("deliver between 1 and 50 mails per check");
        }
        if let Some(bot) = &self.bot
            && (bot.len() > 64 || bot.chars().any(char::is_control))
        {
            return bad("the bot id is not valid");
        }
        Ok(())
    }

    /// Characters of the body to fetch for the current mode.
    #[must_use]
    pub fn fetch_chars(&self) -> usize {
        match self.mode {
            ChannelMode::Summary => self.summary_chars,
            ChannelMode::Full => self.body_chars,
        }
    }
}

//! Durable ownership of Telegram updates before advancing `getUpdates`' offset.
//!
//! Every transition is appended and fsynced before it becomes visible in memory.
//! Completed payloads are retained on disk, but only pending payloads and compact
//! ranges of seen IDs are kept in memory. A final torn line is discarded on open;
//! a corrupt complete record fails closed. One bridge owns a state directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;
use protocol::{ChatId, RunId};
use serde::{Deserialize, Serialize};

use crate::bot_api::Update;
use crate::channel_trait::IncomingKind;
use crate::daemon_client::SendOutcome;
use crate::error::{Result, TelegramError};

pub(crate) const MAX_ATTEMPTS: u32 = 8;
const MAX_RETRY_DELAY_MS: i64 = 5 * 60 * 1000;
// The Bot API reader caps the complete batch at 8 MiB. A record also has metadata.
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InboxState {
    Pending,
    Done,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeliveryReceipt {
    pub(crate) daemon_chat: ChatId,
    pub(crate) outcome: SendOutcome,
    pub(crate) sent_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InboxEntry {
    bot_key: String,
    pub(crate) update_id: i64,
    pub(crate) received_ms: i64,
    pub(crate) payload: Update,
    pub(crate) state: InboxState,
    pub(crate) attempts: u32,
    #[serde(default)]
    next_attempt_ms: i64,
    /// The daemon acknowledged the side effect. Never resend it after a local output failure.
    #[serde(default)]
    pub(crate) delivered: bool,
    #[serde(default)]
    pub(crate) delivery: Option<DeliveryReceipt>,
    /// A dead-letter notification is attempted at most once, including across restarts.
    #[serde(default)]
    dead_notified: bool,
    #[serde(default)]
    pub(crate) stop_chat_id: Option<i64>,
    #[serde(default)]
    pub(crate) cancel_run: Option<RunId>,
}

impl InboxEntry {
    pub(crate) fn is_priority(&self) -> bool {
        self.payload
            .message
            .as_ref()
            .and_then(|m| m.text.as_deref())
            .is_some_and(|text| {
                let text = text.trim();
                text == "/stop" || text.starts_with("/stop@")
            })
    }
}

#[derive(Default)]
struct InboxInner {
    pending: BTreeMap<i64, InboxEntry>,
    /// Inclusive, non-overlapping ranges. Telegram normally extends one range.
    seen: BTreeMap<i64, i64>,
    stopped_before: BTreeMap<i64, i64>,
    poisoned: bool,
}

impl InboxInner {
    fn contains(&self, id: i64) -> bool {
        self.seen
            .range(..=id)
            .next_back()
            .is_some_and(|(_, end)| *end >= id)
    }

    fn remember(&mut self, id: i64) {
        if self.contains(id) {
            return;
        }
        let before = self
            .seen
            .range(..id)
            .next_back()
            .map(|(start, end)| (*start, *end));
        let after = self
            .seen
            .range(id..)
            .next()
            .map(|(start, end)| (*start, *end));
        let mut start = id;
        let mut end = id;
        if let Some((previous_start, previous_end)) = before
            && previous_end.checked_add(1) == Some(id)
        {
            start = previous_start;
            self.seen.remove(&previous_start);
        }
        if let Some((next_start, next_end)) = after
            && id.checked_add(1) == Some(next_start)
        {
            end = next_end;
            self.seen.remove(&next_start);
        }
        self.seen.insert(start, end);
    }

    fn apply(&mut self, entry: InboxEntry) {
        self.remember(entry.update_id);
        if let Some(chat) = entry.stop_chat_id {
            let stopped = self.stopped_before.entry(chat).or_insert(entry.update_id);
            *stopped = (*stopped).max(entry.update_id);
        }
        if entry.state == InboxState::Pending {
            self.pending.insert(entry.update_id, entry);
        } else {
            self.pending.remove(&entry.update_id);
        }
    }
}

pub(crate) struct Inbox {
    path: Option<PathBuf>,
    bot_key: String,
    inner: Mutex<InboxInner>,
    #[cfg(test)]
    fail_writes: std::sync::atomic::AtomicBool,
}

fn storage_error(error: impl std::fmt::Display) -> TelegramError {
    TelegramError::Storage(format!("telegram inbox: {error}"))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn open_log(path: &Path) -> Result<File> {
    std::fs::create_dir_all(parent(path)).map_err(storage_error)?;
    let mut options = OpenOptions::new();
    // Append, not write. On Windows that is `FILE_APPEND_DATA` without
    // `FILE_WRITE_DATA`, so `File::set_len` on this handle returns Access Denied
    // (os error 5). Truncation goes through `truncate_durable`.
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(storage_error)
}

/// Shrink `path` to `len` and fsync the new size.
///
/// Every append-mode handle must already be closed. Windows denies `set_len` on
/// those handles, and a replace over a file that still has an open handle fails
/// the same way. The valid prefix is left in place; only the torn tail is dropped.
fn truncate_durable(path: &Path, len: u64) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(storage_error)?;
    file.set_len(len).map_err(storage_error)?;
    file.sync_all().map_err(storage_error)?;
    Ok(())
}

/// Apply complete records. `Some(len)` is a torn final record: the caller
/// truncates to `len` only after this function drops its append handle.
fn replay(path: &Path, bot_key: &str, inner: &mut InboxInner) -> Result<Option<u64>> {
    let mut reader = BufReader::new(open_log(path)?);
    let mut valid_bytes = 0u64;
    loop {
        let mut line = Vec::new();
        let count = (&mut reader)
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(storage_error)?;
        if count == 0 {
            break;
        }
        if count > MAX_RECORD_BYTES {
            return Err(storage_error("record too large"));
        }
        if line.last() != Some(&b'\n') {
            // No offset could have been acknowledged for a record without its
            // newline and fsync. Discard only the unfinished final append.
            return Ok(Some(valid_bytes));
        }
        let entry: InboxEntry = serde_json::from_slice(&line).map_err(storage_error)?;
        if entry.update_id != entry.payload.update_id {
            return Err(storage_error("record update ID differs from payload"));
        }
        if entry.bot_key == bot_key {
            inner.apply(entry);
        }
        valid_bytes += count as u64;
    }
    Ok(None)
}

impl Inbox {
    pub(crate) fn open(path: Option<PathBuf>, bot_key: &str) -> Result<Self> {
        let mut inner = InboxInner::default();
        if let Some(path) = &path {
            // `replay` closes the append handle before we truncate. Holding it
            // across `set_len` is Access Denied on Windows.
            if let Some(valid_bytes) = replay(path, bot_key, &mut inner)? {
                truncate_durable(path, valid_bytes)?;
            }
        }
        Ok(Self {
            path,
            bot_key: bot_key.to_owned(),
            inner: Mutex::new(inner),
            #[cfg(test)]
            fail_writes: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn append(&self, inner: &mut InboxInner, entries: Vec<InboxEntry>) -> Result<()> {
        if inner.poisoned {
            return Err(storage_error(
                "previous write could not be rolled back; reopen the inbox",
            ));
        }
        #[cfg(test)]
        if self.fail_writes.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(storage_error("injected write failure"));
        }
        if entries.is_empty() {
            return Ok(());
        }
        if let Some(path) = &self.path {
            // Serialise all records before touching the file. Memory is bounded by the
            // already bounded Bot API batch; completed payloads are not retained.
            let mut encoded = Vec::new();
            for entry in &entries {
                let start = encoded.len();
                serde_json::to_writer(&mut encoded, entry).map_err(storage_error)?;
                encoded.push(b'\n');
                if encoded.len() - start > MAX_RECORD_BYTES {
                    return Err(storage_error("record too large"));
                }
            }
            let mut file = open_log(path)?;
            let before = file.metadata().map_err(storage_error)?.len();
            let written = file.write_all(&encoded).and_then(|()| file.sync_all());
            #[cfg(unix)]
            let written = written.and_then(|()| File::open(parent(path))?.sync_all());
            if let Err(error) = written {
                // A partial append must not be followed by another record. If rollback
                // itself fails, reject all subsequent writes/offset advances.
                // Close the append handle first: `set_len` on it is Access Denied on
                // Windows, which would poison a log we could still have repaired.
                drop(file);
                inner.poisoned = truncate_durable(path, before).is_err();
                return Err(storage_error(error));
            }
        }
        for entry in entries {
            inner.apply(entry);
        }
        Ok(())
    }

    pub(crate) fn enqueue(&self, updates: &[Update], received_ms: i64) -> Result<()> {
        let mut inner = self.inner.lock();
        let mut in_batch = BTreeSet::new();
        let entries = updates
            .iter()
            .filter(|update| !inner.contains(update.update_id) && in_batch.insert(update.update_id))
            .map(|update| InboxEntry {
                bot_key: self.bot_key.clone(),
                update_id: update.update_id,
                received_ms,
                payload: update.clone(),
                state: InboxState::Pending,
                attempts: 0,
                next_attempt_ms: received_ms,
                delivered: false,
                delivery: None,
                dead_notified: false,
                stop_chat_id: None,
                cancel_run: None,
            })
            .collect();
        self.append(&mut inner, entries)
    }

    fn change(&self, id: i64, update: impl FnOnce(&mut InboxEntry)) -> Result<()> {
        let mut inner = self.inner.lock();
        let Some(mut entry) = inner.pending.get(&id).cloned() else {
            return Err(storage_error(format!("pending update {id} is missing")));
        };
        update(&mut entry);
        self.append(&mut inner, vec![entry])
    }

    pub(crate) fn get(&self, id: i64) -> Option<InboxEntry> {
        self.inner.lock().pending.get(&id).cloned()
    }

    /// Ordered normal delivery does not jump over an older update awaiting retry.
    /// `/stop` has an independent worker so it can interrupt a slow send or retry.
    pub(crate) fn next(&self, priority: bool, now_ms: i64) -> Option<InboxEntry> {
        self.inner
            .lock()
            .pending
            .values()
            .find(|e| e.is_priority() == priority)
            .filter(|e| e.next_attempt_ms <= now_ms)
            .cloned()
    }

    pub(crate) fn retry_delay(&self, priority: bool, now_ms: i64) -> Option<Duration> {
        self.inner
            .lock()
            .pending
            .values()
            .find(|e| e.is_priority() == priority)
            .map(|e| {
                Duration::from_millis(
                    u64::try_from(e.next_attempt_ms.saturating_sub(now_ms)).unwrap_or(0),
                )
            })
    }

    pub(crate) fn begin_attempt(&self, id: i64, now_ms: i64) -> Result<()> {
        self.change(id, |entry| {
            entry.attempts = entry.attempts.saturating_add(1);
            let delay =
                (1000i64 << entry.attempts.saturating_sub(1).min(16)).min(MAX_RETRY_DELAY_MS);
            entry.next_attempt_ms = now_ms.saturating_add(delay);
        })
    }

    pub(crate) fn stop(&self, id: i64, chat_id: i64, run: Option<RunId>) -> Result<()> {
        self.change(id, |entry| {
            entry.stop_chat_id = Some(chat_id);
            entry.cancel_run = run;
        })
    }

    pub(crate) fn cancelled(&self, id: i64, chat_id: i64) -> bool {
        self.inner
            .lock()
            .stopped_before
            .get(&chat_id)
            .is_some_and(|stopped| id < *stopped)
    }

    pub(crate) fn delivered(&self, id: i64, receipt: Option<DeliveryReceipt>) -> Result<()> {
        self.change(id, |entry| {
            entry.delivered = true;
            entry.delivery = receipt;
        })
    }

    pub(crate) fn done(&self, id: i64) -> Result<()> {
        self.change(id, |entry| entry.state = InboxState::Done)
    }

    /// Persist the terminal state and notification intent before a best-effort send.
    /// Telegram has no idempotent `sendMessage`, so a crash can suppress that notice,
    /// but a duplicate/restart cannot spam it. The dead record remains in the log.
    pub(crate) fn dead(&self, id: i64) -> Result<Option<i64>> {
        let chat = self
            .get(id)
            .and_then(|entry| entry.payload.into_incoming())
            .and_then(|incoming| {
                matches!(
                    incoming.kind,
                    IncomingKind::Text(_) | IncomingKind::Callback { .. }
                )
                .then_some(incoming.chat.0)
            });
        self.change(id, |entry| {
            entry.state = InboxState::Dead;
            entry.dead_notified = true;
        })?;
        Ok(chat)
    }

    #[cfg(test)]
    pub(crate) fn fail_writes(&self, enabled: bool) {
        self.fail_writes
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(id: i64) -> Update {
        Update {
            update_id: id,
            message: None,
            callback_query: None,
        }
    }

    #[test]
    fn recovery_discards_only_a_torn_final_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram-inbox.jsonl");
        let inbox = Inbox::open(Some(path.clone()), "bot").unwrap();
        inbox.enqueue(&[update(1), update(2)], 100).unwrap();
        inbox.done(1).unwrap();
        drop(inbox);
        let valid_len = path.metadata().unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"update_id\":2,")
            .unwrap();
        let reopened = Inbox::open(Some(path.clone()), "bot").unwrap();
        assert_eq!(path.metadata().unwrap().len(), valid_len);
        assert!(reopened.get(1).is_none());
        assert_eq!(reopened.next(false, 100).unwrap().update_id, 2);
        reopened.enqueue(&[update(1)], 200).unwrap();
        assert!(
            reopened.get(1).is_none(),
            "completed record was redelivered"
        );
        drop(reopened);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"invalid complete line\n")
            .unwrap();
        assert!(Inbox::open(Some(path), "bot").is_err());
    }

    #[test]
    fn completed_payloads_are_released_and_seen_ids_coalesce() {
        let inbox = Inbox::open(None, "bot").unwrap();
        for id in (1..=100).step_by(2) {
            inbox.enqueue(&[update(id)], 0).unwrap();
            inbox.done(id).unwrap();
        }
        for id in (2..=100).step_by(2) {
            inbox.enqueue(&[update(id)], 0).unwrap();
            inbox.done(id).unwrap();
        }
        let inner = inbox.inner.lock();
        assert!(inner.pending.is_empty());
        assert_eq!(inner.seen, BTreeMap::from([(1, 100)]));
    }

    #[test]
    fn append_failure_does_not_publish_batch_or_delivery_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let inbox = Inbox::open(Some(path.clone()), "bot").unwrap();
        inbox.enqueue(&[update(1)], 0).unwrap();
        let before = std::fs::read(&path).unwrap();
        inbox.fail_writes(true);
        assert!(inbox.enqueue(&[update(2)], 0).is_err());
        assert!(inbox.delivered(1, None).is_err());
        assert!(inbox.get(2).is_none());
        assert!(!inbox.get(1).unwrap().delivered);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn retry_backoff_is_durable_ordered_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let inbox = Inbox::open(Some(path.clone()), "bot").unwrap();
        inbox.enqueue(&[update(1), update(2)], 1000).unwrap();
        inbox.begin_attempt(1, 1000).unwrap();
        assert!(inbox.next(false, 1999).is_none());
        assert_eq!(inbox.next(false, 2000).unwrap().update_id, 1);
        drop(inbox);
        let inbox = Inbox::open(Some(path), "bot").unwrap();
        assert_eq!(inbox.retry_delay(false, 1000), Some(Duration::from_secs(1)));
        for _ in 0..12 {
            inbox.begin_attempt(1, 1000).unwrap();
        }
        assert_eq!(
            inbox.retry_delay(false, 1000),
            Some(Duration::from_secs(300))
        );
        inbox.dead(1).unwrap();
        assert_eq!(inbox.next(false, 1000).unwrap().update_id, 2);
    }

    #[test]
    fn changing_bot_token_does_not_deliver_another_bots_pending_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.jsonl");
        let first = Inbox::open(Some(path.clone()), "bot-one").unwrap();
        first.enqueue(&[update(1)], 0).unwrap();
        drop(first);
        let second = Inbox::open(Some(path.clone()), "bot-two").unwrap();
        assert!(second.next(false, 0).is_none());
        second.enqueue(&[update(1)], 0).unwrap();
        second.done(1).unwrap();
        drop(second);
        assert!(
            Inbox::open(Some(path), "bot-one")
                .unwrap()
                .next(false, 0)
                .is_some()
        );
    }
}

//! Reading what comes back to a mailbox: replies, out-of-office notices and delivery reports are
//! matched to the send log ([`MailGuard::apply_inbound`]) and acted on without a model.
//!
//! Everything here treats the mail as untrusted data. It only decides between the fixed actions
//! of [`crate::guard::InboundAction`]; no text of the mail reaches a tool or a prompt from here.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::connector::Scoped;
use crate::dsn::DeliveryStatus;
use crate::guard::{InboundMail, InboundOutcome, MAX_REFS, MailGuard};
use crate::imap::SearchQuery;
use crate::message::MessageDetail;

/// Senders of delivery reports looked for by [`sweep_reports`].
const REPORT_SENDERS: [&str; 2] = ["mailer-daemon", "postmaster"];
/// Reports older than this are not read again.
const SWEEP_DAYS: u64 = 7;
/// Most reports read per sender and sweep.
const SWEEP_LIMIT: usize = 20;
/// Body characters read of a report or a reply.
const SWEEP_BODY_CHARS: usize = 8_000;

/// Match `detail` (a message of `account`) to what was sent, and act on a confident match.
#[must_use]
pub fn apply_to_message(
    guard: &MailGuard,
    account: &str,
    detail: &MessageDetail,
) -> InboundOutcome {
    // `In-Reply-To` first, then the newest ids of `References` (the one we answered is last):
    // the matcher reads a bounded number of them.
    let mut references = detail.in_reply_to.clone();
    let room = MAX_REFS.saturating_sub(references.len());
    let skip = detail.references.len().saturating_sub(room);
    references.extend(detail.references.iter().skip(skip).cloned());
    // The API mailboxes give no delivery-status part: the fields are read from the text,
    // which counts only when it comes from a mail system (see `DeliveryStatus::structured`).
    let dsn: Option<DeliveryStatus> = detail
        .delivery_status
        .clone()
        .or_else(|| DeliveryStatus::parse(&detail.body_text, false));
    let id = detail
        .message_id
        .as_deref()
        .or(detail.summary.id.as_deref())
        .unwrap_or_default();
    guard.apply_inbound(&InboundMail {
        account,
        id,
        from_addr: &detail.summary.from_addr,
        subject: &detail.summary.subject,
        body: &detail.body_text,
        auto_submitted: detail
            .summary
            .automation_headers
            .as_ref()
            .map(|h| h.auto_submitted.as_str()),
        references: &references,
        thread_id: detail.thread_id.as_deref(),
        dsn: dsn.as_ref(),
        sender_authenticated: detail.sender_authenticated,
        from_junk: detail.from_junk.or(detail.summary.from_junk),
    })
}

/// `YYYY-MM-DD` of the day `days` ago (UTC).
fn day_ago(days: u64) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .saturating_sub(days * 86_400);
    crate::guard::utc_text(secs)[..10].to_owned()
}

/// Read the recent delivery reports in `account`'s inbox and apply them. A hard bounce puts the
/// failed address on the suppression list, a temporary failure is counted. Reading the same
/// report again changes nothing, so a sweep can run as often as the caller likes. Returns what
/// was done for each report that led to an action.
///
/// A mailbox that cannot be searched, or a report that cannot be read, is skipped: this is
/// housekeeping and never fails a caller.
pub async fn sweep_reports(
    scoped: &Scoped<'_>,
    guard: &MailGuard,
    account: &str,
) -> Vec<InboundOutcome> {
    let mut done = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for sender in REPORT_SENDERS {
        let query = SearchQuery {
            from: Some(sender.to_owned()),
            since: Some(day_ago(SWEEP_DAYS)),
            ..SearchQuery::default()
        };
        let Ok(found) = tokio::time::timeout(
            Duration::from_secs(30),
            scoped.search(Some(account), "INBOX", &query, SWEEP_LIMIT),
        )
        .await
        .unwrap_or(Err(crate::EmailError::Timeout(30))) else {
            continue;
        };
        for m in found.messages {
            let id = if m.uid > 0 {
                m.uid.to_string()
            } else if let Some(id) = m.id.clone() {
                id
            } else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(Ok(detail)) = tokio::time::timeout(
                Duration::from_secs(30),
                scoped.fetch_id(Some(account), "INBOX", &id, SWEEP_BODY_CHARS),
            )
            .await
            else {
                continue;
            };
            let out = apply_to_message(guard, account, &detail);
            if out
                .actions
                .iter()
                .any(|a| !matches!(a, crate::guard::InboundAction::None { .. }))
            {
                done.push(out);
            }
        }
    }
    done
}

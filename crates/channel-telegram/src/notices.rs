//! What every bridge does with the daemon's owner notices, so Telegram and Discord behave the
//! same: follow the notice stream (reconnecting, without losing or repeating a notice), and
//! remember the newest open question so `/answer` has something to answer.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use channel_api::notice::{NoticeKind, OwnerNotice};
use futures::future::BoxFuture;
use futures_util::StreamExt;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::daemon_client::{DaemonClient, NoticeEvent};

/// Longest wait between two connection attempts.
const MAX_RECONNECT: Duration = Duration::from_secs(30);
/// Notices the book keeps.
const BOOK_MAX: usize = 32;

/// Follow the daemon's owner-notice stream until `cancel`, calling `on_notice` for each notice
/// in order. A dropped connection is reopened with the last `seq` handled, so a notice pushed in
/// between is replayed once. Before the first notice the daemon decides what a (re)connecting
/// bridge has not been handed yet, from the delivery cursor it keeps for it.
/// `retry` is the first wait after a failed connection (it doubles up to 30 s).
pub async fn follow_notices<F>(
    client: Arc<dyn DaemonClient>,
    cancel: CancellationToken,
    retry: Duration,
    mut on_notice: F,
) where
    F: FnMut(OwnerNotice) -> BoxFuture<'static, ()> + Send,
{
    let mut after: Option<u64> = None;
    let mut failures = 0u32;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let subscribed = tokio::select! {
            () = cancel.cancelled() => return,
            r = client.subscribe_notices(after) => r,
        };
        match subscribed {
            Ok(mut stream) => {
                failures = 0;
                loop {
                    let ev = tokio::select! {
                        () = cancel.cancelled() => return,
                        ev = stream.next() => ev,
                    };
                    match ev {
                        None | Some(NoticeEvent::Lagged) => break,
                        // Not a cursor: the newest `seq` says nothing of the replay that comes
                        // after it. A connection that drops before the replay arrived must ask
                        // again as before, so only a notice that was handled moves `after`. The
                        // daemon itself remembers what it handed over to this bridge.
                        Some(NoticeEvent::Hello { .. }) => {}
                        Some(NoticeEvent::Notice(n)) => {
                            after = Some(after.map_or(n.seq, |a| a.max(n.seq)));
                            on_notice(*n).await;
                        }
                    }
                }
            }
            Err(_) => failures = failures.saturating_add(1),
        }
        let shift = failures.saturating_sub(1).min(8);
        let wait = retry.saturating_mul(1u32 << shift).min(MAX_RECONNECT);
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

struct Entry {
    notice: OwnerNotice,
    /// Whom the notice was shown to (a Telegram user id, a Discord user id): nobody else can
    /// answer or retry it through this bridge.
    owner: String,
    answered: bool,
}

/// The newest notices a bridge showed, so `/answer` knows which question list it answers and
/// whose it is.
#[derive(Default)]
pub struct NoticeBook {
    items: Mutex<VecDeque<Entry>>,
}

impl NoticeBook {
    /// Remember a notice that was shown to `owner`.
    pub fn remember(&self, notice: &OwnerNotice, owner: &str) {
        let mut items = self.items.lock();
        items.push_back(Entry {
            notice: notice.clone(),
            owner: owner.to_owned(),
            answered: false,
        });
        while items.len() > BOOK_MAX {
            items.pop_front();
        }
    }

    /// The newest blocked notice shown to `owner` that still has questions nobody answered.
    #[must_use]
    pub fn open_question(&self, owner: &str) -> Option<OwnerNotice> {
        self.items
            .lock()
            .iter()
            .rev()
            .find(|e| {
                e.owner == owner
                    && !e.answered
                    && e.notice.kind == NoticeKind::Blocked
                    && !e.notice.questions.is_empty()
            })
            .map(|e| e.notice.clone())
    }

    /// Forget everything (a replacement owner must not inherit open questions).
    pub fn clear(&self) {
        self.items.lock().clear();
    }

    /// The questions of `id`, shown to `owner`, were taken.
    pub fn mark_answered(&self, id: &str, owner: &str) {
        for e in self
            .items
            .lock()
            .iter_mut()
            .filter(|e| e.notice.id == id && e.owner == owner)
        {
            e.answered = true;
        }
    }

    /// Whether `id` is a notice this bridge showed to `owner` (a retry button may only name one).
    #[must_use]
    pub fn knows(&self, id: &str, owner: &str) -> bool {
        self.items
            .lock()
            .iter()
            .any(|e| e.notice.id == id && e.owner == owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use channel_api::notice::{NoticeItem, NoticeSource};
    use futures::stream::BoxStream;

    use crate::daemon_client::{
        ApprovalDecision, ApprovalScope, BotSummary, ChatSummary, DaemonStatus, SendOutcome,
    };
    use crate::error::{Result, TelegramError};
    use protocol::{BotId, ChatId, Envelope, RunId, ToolCallId};

    pub(crate) fn notice(seq: u64, kind: NoticeKind) -> OwnerNotice {
        OwnerNotice {
            seq,
            id: format!("00000000-0000-0000-0000-{seq:012}"),
            at_ms: 1,
            kind,
            source: NoticeSource::Mission,
            title: "Backlinks".into(),
            bot_name: "Scout".into(),
            bot_id: None,
            mission_id: Some("m".into()),
            run_id: None,
            chat_id: None,
            reason: None,
            progress: None,
            done: vec![],
            failed: vec![],
            questions: vec![],
            app_only: vec![],
            can_retry: false,
            late: false,
        }
    }

    /// Each `subscribe_notices` call pops the next scripted connection.
    struct Scripted {
        connections: Mutex<VecDeque<Result<Vec<NoticeEvent>>>>,
        afters: Mutex<Vec<Option<u64>>>,
    }

    #[async_trait]
    impl DaemonClient for Scripted {
        async fn list_bots(&self) -> Result<Vec<BotSummary>> {
            Ok(vec![])
        }
        async fn create_chat(&self, _: BotId, _: Option<&str>) -> Result<ChatSummary> {
            Err(TelegramError::Daemon("unused".into()))
        }
        async fn send_message(&self, _: ChatId, _: &str) -> Result<SendOutcome> {
            Err(TelegramError::Daemon("unused".into()))
        }
        async fn decide_approval(
            &self,
            _: ToolCallId,
            _: ApprovalDecision,
            _: ApprovalScope,
            _: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
        async fn answer_question(&self, _: ToolCallId, _: &str) -> Result<()> {
            Ok(())
        }
        async fn cancel_run(&self, _: RunId) -> Result<()> {
            Ok(())
        }
        async fn status(&self) -> Result<DaemonStatus> {
            Err(TelegramError::Daemon("unused".into()))
        }
        async fn subscribe_run(&self, _: RunId) -> Result<BoxStream<'static, Envelope>> {
            Ok(Box::pin(futures::stream::empty()))
        }
        async fn subscribe_notices(
            &self,
            after: Option<u64>,
        ) -> Result<BoxStream<'static, NoticeEvent>> {
            self.afters.lock().push(after);
            match self.connections.lock().pop_front() {
                Some(Ok(events)) => Ok(Box::pin(futures::stream::iter(events))),
                Some(Err(e)) => Err(e),
                None => Ok(Box::pin(futures::stream::pending())),
            }
        }
    }

    #[tokio::test]
    async fn a_dropped_connection_reconnects_with_the_last_seq_and_repeats_nothing() {
        let client = Arc::new(Scripted {
            connections: Mutex::new(VecDeque::from([
                Ok(vec![
                    NoticeEvent::Hello { latest_seq: 4 },
                    NoticeEvent::Notice(Box::new(notice(5, NoticeKind::Finished))),
                ]),
                Err(TelegramError::Daemon("down".into())),
                Ok(vec![
                    NoticeEvent::Hello { latest_seq: 6 },
                    NoticeEvent::Notice(Box::new(notice(6, NoticeKind::Blocked))),
                    NoticeEvent::Lagged,
                ]),
            ])),
            afters: Mutex::new(vec![]),
        });
        let seen = Arc::new(Mutex::new(Vec::<u64>::new()));
        let cancel = CancellationToken::new();
        let task = {
            let (client, cancel, seen) = (client.clone(), cancel.clone(), seen.clone());
            tokio::spawn(async move {
                follow_notices(client, cancel, Duration::from_millis(5), move |n| {
                    let seen = seen.clone();
                    Box::pin(async move { seen.lock().push(n.seq) })
                })
                .await;
            })
        };
        for _ in 0..200 {
            if client.afters.lock().len() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(*seen.lock(), vec![5, 6], "every notice once, in order");
        let afters = client.afters.lock().clone();
        assert_eq!(
            &afters[..4],
            &[None, Some(5), Some(5), Some(6)],
            "a new bridge starts fresh, then always asks after the last seq"
        );
    }

    #[tokio::test]
    async fn a_connection_dropped_after_hello_does_not_skip_the_replay() {
        // The daemon says "latest 9" and would replay 7..=9, but the connection breaks first.
        let client = Arc::new(Scripted {
            connections: Mutex::new(VecDeque::from([
                Ok(vec![NoticeEvent::Hello { latest_seq: 9 }]),
                Ok(vec![
                    NoticeEvent::Hello { latest_seq: 9 },
                    NoticeEvent::Notice(Box::new(notice(7, NoticeKind::Finished))),
                    NoticeEvent::Notice(Box::new(notice(9, NoticeKind::Failed))),
                ]),
            ])),
            afters: Mutex::new(vec![]),
        });
        let seen = Arc::new(Mutex::new(Vec::<u64>::new()));
        let cancel = CancellationToken::new();
        let task = {
            let (client, cancel, seen) = (client.clone(), cancel.clone(), seen.clone());
            tokio::spawn(async move {
                follow_notices(client, cancel, Duration::from_millis(5), move |n| {
                    let seen = seen.clone();
                    Box::pin(async move { seen.lock().push(n.seq) })
                })
                .await;
            })
        };
        for _ in 0..200 {
            if client.afters.lock().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(*seen.lock(), vec![7, 9]);
        let afters = client.afters.lock().clone();
        assert_eq!(
            &afters[..3],
            &[None, None, Some(9)],
            "Hello is not a cursor: the reconnect asks again as a fresh bridge"
        );
    }

    #[test]
    fn the_book_offers_the_newest_open_question_and_forgets_answered_ones() {
        let book = NoticeBook::default();
        assert!(book.open_question("me").is_none());
        let mut a = notice(1, NoticeKind::Blocked);
        a.questions = vec![NoticeItem {
            key: "k".into(),
            title: "t".into(),
            text: None,
        }];
        let mut b = notice(2, NoticeKind::Blocked);
        b.questions = a.questions.clone();
        let done = notice(3, NoticeKind::Finished);
        for n in [&a, &b, &done] {
            book.remember(n, "me");
        }
        assert_eq!(
            book.open_question("me").unwrap().id,
            b.id,
            "newest question first"
        );
        book.mark_answered(&b.id, "me");
        assert_eq!(book.open_question("me").unwrap().id, a.id);
        book.mark_answered(&a.id, "me");
        assert!(book.open_question("me").is_none());
        assert!(book.knows(&done.id, "me") && !book.knows("nope", "me"));
    }

    #[test]
    fn a_notice_belongs_to_the_user_it_was_shown_to() {
        let book = NoticeBook::default();
        let mut a = notice(1, NoticeKind::Blocked);
        a.questions = vec![NoticeItem {
            key: "k".into(),
            title: "t".into(),
            text: None,
        }];
        book.remember(&a, "alice");
        assert!(book.open_question("bob").is_none());
        assert!(!book.knows(&a.id, "bob"));
        book.mark_answered(&a.id, "bob");
        assert!(
            book.open_question("alice").is_some(),
            "someone else's answer closes nothing"
        );
        assert!(book.knows(&a.id, "alice"));
    }
}

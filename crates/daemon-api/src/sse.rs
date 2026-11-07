//! `GET /v1/events`: server-sent events of [`protocol::Envelope`]s.
//!
//! Envelope `seq` is per run, so resumption is cursor based:
//!
//! * `?run_id=R` follows one run.  The SSE `id` is the plain `seq`; with
//!   `Last-Event-ID: N` the events `> N` are replayed from the run's NDJSON log
//!   (everything when absent) and the stream then goes live, without gaps or
//!   duplicates.  The stream ends after the run's `run_finished`.
//! * No `run_id`: live events of all runs; the SSE `id` is `<run_id>:<seq>`.
//!   `Last-Event-ID: <run_id>:<seq>` replays that run's missed events first.
//!   (Other runs' missed events are not replayed; follow them per run.)
//!
//! A subscriber that lags behind the broadcast buffer recovers a run-scoped
//! stream from the log; a global one skips ahead.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::PathBuf;

use agent_core::StreamItem;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::stream::{self, Stream};
use protocol::{Envelope, Event, RunId};
use serde::Deserialize;
use storage::read_event_log;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;

/// Query of `GET /v1/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Follow only this run.
    pub run_id: Option<Uuid>,
}

struct Cursor {
    scoped: bool,
    run: Option<RunId>,
    last: Option<u64>,
}

fn parse_cursor(scoped_run: Option<RunId>, header: Option<&str>) -> Result<Cursor, ApiError> {
    let bad = || ApiError::bad_request("malformed Last-Event-ID");
    let Some(h) = header.map(str::trim).filter(|h| !h.is_empty()) else {
        return Ok(Cursor {
            scoped: scoped_run.is_some(),
            run: scoped_run,
            last: None,
        });
    };
    if let Some(run) = scoped_run {
        let seq = h.parse::<u64>().map_err(|_| bad())?;
        return Ok(Cursor {
            scoped: true,
            run: Some(run),
            last: Some(seq),
        });
    }
    let (run, seq) = h.split_once(':').ok_or_else(bad)?;
    Ok(Cursor {
        scoped: false,
        run: Some(RunId(run.parse::<Uuid>().map_err(|_| bad())?)),
        last: Some(seq.parse::<u64>().map_err(|_| bad())?),
    })
}

struct Feed {
    rx: broadcast::Receiver<StreamItem>,
    backlog: VecDeque<StreamItem>,
    scoped: bool,
    /// Run whose seqs are tracked for de-duplication.
    run: Option<RunId>,
    last: Option<u64>,
    data_dir: PathBuf,
    cancel: CancellationToken,
    done: bool,
    /// The run's log already holds its `run_finished`: end once drained.
    finished_in_log: bool,
}

impl Feed {
    async fn reload(&mut self) {
        let Some(run) = self.run else { return };
        let dir = self.data_dir.clone();
        let after = self.last;
        let logged = tokio::task::spawn_blocking(move || read_event_log(&dir, run))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        self.finished_in_log |= logged
            .iter()
            .any(|e| matches!(e.event, Event::RunFinished(_)));
        self.backlog = logged
            .into_iter()
            .filter(|e| after.is_none_or(|a| e.seq > a))
            .map(|envelope| StreamItem {
                run_id: run,
                envelope,
            })
            .collect();
    }

    /// Next item to deliver, `None` when the stream is over.
    async fn next_item(&mut self) -> Option<StreamItem> {
        loop {
            if self.done {
                return None;
            }
            if let Some(item) = self.backlog.pop_front() {
                if self.accept(&item) {
                    return Some(item);
                }
                continue;
            }
            if self.scoped && self.finished_in_log {
                return None;
            }
            let received = tokio::select! {
                () = self.cancel.cancelled() => return None,
                r = self.rx.recv() => r,
            };
            match received {
                Ok(item) => {
                    if self.scoped && Some(item.run_id) != self.run {
                        continue;
                    }
                    if self.accept(&item) {
                        return Some(item);
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if self.scoped {
                        self.reload().await;
                    }
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }

    /// De-duplicate against the replayed run and update the cursor.
    fn accept(&mut self, item: &StreamItem) -> bool {
        if Some(item.run_id) == self.run {
            let seq = item.envelope.seq;
            if self.last.is_some_and(|l| seq <= l) {
                return false;
            }
            self.last = Some(seq);
            if self.scoped && matches!(item.envelope.event, Event::RunFinished(_)) {
                self.done = true;
            }
        }
        true
    }
}

fn to_sse(item: &StreamItem, scoped: bool) -> SseEvent {
    let env: &Envelope = &item.envelope;
    let id = if scoped {
        env.seq.to_string()
    } else {
        format!("{}:{}", item.run_id, env.seq)
    };
    let name = serde_json::to_value(&env.event)
        .ok()
        .and_then(|v| v["type"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "event".to_owned());
    SseEvent::default()
        .id(id)
        .event(name)
        .json_data(env)
        .unwrap_or_else(|_| SseEvent::default().comment("unserialisable event"))
}

fn event_stream(feed: Feed) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    stream::unfold(feed, |mut feed| async move {
        let item = feed.next_item().await?;
        let ev = to_sse(&item, feed.scoped);
        Some((Ok(ev), feed))
    })
}

/// `GET /v1/events`.
pub(crate) async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let scoped_run = q.run_id.map(RunId);
    if let Some(run) = scoped_run {
        state.run_of_user(run)?;
    }
    let last_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cursor = parse_cursor(scoped_run, last_id.as_deref())?;
    // Subscribe before reading the log so nothing falls between the two.
    let rx = state.agent().subscribe();
    let mut feed = Feed {
        rx,
        backlog: VecDeque::new(),
        scoped: cursor.scoped,
        run: cursor.run,
        last: cursor.last,
        data_dir: state.data_dir(),
        cancel: state.shutdown_token(),
        done: false,
        finished_in_log: false,
    };
    feed.reload().await;
    Ok(Sse::new(event_stream(feed)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_forms() {
        let run = RunId::new();
        let c = parse_cursor(Some(run), Some("7")).unwrap();
        assert!(c.scoped && c.last == Some(7) && c.run == Some(run));
        let c = parse_cursor(Some(run), None).unwrap();
        assert!(c.scoped && c.last.is_none());
        let c = parse_cursor(None, Some(&format!("{run}:3"))).unwrap();
        assert!(!c.scoped && c.last == Some(3) && c.run == Some(run));
        assert!(parse_cursor(None, Some("5")).is_err());
        assert!(parse_cursor(Some(run), Some("x")).is_err());
        assert!(parse_cursor(None, None).unwrap().run.is_none());
    }
}

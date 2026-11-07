//! Thin HTTP client of the daemon API used by `bot status|chat|bots|approve`.

use std::fmt;
use std::time::Duration;

use futures_util::StreamExt;
use protocol::{Envelope, Event};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::Result;

/// Process exit codes (stable; see `bot --help`).
pub mod exit {
    /// Any other failure.
    pub const GENERIC: i32 = 1;
    /// The daemon is not reachable.
    pub const UNREACHABLE: i32 = 3;
    /// Missing / wrong token, or a Host / Origin rejection.
    pub const AUTH: i32 = 4;
    /// The requested object does not exist.
    pub const NOT_FOUND: i32 = 5;
    /// The daemon rejected the request (invalid input, conflict, ...).
    pub const REJECTED: i32 = 6;
    /// The run ended with an error or was cancelled.
    pub const RUN_FAILED: i32 = 7;
}

/// An error that carries the process exit code.
#[derive(Debug)]
pub struct ExitError {
    /// Exit code.
    pub code: i32,
    /// Message for stderr.
    pub msg: String,
}

impl fmt::Display for ExitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for ExitError {}

pub fn fail(code: i32, msg: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(ExitError {
        code,
        msg: msg.into(),
    })
}

/// A connected client.
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// Client for `base` (e.g. `http://127.0.0.1:47821`) with a bearer token.
    pub fn new(base: &str, token: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            http: reqwest::Client::new(),
        }
    }

    fn unreachable(&self, e: &reqwest::Error) -> Box<dyn std::error::Error> {
        if e.is_connect() || e.is_timeout() {
            fail(
                exit::UNREACHABLE,
                format!(
                    "daemon not reachable at {} (start it with `bot serve`)",
                    self.base
                ),
            )
        } else {
            fail(exit::GENERIC, e.to_string())
        }
    }

    /// Map an error response to an [`ExitError`].
    async fn check(&self, resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let msg = body["error"]["message"]
            .as_str()
            .map_or_else(|| status.to_string(), str::to_owned);
        let code = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => exit::AUTH,
            StatusCode::NOT_FOUND => exit::NOT_FOUND,
            s if s.is_client_error() => exit::REJECTED,
            _ => exit::GENERIC,
        };
        Err(fail(code, format!("{status}: {msg}")))
    }

    /// Send a request, return the JSON body (`Null` for empty bodies).
    pub async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(30));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.map_err(|e| self.unreachable(&e))?;
        let resp = self.check(resp).await?;
        let bytes = resp.bytes().await?;
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// `GET /v1/health` (no token needed).
    pub async fn health(&self) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/v1/health", self.base))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| self.unreachable(&e))?;
        Ok(self.check(resp).await?.json().await?)
    }

    /// Open the daemon-wide event stream (live events from now on).  Open it
    /// *before* sending a message, then hand it to [`Self::follow_tree`], so
    /// no early event is missed.
    ///
    /// # Errors
    /// Daemon errors.
    pub async fn open_events(&self) -> Result<reqwest::Response> {
        let resp = self
            .http
            .get(format!("{}/v1/events", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| self.unreachable(&e))?;
        self.check(resp).await
    }

    /// The outcome of a run that has already ended, from `GET /v1/runs/{id}`
    /// (`None` while it is still running or the lookup fails).  The outer
    /// `Option` is "finished?", the inner one the run's error.
    async fn finished_result(&self, run_id: &str) -> Option<Option<String>> {
        let run = self
            .request(Method::GET, &format!("/v1/runs/{run_id}"), None)
            .await
            .ok()?;
        let error = run["error"].as_str().map(str::to_owned);
        match run["state"].as_str()? {
            "succeeded" => Some(error),
            "failed" => Some(Some(error.unwrap_or_else(|| "run failed".to_owned()))),
            "cancelled" => Some(Some(error.unwrap_or_else(|| "run cancelled".to_owned()))),
            _ => None,
        }
    }

    /// Follow a run **and its subagents** on the global stream `first` (from
    /// [`Self::open_events`]), reconnecting with the daemon-wide
    /// `Last-Event-ID`.  Returns the root run's final error (`None` = success).
    ///
    /// # Errors
    /// The stream cannot be kept alive.
    pub async fn follow_tree(
        &self,
        first: reqwest::Response,
        run_id: &str,
        mut on_event: impl FnMut(&str, &Envelope),
    ) -> Result<Option<String>> {
        let mut tree = RunTree::new(run_id);
        let mut last: Option<u64> = None;
        let mut failures = 0u32;
        let mut resp = Some(first);
        loop {
            let reconnecting = resp.is_none();
            let current = if let Some(r) = resp.take() {
                r
            } else {
                let mut rb = self
                    .http
                    .get(format!("{}/v1/events", self.base))
                    .bearer_auth(&self.token);
                if let Some(l) = last {
                    rb = rb.header("Last-Event-ID", l.to_string());
                }
                match rb.send().await {
                    Ok(r) => self.check(r).await?,
                    Err(e) => return Err(self.unreachable(&e)),
                }
            };
            // Reconnected without a cursor (the first stream ended before it
            // delivered anything): the daemon only sends live events now, so a
            // run that finished in between would never report.  The stream is
            // already open, hence anything after this check arrives live.
            if reconnecting
                && last.is_none()
                && let Some(result) = self.finished_result(run_id).await
            {
                return Ok(result);
            }
            let mut stream = current.bytes_stream();
            let mut buf = SseBuf::default();
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else { break };
                buf.push(&chunk);
                while let Some(block) = buf.next_block() {
                    if let Some(id) = sse_id(&block) {
                        last = Some(id);
                    }
                    let Some(data) = sse_data(&block) else {
                        continue;
                    };
                    let Ok(env) = serde_json::from_str::<Envelope>(&data) else {
                        continue;
                    };
                    if !tree.accepts(&env) {
                        continue;
                    }
                    on_event(&data, &env);
                    if tree.is_root_finished(&env)
                        && let Event::RunFinished(f) = &env.event
                    {
                        return Ok(f.error.clone());
                    }
                }
            }
            failures += 1;
            if failures > 5 {
                return Err(fail(
                    exit::GENERIC,
                    "event stream ended before the run finished",
                ));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    #[cfg(test)]
    /// Follow a run's SSE stream, reconnecting with `Last-Event-ID` when the
    /// connection drops before `run_finished`.  `on_event` gets the raw JSON
    /// data line and the parsed envelope; returns the run's final error
    /// (`None` = success).
    pub async fn follow_run(
        &self,
        run_id: &str,
        mut on_event: impl FnMut(&str, &Envelope),
    ) -> Result<Option<String>> {
        let mut last: Option<u64> = None;
        let mut failures = 0u32;
        loop {
            let mut rb = self
                .http
                .get(format!("{}/v1/events?run_id={run_id}", self.base))
                .bearer_auth(&self.token);
            if let Some(l) = last {
                rb = rb.header("Last-Event-ID", l.to_string());
            }
            let outcome = match rb.send().await {
                Ok(resp) => {
                    let resp = self.check(resp).await?;
                    let mut stream = resp.bytes_stream();
                    let mut buf = SseBuf::default();
                    let mut finished = None;
                    'read: while let Some(chunk) = stream.next().await {
                        let Ok(chunk) = chunk else { break };
                        buf.push(&chunk);
                        while let Some(block) = buf.next_block() {
                            let Some(data) = sse_data(&block) else {
                                continue;
                            };
                            let Ok(env) = serde_json::from_str::<Envelope>(&data) else {
                                continue;
                            };
                            last = Some(env.seq);
                            on_event(&data, &env);
                            if let Event::RunFinished(f) = &env.event {
                                finished = Some(f.error.clone());
                                break 'read;
                            }
                        }
                    }
                    finished
                }
                Err(e) => return Err(self.unreachable(&e)),
            };
            if let Some(result) = outcome {
                return Ok(result);
            }
            failures += 1;
            if failures > 5 {
                return Err(fail(
                    exit::GENERIC,
                    "event stream ended before the run finished",
                ));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

/// The runs of one request: the root run and every subagent below it.
///
/// The global event stream carries all runs; this picks the ones a `bot chat`
/// call must show (so an approval raised inside a subagent is not missed).
#[derive(Debug)]
pub struct RunTree {
    root: String,
    ids: std::collections::HashSet<String>,
}

impl RunTree {
    /// A tree with only `root`.
    #[must_use]
    pub fn new(root: &str) -> Self {
        Self {
            root: root.to_owned(),
            ids: std::iter::once(root.to_owned()).collect(),
        }
    }

    /// Whether `env` belongs to the tree (remembering new children).
    pub fn accepts(&mut self, env: &Envelope) -> bool {
        let payload = serde_json::to_value(&env.event).unwrap_or(Value::Null);
        let run = payload["run_id"].as_str();
        let parent = env
            .parent_run_id
            .map(|p| p.to_string())
            .or_else(|| payload["parent_run_id"].as_str().map(str::to_owned));
        if run.is_some_and(|r| self.ids.contains(r)) {
            return true;
        }
        match (parent, run) {
            (Some(p), Some(r)) if self.ids.contains(&p) => {
                self.ids.insert(r.to_owned());
                true
            }
            // Child events without a payload run id (workflow events).
            (Some(p), None) => self.ids.contains(&p),
            _ => false,
        }
    }

    /// Whether `env` is the end of the root run.
    #[must_use]
    pub fn is_root_finished(&self, env: &Envelope) -> bool {
        matches!(&env.event, Event::RunFinished(f) if f.run_id.to_string() == self.root)
    }
}

/// Receive buffer of an SSE stream.  It holds *bytes*: a network chunk may end
/// in the middle of a multi-byte character, so text is only decoded once a
/// whole block (terminated by a blank line, `\n` or `\r\n`) is there.
#[derive(Debug, Default)]
pub struct SseBuf {
    buf: Vec<u8>,
}

impl SseBuf {
    /// Append a network chunk.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// The next complete block (line endings normalised to `\n`, ending in a
    /// blank line), if one is buffered.
    pub fn next_block(&mut self) -> Option<String> {
        let mut line_start = 0;
        while let Some(rel) = self.buf[line_start..].iter().position(|b| *b == b'\n') {
            let end = line_start + rel;
            let line = &self.buf[line_start..end];
            let blank = line.is_empty() || line == b"\r";
            if blank && line_start == 0 {
                // Stray blank line between blocks.
                self.buf.drain(..=end);
                continue;
            }
            if blank {
                let raw: Vec<u8> = self.buf.drain(..=end).collect();
                return Some(String::from_utf8_lossy(&raw).replace("\r\n", "\n"));
            }
            line_start = end + 1;
        }
        None
    }
}

/// The `id:` of an SSE block.
fn sse_id(block: &str) -> Option<u64> {
    block
        .lines()
        .find_map(|l| l.strip_prefix("id:"))
        .and_then(|v| v.trim().parse().ok())
}

/// The joined `data:` lines of one SSE block.
pub fn sse_data(block: &str) -> Option<String> {
    let data: Vec<&str> = block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| d.strip_prefix(' ').unwrap_or(d))
        .collect();
    (!data.is_empty()).then(|| data.join("\n"))
}

/// Body of `POST /v1/approvals/{id}`.
pub fn decision_body(deny: bool, scope: &str, reason: Option<&str>) -> Value {
    if deny {
        json!({"decision": "deny", "reason": reason})
    } else {
        json!({"decision": "approve", "scope": scope})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_blocks() {
        let b = "id: 3\nevent: message_delta\ndata: {\"a\":1}\n\n";
        assert_eq!(sse_data(b).as_deref(), Some("{\"a\":1}"));
        assert_eq!(sse_data(": keep-alive\n\n"), None);
        assert_eq!(sse_data("data: a\ndata: b\n\n").as_deref(), Some("a\nb"));
    }

    fn env(event: Event, parent: Option<protocol::RunId>) -> Envelope {
        Envelope::new(0, event).with_parent(parent)
    }

    #[test]
    fn run_tree_follows_children_and_grandchildren_only() {
        use protocol::RunId;
        use protocol::event::{MessageDelta, RunFinished, SubagentStarted};
        let (root, child, grand, other) = (RunId::new(), RunId::new(), RunId::new(), RunId::new());
        let mut t = RunTree::new(&root.to_string());
        let delta = |r| {
            Event::MessageDelta(MessageDelta {
                run_id: r,
                text: "x".into(),
            })
        };
        assert!(t.accepts(&env(delta(root), None)));
        assert!(!t.accepts(&env(delta(other), None)));
        // The child announces itself on the parent stream, then speaks with `parent_run_id`.
        let started = Event::SubagentStarted(SubagentStarted {
            run_id: child,
            parent_run_id: root,
            task: "t".into(),
            call_id: None,
        });
        assert!(t.accepts(&env(started, None)));
        assert!(t.accepts(&env(delta(child), Some(root))));
        assert!(t.accepts(&env(delta(grand), Some(child))));
        assert!(t.accepts(&env(delta(grand), Some(child))));
        assert!(!t.accepts(&env(delta(other), Some(other))));
        let fin = |r| {
            Event::RunFinished(RunFinished {
                run_id: r,
                error: None,
            })
        };
        assert!(!t.is_root_finished(&env(fin(child), Some(root))));
        assert!(t.is_root_finished(&env(fin(root), None)));
    }

    #[test]
    fn sse_buffer_keeps_multibyte_characters_split_across_chunks() {
        let block = "id: 1\ndata: {\"t\":\"şğ😀\"}\n\n";
        let bytes = block.as_bytes();
        // Every possible split point, including inside each character.
        for cut in 0..=bytes.len() {
            let mut b = SseBuf::default();
            b.push(&bytes[..cut]);
            let early = b.next_block();
            b.push(&bytes[cut..]);
            let got = early.or_else(|| b.next_block()).unwrap();
            assert_eq!(got, block, "cut at {cut}");
            assert_eq!(b.next_block(), None);
        }
    }

    #[test]
    fn sse_buffer_handles_crlf_and_several_blocks() {
        let mut b = SseBuf::default();
        b.push(b"id: 1\r\ndata: a\r\n\r\nid: 2\r\ndata: b\r\n\r\nid: 3\r\nda");
        assert_eq!(b.next_block().as_deref(), Some("id: 1\ndata: a\n\n"));
        assert_eq!(b.next_block().as_deref(), Some("id: 2\ndata: b\n\n"));
        assert_eq!(b.next_block(), None, "the third block is incomplete");
        b.push(b"ta: c\n\n");
        assert_eq!(b.next_block().as_deref(), Some("id: 3\ndata: c\n\n"));
    }

    #[tokio::test]
    async fn follow_tree_polls_the_run_when_it_reconnects_without_a_cursor() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let run = "11111111-1111-1111-1111-111111111111";
        let server = MockServer::start().await;
        // Every event stream ends at once, without a single event.
        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                ": keep-alive

",
            ))
            .mount(&server)
            .await;
        // The run finished while no stream was connected.
        Mock::given(method("GET"))
            .and(path(format!("/v1/runs/{run}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": run, "state": "failed", "error": "boom"})),
            )
            .mount(&server)
            .await;
        let c = Client::new(&server.uri(), "t".into());
        let first = c.open_events().await.unwrap();
        let got =
            tokio::time::timeout(Duration::from_secs(5), c.follow_tree(first, run, |_, _| {}))
                .await
                .expect("must not hang or retry forever")
                .unwrap();
        assert_eq!(got.as_deref(), Some("boom"));
    }

    #[test]
    fn sse_ids_are_parsed() {
        assert_eq!(sse_id("id: 42\nevent: x\ndata: {}\n\n"), Some(42));
        assert_eq!(sse_id("data: {}\n\n"), None);
    }

    #[test]
    fn decision_bodies() {
        assert_eq!(
            decision_body(false, "always", None),
            json!({"decision": "approve", "scope": "always"})
        );
        assert_eq!(
            decision_body(true, "once", Some("no")),
            json!({"decision": "deny", "reason": "no"})
        );
    }
}

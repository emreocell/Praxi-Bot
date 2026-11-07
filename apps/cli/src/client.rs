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
                    let mut buf = String::new();
                    let mut finished = None;
                    'read: while let Some(chunk) = stream.next().await {
                        let Ok(chunk) = chunk else { break };
                        buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
                        while let Some(i) = buf.find("\n\n") {
                            let block: String = buf.drain(..i + 2).collect();
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

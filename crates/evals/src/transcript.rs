//! In-memory provider capture, privacy filtering and strict offline replay.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use parking_lot::Mutex;
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ProviderError, ToolCall, Usage,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Result, scenario::Scenario};

/// Recording is an explicit local-owner operation and is never allowed in CI.
///
/// # Errors
/// Missing opt-in or any set CI variable.
pub fn recording_allowed(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<()> {
    if env("CI").is_some() {
        return Err("recording is forbidden when CI is set".into());
    }
    if env("BOT_EVAL_RECORD").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Err("owner-machine recording requires BOT_EVAL_RECORD=1".into());
    }
    Ok(())
}

/// Portable exchange stored inside a scenario; no raw system prompts or credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    /// Format version.
    pub version: u32,
    /// True when captured from a scripted provider rather than a real model.
    pub synthetic: bool,
    /// Original provider/model reference, for provenance only; ignored during replay.
    pub source_model: String,
    /// Calls in invocation order.
    pub calls: Vec<Exchange>,
}

/// One provider invocation, including stream boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exchange {
    /// Request structure and tool results as the loop supplied them.
    pub request: Value,
    /// Ordered streaming chunks.
    pub chunks: Vec<Chunk>,
}

/// Serializable counterpart of the provider's public chunk contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chunk {
    /// Partial assistant text.
    Text(String),
    /// Partial tool arguments.
    ToolDelta {
        call_id: String,
        name: Option<String>,
        input_delta: String,
    },
    /// Assembled tool invocation.
    Tool(ToolCall),
    /// Provider usage.
    Usage(Usage),
    /// Stream terminator.
    Done(FinishReason),
}

impl From<ChatChunk> for Chunk {
    fn from(c: ChatChunk) -> Self {
        match c {
            ChatChunk::TextDelta(t) => Self::Text(t),
            ChatChunk::ToolCallDelta {
                call_id,
                name,
                input_delta,
            } => Self::ToolDelta {
                call_id,
                name,
                input_delta,
            },
            ChatChunk::ToolCall(t) => Self::Tool(t),
            ChatChunk::Usage(u) => Self::Usage(u),
            ChatChunk::Done { finish_reason } => Self::Done(finish_reason),
        }
    }
}

impl From<Chunk> for ChatChunk {
    fn from(c: Chunk) -> Self {
        match c {
            Chunk::Text(t) => Self::TextDelta(t),
            Chunk::ToolDelta {
                call_id,
                name,
                input_delta,
            } => Self::ToolCallDelta {
                call_id,
                name,
                input_delta,
            },
            Chunk::Tool(t) => Self::ToolCall(t),
            Chunk::Usage(u) => Self::Usage(u),
            Chunk::Done(finish_reason) => Self::Done { finish_reason },
        }
    }
}

impl Transcript {
    /// Reject unknown versions, missing calls and incomplete streams.
    ///
    /// # Errors
    /// A truncated or unsupported transcript.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 || self.calls.is_empty() {
            return Err("transcript: unsupported version or no model calls".into());
        }
        for (i, c) in self.calls.iter().enumerate() {
            if !["roles", "tools", "tool_results"]
                .iter()
                .all(|key| c.request[key].is_array())
            {
                return Err(format!("transcript call {i}: invalid request summary").into());
            }
            let mut checked = c.request.clone();
            fingerprint(&mut checked);
            if checked != c.request {
                return Err(format!("transcript call {i}: request fingerprint mismatch").into());
            }
            if !matches!(c.chunks.last(), Some(Chunk::Done(_)))
                || c.chunks
                    .iter()
                    .filter(|c| matches!(c, Chunk::Done(_)))
                    .count()
                    != 1
            {
                return Err(format!(
                    "transcript call {i}: truncated stream (expected one final done chunk)"
                )
                .into());
            }
        }
        Ok(())
    }
}

/// Redacts registered private values, credentials, emails and home paths before writing.
pub struct Privacy {
    redactor: secret_store::Redactor,
    patterns: Vec<Regex>,
}

impl Default for Privacy {
    fn default() -> Self {
        Self::new(secret_store::Redactor::new())
    }
}

impl Privacy {
    /// Include the owner's vault/environment redactor and extra private values.
    ///
    /// # Panics
    /// Only if a built-in privacy regular expression is invalid.
    #[must_use]
    pub fn new(redactor: secret_store::Redactor) -> Self {
        let mut privacy = Self {
            redactor,
            patterns: [
                r"(?i)\bBearer\s+[^\s\x22'<>]+",
                r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
                r"(?i)(?:[a-z]:[/\\]+users[/\\]+|/(?:home|Users)/)[^\r\n\x22'<>]+",
                r"(?is)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            ]
            .into_iter()
            .map(|p| Regex::new(p).expect("privacy regex"))
            .collect(),
        };
        for key in ["USERPROFILE", "HOME"] {
            if let Some(home) = std::env::var_os(key) {
                let home = home.to_string_lossy();
                if home.len() > 3 {
                    for form in [home.to_string(), home.replace('\\', "/")] {
                        let pattern = format!(r"(?i){}[^\r\n\x22'<>]*", regex::escape(&form));
                        privacy
                            .patterns
                            .push(Regex::new(&pattern).expect("escaped home path"));
                    }
                }
            }
        }
        privacy
    }

    /// Register a private value, including encoded variants handled by the project redactor.
    pub fn register(&mut self, value: &str) {
        for v in secret_store::secret_variants(value) {
            self.redactor.add_secret(&v);
        }
    }

    fn text(&self, text: &str) -> String {
        let mut out = self.redactor.redact(text);
        // Detect the additional credential classes supported by secret-store.
        let hits = secret_store::detect_secrets(&out);
        for hit in hits.iter().rev() {
            out.replace_range(hit.span(), "[REDACTED]");
        }
        for p in &self.patterns {
            out = p.replace_all(&out, "[REDACTED]").into_owned();
        }
        out
    }

    fn value(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                // Tool arguments and tool outputs often contain embedded JSON.
                if let Ok(mut nested) = serde_json::from_str::<Value>(s) {
                    self.value(&mut nested);
                    *s = nested.to_string();
                } else {
                    *s = self.text(s);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|v| self.value(v)),
            Value::Object(o) => {
                *o = std::mem::take(o)
                    .into_iter()
                    .map(|(k, mut v)| {
                        self.value(&mut v);
                        (self.text(&k), v)
                    })
                    .collect();
            }
            _ => {}
        }
    }

    /// Serialize only sanitized data; raw captures never touch disk. Refuse overwrites.
    ///
    /// # Errors
    /// Invalid capture, privacy lint, serialization or file I/O failure.
    pub fn write(&self, scenario: &Scenario, path: &Path) -> Result<()> {
        let mut safe = scenario.clone();
        let t = safe.transcript.as_mut().ok_or("no transcript to save")?;
        t.validate()?;
        // A secret may span multiple streaming deltas. If the assembled text needs
        // masking, discard those fragment payloads and put sanitized text in the first.
        for call in &mut t.calls {
            let joined: String = call
                .chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            let clean = self.text(&joined);
            if clean != joined {
                let mut first = true;
                for c in &mut call.chunks {
                    if let Chunk::Text(t) = c {
                        *t = if first { clean.clone() } else { String::new() };
                        first = false;
                    }
                }
            }
            let mut inputs = std::collections::BTreeMap::<String, String>::new();
            for c in &call.chunks {
                if let Chunk::ToolDelta {
                    call_id,
                    input_delta,
                    ..
                } = c
                {
                    inputs
                        .entry(call_id.clone())
                        .or_default()
                        .push_str(input_delta);
                }
            }
            let private: Vec<_> = inputs
                .into_iter()
                .filter_map(|(id, input)| {
                    let mut v = Value::String(input.clone());
                    self.value(&mut v);
                    (v != Value::String(input)).then_some(id)
                })
                .collect();
            for c in &mut call.chunks {
                if let Chunk::ToolDelta {
                    call_id,
                    input_delta,
                    ..
                } = c
                    && private.contains(call_id)
                {
                    input_delta.clear();
                }
            }
            // Partial arguments can split a credential too; the complete call keeps
            // sanitized arguments. Opaque provider signatures have no offline use.
            for c in &mut call.chunks {
                if let Chunk::Tool(t) = c {
                    t.provider_extra = None;
                }
            }
        }
        let mut value = serde_json::to_value(&safe)?;
        self.value(&mut value);
        let mut safe: Scenario = serde_json::from_value(value)?;
        safe.secrets.clear();
        safe.redact_secrets = false;
        if let Some(t) = &mut safe.transcript {
            for call in &mut t.calls {
                fingerprint(&mut call.request);
            }
        }
        let yaml = serde_yaml::to_string(&safe)?;
        // Lint the decoded tree as well as its escaped serialized representation.
        let mut lint = serde_json::to_value(&safe)?;
        let before = lint.clone();
        self.value(&mut lint);
        if lint != before || self.text(&yaml) != yaml {
            return Err("recording rejected by privacy lint".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(yaml.as_bytes())?;
        Ok(())
    }
}

fn summary(req: &ChatRequest) -> Value {
    let messages: Vec<_> = req
        .messages
        .iter()
        .filter(|m| {
            m.role != MessageRole::System
                && !m
                    .content
                    .as_str()
                    .is_some_and(|s| s.starts_with("[action ledger"))
        })
        .collect();
    // Fingerprint only a portable summary: no model, random runtime ids or system prompt.
    let roles: Vec<_> = messages.iter().map(|m| &m.role).collect();
    let results: Vec<_> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| &m.content)
        .collect();
    let mut tools: Vec<_> = req
        .tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    tools.sort_unstable();
    let mut summary = json!({"roles": roles, "tools": tools, "tool_results": results});
    fingerprint(&mut summary);
    summary
}

// Stable FNV-1a checksum of the *redacted summary*, not a prompt/security hash.
fn fingerprint(summary: &mut Value) {
    if let Some(o) = summary.as_object_mut() {
        o.remove("fingerprint");
    }
    let hash = summary
        .to_string()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    summary["fingerprint"] = json!(format!("{hash:016x}"));
}

fn summary_diff(expected: &Value, actual: &Value) -> String {
    let mut differences = Vec::new();
    for key in ["roles", "tools", "tool_results"] {
        if expected[key] != actual[key] {
            let want = expected[key].as_array().map_or(0, Vec::len);
            let got = actual[key].as_array().map_or(0, Vec::len);
            differences.push(format!("{key} differ (expected {want} entries, got {got})"));
        }
    }
    differences.join("; ")
}

/// Transparent provider wrapper; captures in memory until the privacy pass writes it.
pub struct RecordingProvider {
    inner: Arc<dyn ChatProvider>,
    calls: Arc<Mutex<Vec<Exchange>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl RecordingProvider {
    /// Wrap an arbitrary provider without changing its identity or stream.
    #[must_use]
    pub fn new(inner: Arc<dyn ChatProvider>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            calls: Arc::default(),
            requests: Mutex::default(),
        })
    }

    /// Requests used by the existing harness assertions.
    #[must_use]
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().clone()
    }

    /// Snapshot; incomplete streams will be rejected on save.
    #[must_use]
    pub fn transcript(&self, synthetic: bool) -> Transcript {
        Transcript {
            version: 1,
            synthetic,
            source_model: self
                .requests
                .lock()
                .first()
                .map_or_else(String::new, |r| r.model.0.clone()),
            calls: self.calls.lock().clone(),
        }
    }
}

#[async_trait]
impl ChatProvider for RecordingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> std::result::Result<
        BoxStream<'static, std::result::Result<ChatChunk, ProviderError>>,
        ProviderError,
    > {
        let index = {
            let mut calls = self.calls.lock();
            let i = calls.len();
            calls.push(Exchange {
                request: summary(&req),
                chunks: Vec::new(),
            });
            i
        };
        self.requests.lock().push(req.clone());
        let stream = self.inner.stream(req).await?;
        let calls = Arc::clone(&self.calls);
        Ok(stream
            .map(move |item| {
                if let Ok(c) = &item {
                    calls.lock()[index].chunks.push(c.clone().into());
                }
                item
            })
            .boxed())
    }
}

/// Strict, network-free playback. All permission and taint decisions stay in the loop.
pub struct ReplayProvider {
    transcript: Transcript,
    cursor: Mutex<usize>,
    failures: Mutex<Vec<String>>,
}

impl ReplayProvider {
    /// Validate before any replay is attempted.
    ///
    /// # Errors
    /// Invalid/truncated transcript.
    pub fn new(transcript: Transcript) -> Result<Arc<Self>> {
        transcript.validate()?;
        Ok(Arc::new(Self {
            transcript,
            cursor: Mutex::new(0),
            failures: Mutex::default(),
        }))
    }

    /// Includes unused calls and divergences even when the loop handled the provider error.
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        let mut f = self.failures.lock().clone();
        let used = *self.cursor.lock();
        if used != self.transcript.calls.len() {
            f.push(format!(
                "transcript: consumed {used} of {} recorded model calls",
                self.transcript.calls.len()
            ));
        }
        f
    }
}

#[async_trait]
impl ChatProvider for ReplayProvider {
    fn id(&self) -> &'static str {
        "fake"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> std::result::Result<
        BoxStream<'static, std::result::Result<ChatChunk, ProviderError>>,
        ProviderError,
    > {
        let mut index = self.cursor.lock();
        let at = *index;
        *index += 1;
        let got = summary(&req);
        let error = match self.transcript.calls.get(at) {
            None => Some(format!(
                "transcript call {at}: unexpected model call; recording exhausted"
            )),
            Some(c) if c.request != got => Some(format!(
                "transcript call {at}: request/tool results diverged: {}",
                summary_diff(&c.request, &got)
            )),
            Some(_) => None,
        };
        if let Some(e) = error {
            self.failures.lock().push(e.clone());
            return Err(ProviderError::InvalidRequest(e));
        }
        let chunks = self.transcript.calls[at].chunks.clone();
        Ok(futures::stream::iter(chunks.into_iter().map(|c| Ok(c.into()))).boxed())
    }
}

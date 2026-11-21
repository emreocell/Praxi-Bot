//! Bounded, advisory decisions. An unanswered question leaves the caller's
//! deterministic path in charge.

mod backends;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::time::Instant;

pub use backends::{ChatJsonBackend, DecisionsApi, NativeDecisions};

/// Default and maximum elapsed time for a decision, including all fallbacks.
pub const MAX_TIMEOUT: Duration = Duration::from_millis(1_500);
const MAX_INPUT_BYTES: usize = 6 * 1024;
const MAX_QUESTIONS: usize = 32;
const MAX_METADATA_BYTES: usize = 32 * 1024;

/// Trusted, host-authored metadata about the decision. Values in a choice map
/// describe its ids; only the ids can appear in a validated answer.
#[derive(Clone, Debug)]
pub enum Question {
    YesNo {
        instructions: String,
    },
    Choice {
        instructions: String,
        options: BTreeMap<String, String>,
    },
}

pub type Questions = BTreeMap<String, Question>;

/// User text and trusted question metadata. Deliberately omits history,
/// attachments, and tool results.
pub struct DecisionRequest {
    input: String,
    questions: Questions,
}

impl DecisionRequest {
    #[must_use]
    pub fn new(mut user_text: String, questions: Questions) -> Self {
        let mut end = user_text.len().min(MAX_INPUT_BYTES);
        while !user_text.is_char_boundary(end) {
            end -= 1;
        }
        user_text.truncate(end);
        Self {
            input: user_text,
            questions,
        }
    }

    #[must_use]
    pub fn user_text(&self) -> &str {
        &self.input
    }

    #[must_use]
    pub fn questions(&self) -> &Questions {
        &self.questions
    }

    fn valid(&self) -> bool {
        if !(1..=MAX_QUESTIONS).contains(&self.questions.len()) {
            return false;
        }
        let mut bytes = 0usize;
        for (key, question) in &self.questions {
            if !valid_id(key) {
                return false;
            }
            let instructions = match question {
                Question::YesNo { instructions } => instructions,
                Question::Choice {
                    instructions,
                    options,
                } => {
                    if !(2..=255).contains(&options.len()) || !options.keys().all(|id| valid_id(id))
                    {
                        return false;
                    }
                    for (id, description) in options {
                        bytes = bytes
                            .saturating_add(id.len())
                            .saturating_add(description.len());
                    }
                    instructions
                }
            };
            if instructions.trim().is_empty() {
                return false;
            }
            bytes = bytes
                .saturating_add(key.len())
                .saturating_add(instructions.len());
        }
        bytes <= MAX_METADATA_BYTES
    }
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Only values from the caller's answer space leave the pool.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    YesNo { yes: f64 },
    Choice { id: String, confidence: f64 },
}

pub type Answers = BTreeMap<String, Answer>;

fn probability(raw: &Value) -> Option<f64> {
    raw.as_f64()
        .filter(|p| p.is_finite())
        .map(|p| p.clamp(0.0, 1.0))
}

fn validate(questions: &Questions, raw: &Value) -> Answers {
    questions
        .iter()
        .filter_map(|(key, question)| {
            let answer = raw.get(key)?;
            if answer.get("refusal").is_some_and(|v| !v.is_null()) {
                return None;
            }
            let value = match (question, answer.get("type")?.as_str()?) {
                (Question::YesNo { .. }, "noul") => Answer::YesNo {
                    yes: probability(&answer["noul"])?,
                },
                (Question::Choice { options, .. }, "choice") => {
                    let (id, _) = options.get_key_value(answer["choice"].as_str()?)?;
                    Answer::Choice {
                        id: id.clone(),
                        confidence: probability(&answer["confidence"])?,
                    }
                }
                _ => return None,
            };
            Some((key.clone(), value))
        })
        .collect()
}

/// A provider's reported counts. Absence means usage was unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cached_input: Option<u64>,
}

/// Untrusted adapter response. The pool validates every answer it returns.
pub struct BackendReply {
    pub answers: Value,
    pub usage: Option<TokenUsage>,
}

/// Errors deliberately carry no body, URL, credential, or prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("decision provider rate limited the request")]
    RateLimited,
    #[error("decision provider returned HTTP {0}")]
    Http(u16),
    #[error("decision provider transport failed")]
    Transport,
    #[error("invalid decision response")]
    BadResponse,
    #[error("decision dispatch unavailable")]
    Unavailable,
}

#[async_trait]
pub trait DecisionBackend: Send + Sync {
    async fn decide(&self, request: &DecisionRequest) -> Result<BackendReply, BackendError>;
}

/// Share one budget across all questions asked during a turn. Every provider
/// attempt, including a failed or timed-out attempt, costs one call.
#[derive(Debug)]
pub struct TurnBudget(Mutex<u32>);

impl TurnBudget {
    #[must_use]
    pub const fn new(calls: u32) -> Self {
        Self(Mutex::new(calls))
    }

    #[must_use]
    pub fn remaining(&self) -> u32 {
        *self.0.lock()
    }

    fn take(&self) -> bool {
        let mut remaining = self.0.lock();
        let Some(next) = remaining.checked_sub(1) else {
            return false;
        };
        *remaining = next;
        true
    }
}

#[derive(Default)]
struct RateWindow {
    started: VecDeque<Instant>,
    cooldown_until: Option<Instant>,
}

/// One backend and its shared rolling requests-per-minute limit.
pub struct Backend {
    inner: Box<dyn DecisionBackend>,
    rpm: NonZeroU32,
    window: Mutex<RateWindow>,
}

impl Backend {
    #[must_use]
    pub fn new(inner: Box<dyn DecisionBackend>, rpm: NonZeroU32) -> Self {
        Self {
            inner,
            rpm,
            window: Mutex::default(),
        }
    }

    fn try_start(&self, budget: &TurnBudget) -> bool {
        let mut window = self.window.lock();
        let now = Instant::now();
        while window
            .started
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            window.started.pop_front();
        }
        if window.cooldown_until.is_some_and(|until| now < until)
            || window.started.len() >= self.rpm.get() as usize
            || !budget.take()
        {
            return false;
        }
        window.started.push_back(now);
        true
    }
}

/// An ordered chain under one deadline.
pub struct Decider {
    backends: Vec<Backend>,
    timeout: Duration,
}

impl fmt::Debug for Decider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decider")
            .field("backend_count", &self.backends.len())
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Decider {
    #[must_use]
    pub fn new(backends: Vec<Backend>, timeout: Duration) -> Self {
        Self {
            backends,
            timeout: timeout.min(MAX_TIMEOUT),
        }
    }

    /// Try backends in order. The first usable reply wins. Missing answers,
    /// timeout, and exhaustion all leave the deterministic fallback available.
    pub async fn decide(&self, request: &DecisionRequest, budget: &TurnBudget) -> Answers {
        if self.timeout.is_zero() || !request.valid() {
            return Answers::new();
        }
        tokio::time::timeout(self.timeout, self.try_chain(request, budget))
            .await
            .unwrap_or_default()
    }

    async fn try_chain(&self, request: &DecisionRequest, budget: &TurnBudget) -> Answers {
        for backend in &self.backends {
            if budget.remaining() == 0 {
                break;
            }
            if !backend.try_start(budget) {
                continue;
            }
            match backend.inner.decide(request).await {
                Ok(reply) => {
                    let answers = validate(request.questions(), &reply.answers);
                    if !answers.is_empty() {
                        return answers;
                    }
                }
                Err(BackendError::RateLimited) => {
                    backend.window.lock().cooldown_until =
                        Some(Instant::now() + Duration::from_secs(60));
                }
                Err(_) => {}
            }
        }
        Answers::new()
    }
}

//! Host adapter for the optional decision pool. The existing K0–K3 ladder
//! and first-turn model selection currently run without consulting it.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_router::decider::{
    Answers, Backend, BackendError, BackendReply, ChatJsonBackend, Decider, DecisionBackend,
    DecisionRequest, DecisionsApi, MAX_TIMEOUT, NativeDecisions, Question, Questions, TurnBudget,
};
use agent_router::ladder::{BackendError as ClassifierError, Classifier};
use async_trait::async_trait;
use provider_catalog::{Catalog, ChatProvider, ModelRef, ProviderConfig, ProviderKind, Usage};
use secret_store::SecretStore;
use serde::Deserialize;

use crate::exec::TurnExecutor;
use crate::shared::{Providers, provider_key};
use crate::usage::{CallPurpose, Scope};

pub const FILE: &str = "decider.toml";
const MAX_BACKENDS: usize = 8;

/// The transport to use for a configured model.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    #[default]
    Chat,
    GeminiDecisions,
    #[serde(rename = "openrouter_decisions")]
    OpenRouterDecisions,
}

impl Api {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::GeminiDecisions => "gemini_decisions",
            Self::OpenRouterDecisions => "openrouter_decisions",
        }
    }

    fn native(self) -> Option<DecisionsApi> {
        match self {
            Self::Chat => None,
            Self::GeminiDecisions => Some(DecisionsApi::Gemini),
            Self::OpenRouterDecisions => Some(DecisionsApi::OpenRouter),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendSettings {
    #[serde(default)]
    pub api: Api,
    /// A full model reference from an enabled provider.
    pub model: String,
    pub rpm: NonZeroU32,
}

/// Optional daemon-wide decision-pool configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub timeout_ms: u64,
    pub max_calls_per_turn: u32,
    pub backends: Vec<BackendSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_ms: 1_500,
            max_calls_per_turn: 2,
            backends: Vec::new(),
        }
    }
}

impl Settings {
    /// Missing configuration disables the pool.
    ///
    /// # Errors
    /// Returns a configuration error without echoing the contents of the file.
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(data_dir.join(FILE)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(_) => return Err(format!("could not read {FILE}")),
        };
        let settings: Self = toml::from_str(&text).map_err(|error| {
            let location = error
                .span()
                .and_then(|span| text.get(..span.start))
                .map_or(1, |prefix| prefix.split('\n').count());
            format!("invalid {FILE} at line {location}")
        })?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<(), String> {
        if self.timeout_ms == 0 || Duration::from_millis(self.timeout_ms) > MAX_TIMEOUT {
            return Err("timeout_ms must be between 1 and 1500".into());
        }
        if !(1..=8).contains(&self.max_calls_per_turn) {
            return Err("max_calls_per_turn must be between 1 and 8".into());
        }
        if self.backends.len() > MAX_BACKENDS || (self.enabled && self.backends.is_empty()) {
            return Err("the chain accepts up to 8 backends and requires one when enabled".into());
        }
        let mut seen = HashSet::new();
        for (i, backend) in self.backends.iter().enumerate() {
            let bad = |msg| format!("decider backend {} {msg}", i + 1);
            let (provider, model) = backend
                .model
                .split_once('/')
                .ok_or_else(|| bad("requires provider/model"))?;
            if provider.is_empty()
                || model.is_empty()
                || backend.model.len() > 256
                || !provider
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || !model.split('/').all(|part| {
                    !part.is_empty()
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._:-@".contains(&b))
                })
            {
                return Err(bad("requires a valid HTTP provider/model reference"));
            }
            if !match backend.api {
                Api::Chat => true,
                Api::GeminiDecisions => provider == "gemini",
                Api::OpenRouterDecisions => provider == "openrouter",
            } {
                return Err(bad(
                    "requires the matching built-in provider for its native API",
                ));
            }
            if !seen.insert(backend.model.as_str()) {
                return Err(bad("duplicates a model already in the chain"));
            }
            if backend.rpm.get() > 10_000 {
                return Err(bad("requires rpm between 1 and 10000"));
            }
        }
        Ok(())
    }
}

/// A host-owned pool that can only dispatch in an active human turn's ledger
/// scope. Callers supply trusted question metadata, and reuse one budget per turn.
#[derive(Debug)]
pub struct TurnDecider {
    pool: Decider,
    max_calls: u32,
}

impl TurnDecider {
    #[must_use]
    pub fn budget(&self) -> TurnBudget {
        TurnBudget::new(self.max_calls)
    }

    /// Ask about the original user message. Missing context, redaction, or
    /// usable answers leaves the deterministic path available.
    pub async fn decide(&self, mut questions: Questions, budget: &TurnBudget) -> Answers {
        let Some(scope) = Scope::current() else {
            return Answers::new();
        };
        let Some(redactor) = &scope.sh.cfg.redactor else {
            return Answers::new();
        };
        if scope.ctx.initiator != protocol::InitiatorKind::Human
            || scope.ctx.parent_run_id.is_some()
        {
            return Answers::new();
        }
        let Some(run) = scope.sh.storage.get_run(scope.ctx.run_id).ok().flatten() else {
            return Answers::new();
        };
        let executor = TurnExecutor {
            sh: Arc::clone(&scope.sh),
        };
        let Ok(user_text) = executor.first_user_text(&scope.ctx, &run) else {
            return Answers::new();
        };
        for (id, question) in &mut questions {
            if redactor.contains_secret(id) {
                return Answers::new();
            }
            match question {
                Question::YesNo { instructions } => *instructions = redactor.redact(instructions),
                Question::Choice {
                    instructions,
                    options,
                } => {
                    *instructions = redactor.redact(instructions);
                    for (id, description) in options {
                        if redactor.contains_secret(id) {
                            return Answers::new();
                        }
                        *description = redactor.redact(description);
                    }
                }
            }
        }
        let request = DecisionRequest::new(redactor.redact(&user_text), questions);
        self.pool.decide(&request, budget).await
    }
}

pub(crate) fn build(
    data_dir: &Path,
    providers: &Providers,
    config: &ProviderConfig,
    catalog: &Catalog,
    store: &dyn SecretStore,
    warn: &mut dyn FnMut(&str),
) -> Option<Arc<TurnDecider>> {
    let settings = match Settings::load(data_dir) {
        Ok(settings) if settings.enabled => settings,
        Ok(_) => return None,
        Err(error) => {
            warn(&format!("decider disabled: {error}"));
            return None;
        }
    };
    let backends: Vec<_> = settings
        .backends
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let backend = build_backend(entry, providers, config, catalog, store);
            if backend.is_none() {
                warn(&format!(
                    "decider backend {} unavailable; check its provider setup",
                    index + 1
                ));
            }
            backend.map(|backend| Backend::new(backend, entry.rpm))
        })
        .collect();
    if backends.is_empty() {
        return None;
    }
    Some(Arc::new(TurnDecider {
        pool: Decider::new(backends, Duration::from_millis(settings.timeout_ms)),
        max_calls: settings.max_calls_per_turn,
    }))
}

fn build_backend(
    entry: &BackendSettings,
    providers: &Providers,
    config: &ProviderConfig,
    catalog: &Catalog,
    store: &dyn SecretStore,
) -> Option<Box<dyn DecisionBackend>> {
    let (id, model) = entry.model.split_once('/')?;
    let settings = config.get(id).filter(|s| s.enabled && s.cli.is_none())?;
    let Some(api) = entry.api.native() else {
        return Some(Box::new(ChatJsonBackend(Arc::new(DeciderClassifier {
            model: ModelRef(entry.model.clone()),
            provider: providers.get(id)?,
        }))));
    };
    let provider = catalog.get(id)?;
    let expected_kind = match api {
        DecisionsApi::Gemini => ProviderKind::Gemini,
        DecisionsApi::OpenRouter => ProviderKind::OpenAiCompatible,
    };
    if settings.custom.is_some()
        || provider.kind != expected_kind
        || provider.base_url.trim_end_matches('/') != api.catalog_base()
    {
        return None;
    }
    let key = provider_key(settings, provider, store)?;
    let backend = NativeDecisions::new(api, &key, model.to_owned()).ok()?;
    Some(Box::new(MeteredNative {
        model: entry.model.clone(),
        backend,
    }))
}

struct MeteredNative<B> {
    model: String,
    backend: B,
}

#[async_trait]
impl<B: DecisionBackend> DecisionBackend for MeteredNative<B> {
    async fn decide(&self, request: &DecisionRequest) -> Result<BackendReply, BackendError> {
        let mut call = Scope::call(&self.model, CallPurpose::Decider)
            .map_err(|_| BackendError::Unavailable)?;
        let reply = self.backend.decide(request).await?;
        if let Some(usage) = reply.usage {
            call.usage(&Usage {
                prompt_tokens: usage.input,
                completion_tokens: usage.output,
                cached_tokens: usage.cached_input,
                cache_write_tokens: None,
            });
        }
        Ok(reply)
    }
}

struct DeciderClassifier {
    model: ModelRef,
    provider: Arc<dyn ChatProvider>,
}

#[async_trait]
impl Classifier for DeciderClassifier {
    async fn classify(&self, prompt: &str, max_tokens: u32) -> Result<String, ClassifierError> {
        let mut call = Scope::call(&self.model.0, CallPurpose::Decider)?;
        crate::router::classify_json(
            self.provider.as_ref(),
            self.model.clone(),
            prompt,
            max_tokens,
            &mut call,
        )
        .await
        .map_err(|_| ClassifierError::new("decision provider failed"))
    }
}

#[cfg(test)]
mod tests;

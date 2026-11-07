//! User provider configuration persisted in the data directory
//! (`providers.toml`): which providers are enabled, where each API key lives
//! (a [`SecretRef`], never the key itself), the chosen model and when the
//! provider was added. ADR-0016: the first provider added is the global
//! default unless another one is chosen explicitly.

use std::path::{Path, PathBuf};

use secret_store::{SecretRef, SecretStore, resolve};
use serde::{Deserialize, Serialize};

use crate::{
    entry::{Auth, ProviderEntry},
    error::ProviderError,
    factory::build_chat_provider,
    openai_compat::OpenAiCompatConfig,
    provider::{ChatChunk, ChatProvider, ChatRequest, Message, MessageRole, ModelRef},
    util::key_from_env,
};

/// File name inside the data directory.
pub const CONFIG_FILE_NAME: &str = "providers.toml";

/// Path of the provider config for a data directory.
#[must_use]
pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE_NAME)
}

/// Conventional secret name for a provider's API key.
#[must_use]
pub fn api_key_secret_name(provider_id: &str) -> String {
    format!("provider.{provider_id}.api_key")
}

/// One configured provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// Catalog id (or the id chosen for a custom provider).
    pub id: String,
    /// Whether the provider may be used.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Where the API key is stored; `None` for keyless providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<SecretRef>,
    /// Chosen default model (without the provider prefix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Unix epoch milliseconds when added.
    pub added_at_ms: i64,
    /// Full entry for custom (non-catalog) providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<ProviderEntry>,
}

fn default_true() -> bool {
    true
}

impl ProviderSettings {
    /// Settings for a catalog provider.
    #[must_use]
    pub fn new(id: impl Into<String>, added_at_ms: i64) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            key: None,
            default_model: None,
            added_at_ms,
            custom: None,
        }
    }

    /// `provider/model` reference when a default model is set.
    #[must_use]
    pub fn model_ref(&self) -> Option<ModelRef> {
        self.default_model
            .as_ref()
            .map(|m| ModelRef(format!("{}/{m}", self.id)))
    }

    /// Build a live provider. This is a secret *boundary*: the key is resolved
    /// from `store` here and goes straight into the HTTP client.
    ///
    /// Without a stored key the catalog entry's environment variable (if any)
    /// is used.
    ///
    /// # Errors
    /// Missing secret / env var, or HTTP client construction failure.
    pub fn build_provider(
        &self,
        entry: &ProviderEntry,
        store: &dyn SecretStore,
        config: OpenAiCompatConfig,
    ) -> Result<Box<dyn ChatProvider>, ProviderError> {
        let key = match (&self.key, &entry.auth) {
            (Some(r), _) => Some(
                resolve(store, r)
                    .map_err(|e| ProviderError::Auth(e.to_string()))?
                    .expose()
                    .to_string(),
            ),
            (None, Auth::None) => None,
            (None, Auth::ApiKeyEnv(var)) => Some(key_from_env(var)?),
        };
        build_chat_provider(entry, config, key)
    }
}

/// All configured providers plus the explicit global default, if any.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Explicit global default provider id; `None` means "first added".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// Configured providers in insertion order.
    #[serde(default, rename = "provider")]
    pub providers: Vec<ProviderSettings>,
}

/// Errors from reading / writing the provider config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed `providers.toml`.
    #[error("invalid providers.toml: {0}")]
    Parse(#[from] toml::de::Error),
    /// Serialisation failure.
    #[error("cannot serialise providers.toml: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// Unknown provider id.
    #[error("provider {0:?} is not configured")]
    Unknown(String),
}

impl ProviderConfig {
    /// Load the config; a missing file yields an empty config.
    ///
    /// # Errors
    /// I/O or parse errors.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(toml::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically write the config.
    ///
    /// # Errors
    /// I/O or serialisation errors.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Look up settings by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ProviderSettings> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Add a provider, or replace the existing one with the same id while
    /// keeping its original `added_at_ms` (so re-adding never changes which
    /// provider is "first").
    pub fn upsert(&mut self, mut settings: ProviderSettings) {
        if let Some(existing) = self.providers.iter_mut().find(|p| p.id == settings.id) {
            settings.added_at_ms = existing.added_at_ms;
            *existing = settings;
        } else {
            self.providers.push(settings);
        }
    }

    /// Remove a provider; clears the explicit default if it pointed at it.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        if self.default_provider.as_deref() == Some(id) {
            self.default_provider = None;
        }
        self.providers.len() != before
    }

    /// Choose the global default provider explicitly.
    ///
    /// # Errors
    /// [`ConfigError::Unknown`] if `id` is not configured.
    pub fn set_default(&mut self, id: &str) -> Result<(), ConfigError> {
        if self.get(id).is_none() {
            return Err(ConfigError::Unknown(id.to_string()));
        }
        self.default_provider = Some(id.to_string());
        Ok(())
    }

    /// Set the chosen model of a provider.
    ///
    /// # Errors
    /// [`ConfigError::Unknown`] if `id` is not configured.
    pub fn set_model(&mut self, id: &str, model: &str) -> Result<(), ConfigError> {
        let p = self
            .providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ConfigError::Unknown(id.to_string()))?;
        p.default_model = Some(model.to_string());
        Ok(())
    }

    /// The effective global default: the explicit default if it is enabled,
    /// otherwise the enabled provider with the smallest `added_at_ms`
    /// (insertion order breaks ties).
    #[must_use]
    pub fn effective_default(&self) -> Option<&ProviderSettings> {
        if let Some(p) = self
            .default_provider
            .as_deref()
            .and_then(|id| self.get(id))
            .filter(|p| p.enabled)
        {
            return Some(p);
        }
        self.providers
            .iter()
            .filter(|p| p.enabled)
            .min_by_key(|p| p.added_at_ms)
    }

    /// `provider/model` of the effective default, when it has a model set.
    #[must_use]
    pub fn default_model_ref(&self) -> Option<ModelRef> {
        self.effective_default()
            .and_then(ProviderSettings::model_ref)
    }
}

/// Outcome of a successful connection test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionReport {
    /// Model ids reported by the provider (empty when it has no listing).
    pub models: Vec<String>,
}

/// Test connectivity and credentials: `list_models`, and if the provider has no
/// listing and a `model` is given, a 1-token completion.
///
/// # Errors
/// Any [`ProviderError`] from the probe (auth, connection, ...).
pub async fn test_connection(
    provider: &dyn ChatProvider,
    model: Option<&ModelRef>,
) -> Result<ConnectionReport, ProviderError> {
    use futures::StreamExt;

    let models = provider.list_models().await?;
    if !models.is_empty() {
        return Ok(ConnectionReport { models });
    }
    if let Some(m) = model {
        let mut req = ChatRequest::new(m.clone(), vec![Message::text(MessageRole::User, "ping")]);
        req.max_tokens = Some(1);
        let mut stream = provider.stream(req).await?;
        while let Some(chunk) = stream.next().await {
            if matches!(chunk?, ChatChunk::Done { .. }) {
                break;
            }
        }
    }
    Ok(ConnectionReport { models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{CompatFlags, ModelCapabilities, ProviderKind};
    use secret_store::MemoryStore;

    fn custom_entry() -> ProviderEntry {
        ProviderEntry {
            id: "mine".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:1234/v1".into(),
            auth: Auth::None,
            capabilities: ModelCapabilities {
                tools: true,
                vision: false,
                streaming: true,
                json_mode: false,
            },
            compat_flags: CompatFlags::default(),
            models_endpoint: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn first_added_is_default_until_overridden() {
        let mut c = ProviderConfig::default();
        assert!(c.effective_default().is_none());
        c.upsert(ProviderSettings::new("a", 100));
        c.upsert(ProviderSettings::new("b", 200));
        assert_eq!(c.effective_default().unwrap().id, "a");
        c.set_default("b").unwrap();
        assert_eq!(c.effective_default().unwrap().id, "b");
        assert!(c.set_default("zzz").is_err());
    }

    #[test]
    fn default_follows_earliest_added_not_order() {
        let mut c = ProviderConfig::default();
        c.upsert(ProviderSettings::new("late", 500));
        c.upsert(ProviderSettings::new("early", 100));
        assert_eq!(c.effective_default().unwrap().id, "early");
    }

    #[test]
    fn disabled_and_removed_defaults_fall_back() {
        let mut c = ProviderConfig::default();
        c.upsert(ProviderSettings::new("a", 1));
        c.upsert(ProviderSettings::new("b", 2));
        c.set_default("b").unwrap();
        c.providers[1].enabled = false;
        assert_eq!(c.effective_default().unwrap().id, "a");
        c.providers[1].enabled = true;
        assert!(c.remove("b"));
        assert!(c.default_provider.is_none());
        assert_eq!(c.effective_default().unwrap().id, "a");
    }

    #[test]
    fn re_adding_keeps_original_added_at() {
        let mut c = ProviderConfig::default();
        c.upsert(ProviderSettings::new("a", 1));
        c.upsert(ProviderSettings::new("b", 2));
        c.upsert(ProviderSettings::new("a", 999));
        assert_eq!(c.get("a").unwrap().added_at_ms, 1);
        assert_eq!(c.effective_default().unwrap().id, "a");
    }

    #[test]
    fn model_ref_uses_effective_default() {
        let mut c = ProviderConfig::default();
        let mut s = ProviderSettings::new("openrouter", 1);
        s.default_model = Some("gpt-4o".into());
        c.upsert(s);
        assert_eq!(c.default_model_ref().unwrap().0, "openrouter/gpt-4o");
    }

    #[test]
    fn save_load_round_trip_never_contains_key_value() {
        let dir = std::env::temp_dir().join(format!("pc-test-{}", std::process::id()));
        let path = config_path(&dir);
        let mut c = ProviderConfig::default();
        let mut s = ProviderSettings::new("openrouter", 10);
        s.key = Some(SecretRef::new(api_key_secret_name("openrouter")).unwrap());
        s.default_model = Some("m".into());
        c.upsert(s);
        let mut custom = ProviderSettings::new("mine", 20);
        custom.custom = Some(custom_entry());
        c.upsert(custom);
        c.set_default("mine").unwrap();
        c.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("provider.openrouter.api_key"));
        let back = ProviderConfig::load(&path).unwrap();
        assert_eq!(back, c);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_missing_is_empty() {
        let p = std::env::temp_dir().join("definitely-missing-providers.toml");
        assert_eq!(ProviderConfig::load(&p).unwrap(), ProviderConfig::default());
    }

    #[test]
    fn build_provider_resolves_key_from_store() {
        let store = MemoryStore::new();
        let mut s = ProviderSettings::new("mine", 1);
        let name = api_key_secret_name("mine");
        s.key = Some(SecretRef::new(&name).unwrap());
        // Missing secret -> auth error.
        let err = s
            .build_provider(&custom_entry(), &store, OpenAiCompatConfig::default())
            .err();
        assert!(matches!(err, Some(ProviderError::Auth(_))));
        store.set(&name, "sk-test-123456").unwrap();
        assert!(
            s.build_provider(&custom_entry(), &store, OpenAiCompatConfig::default())
                .is_ok()
        );
        // Keyless entry without a secret ref works.
        let k = ProviderSettings::new("mine", 1);
        assert!(
            k.build_provider(&custom_entry(), &store, OpenAiCompatConfig::default())
                .is_ok()
        );
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream};

    struct Fake {
        models: Vec<String>,
        fail: bool,
    }

    #[async_trait]
    impl ChatProvider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        async fn stream(
            &self,
            req: ChatRequest,
        ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
            assert_eq!(req.max_tokens, Some(1));
            Ok(Box::pin(stream::iter(vec![Ok(ChatChunk::Done {
                finish_reason: crate::provider::FinishReason::Stop,
            })])))
        }
        async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
            if self.fail {
                Err(ProviderError::Auth("bad key".into()))
            } else {
                Ok(self.models.clone())
            }
        }
    }

    #[tokio::test]
    async fn lists_models_when_available() {
        let p = Fake {
            models: vec!["m".into()],
            fail: false,
        };
        assert_eq!(test_connection(&p, None).await.unwrap().models, ["m"]);
    }

    #[tokio::test]
    async fn falls_back_to_one_token_completion() {
        let p = Fake {
            models: vec![],
            fail: false,
        };
        let m = ModelRef("fake/x".into());
        assert_eq!(
            test_connection(&p, Some(&m)).await.unwrap().models,
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn propagates_auth_failure() {
        let p = Fake {
            models: vec![],
            fail: true,
        };
        assert!(matches!(
            test_connection(&p, None).await,
            Err(ProviderError::Auth(_))
        ));
    }
}

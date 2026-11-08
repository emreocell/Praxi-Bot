//! User provider configuration persisted in the data directory
//! (`providers.toml`): which providers are enabled, where each API key lives
//! (a [`SecretRef`], never the key itself), the chosen model and when the
//! provider was added. ADR-0016: the first provider added is the global
//! default unless another one is chosen explicitly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use secret_store::{SecretRef, SecretStore, resolve};
use serde::{Deserialize, Serialize};

use crate::{
    entry::{Auth, CapabilityOverrides, ModelInfo, ProviderEntry},
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
    /// Session configuration; absent for catalog-backed HTTP providers.
    #[serde(default, flatten, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliSettings>,
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
    /// Per-model capability overrides, keyed by model id (no provider prefix).
    /// Empty by default and omitted from `providers.toml` when empty. Applied
    /// on top of the catalog [`ModelInfo`] overrides when the provider is
    /// built; the user's values win. No provenance or date is stored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_caps: BTreeMap<String, CapabilityOverrides>,
}

/// Secret-free CLI provider shape in `providers.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CliSettings {
    /// An installed CLI, resolved on PATH at turn time.
    Cli { engine: CliEngine, command: String },
}

/// Engines supported by the fixture-locked session runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliEngine {
    Claude,
    Codex,
}

impl CliSettings {
    /// Engine and argv0; never a path or shell command.
    #[must_use]
    pub fn parts(&self) -> (CliEngine, &str) {
        match self {
            Self::Cli { engine, command } => (*engine, command),
        }
    }
}

fn default_true() -> bool {
    true
}

impl ProviderSettings {
    /// Settings for a catalog provider.
    #[must_use]
    pub fn new(id: impl Into<String>, added_at_ms: i64) -> Self {
        Self {
            cli: None,
            id: id.into(),
            enabled: true,
            key: None,
            default_model: None,
            added_at_ms,
            custom: None,
            model_caps: BTreeMap::new(),
        }
    }

    /// `entry` with [`Self::model_caps`] written onto each model's capability
    /// overrides. The user's values win field by field over the catalog.
    /// A model named only here is added so the override still applies. No
    /// provenance or date is recorded.
    #[must_use]
    pub fn apply_model_caps(&self, entry: &ProviderEntry) -> ProviderEntry {
        let mut entry = entry.clone();
        if self.model_caps.is_empty() {
            return entry;
        }
        for (id, user) in &self.model_caps {
            if let Some(info) = entry.models.iter_mut().find(|m| m.id == *id) {
                info.capabilities = Some(match info.capabilities.take() {
                    Some(catalog) => user.merge_over(&catalog),
                    None => user.clone(),
                });
            } else {
                entry.models.push(ModelInfo {
                    id: id.clone(),
                    capabilities: Some(user.clone()),
                    ..ModelInfo::default()
                });
            }
        }
        entry
    }

    /// `provider/model` reference when a default model is set.
    #[must_use]
    pub fn model_ref(&self) -> Option<ModelRef> {
        if self.cli.is_some() && self.default_model.is_none() {
            return Some(ModelRef(self.id.clone()));
        }
        self.default_model
            .as_ref()
            .map(|m| ModelRef(format!("{}/{m}", self.id)))
    }

    /// Build a live provider. This is a secret *boundary*: the key is resolved
    /// from `store` here and goes straight into the HTTP client.
    ///
    /// Without a stored key the catalog entry's environment variable (if any)
    /// is used. [`Self::model_caps`] is applied on top of `entry` first.
    ///
    /// # Errors
    /// Missing secret / env var, or HTTP client construction failure.
    pub fn build_provider(
        &self,
        entry: &ProviderEntry,
        store: &dyn SecretStore,
        config: OpenAiCompatConfig,
    ) -> Result<Box<dyn ChatProvider>, ProviderError> {
        if self.cli.is_some() || entry.kind == crate::ProviderKind::Cli {
            return Err(ProviderError::CliSession);
        }
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
        let entry = self.apply_model_caps(entry);
        build_chat_provider(&entry, config, key)
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
    /// A CLI record must contain only an argv0 and no HTTP credentials.
    #[error("invalid CLI provider: {0}")]
    InvalidCli(String),
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
            Ok(s) => {
                let cfg: Self = toml::from_str(&s)?;
                cfg.validate_cli()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically write the config.
    ///
    /// # Errors
    /// I/O or serialisation errors.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate_cli()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn validate_cli(&self) -> Result<(), ConfigError> {
        for p in &self.providers {
            if let Some(cli) = &p.cli {
                let (engine, command) = cli.parts();
                let id = match engine {
                    CliEngine::Claude => "cli:claude",
                    CliEngine::Codex => "cli:codex",
                };
                if p.id != id
                    || p.key.is_some()
                    || p.custom.is_some()
                    || command.is_empty()
                    || command.starts_with('-')
                    || !command
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                {
                    return Err(ConfigError::InvalidCli(p.id.clone()));
                }
            } else if p.id.starts_with("cli:") {
                return Err(ConfigError::InvalidCli(p.id.clone()));
            }
        }
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
    fn model_caps_round_trip_and_legacy_settings_still_load() {
        let dir = std::env::temp_dir().join(format!("pc-caps-{}", std::process::id()));
        let path = config_path(&dir);
        let mut c = ProviderConfig::default();
        let mut s = ProviderSettings::new("ollama", 10);
        s.model_caps.insert(
            "llama3.2".into(),
            CapabilityOverrides {
                tools: Some(false),
                vision: Some(true),
                ..CapabilityOverrides::default()
            },
        );
        c.upsert(s);
        c.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model_caps"));
        assert!(text.contains("llama3.2"));
        assert!(!text.contains("provenance"));
        assert!(!text.contains("as_of"));
        let back = ProviderConfig::load(&path).unwrap();
        assert_eq!(back, c);
        assert_eq!(
            back.get("ollama").unwrap().model_caps["llama3.2"].tools,
            Some(false)
        );
        std::fs::remove_dir_all(&dir).unwrap();

        let legacy = r#"
default_provider = "openrouter"

[[provider]]
id = "openrouter"
enabled = true
added_at_ms = 10
default_model = "openai/gpt-4o"
key = "provider.openrouter.api_key"

[[provider]]
id = "ollama"
enabled = false
added_at_ms = 20
"#;
        let cfg: ProviderConfig = toml::from_str(legacy).unwrap();
        assert_eq!(cfg.default_provider.as_deref(), Some("openrouter"));
        assert!(cfg.get("openrouter").unwrap().model_caps.is_empty());
        assert_eq!(
            cfg.get("openrouter").unwrap().default_model.as_deref(),
            Some("openai/gpt-4o")
        );
        assert!(!cfg.get("ollama").unwrap().enabled);
    }

    #[tokio::test]
    async fn build_provider_user_model_caps_beat_the_catalog() {
        use std::time::Duration;

        use crate::entry::{CapabilityOverrides, ModelInfo};
        use crate::provider::{ChatRequest, Message, MessageRole, ModelRef};

        let store = MemoryStore::new();
        let mut entry = custom_entry();
        entry.base_url = "http://127.0.0.1:1/v1".into();
        entry.models.push(ModelInfo {
            id: "m".into(),
            capabilities: Some(CapabilityOverrides {
                tools: Some(false),
                ..CapabilityOverrides::default()
            }),
            ..ModelInfo::default()
        });
        let cfg = OpenAiCompatConfig {
            max_retries: 0,
            connect_timeout: Duration::from_millis(200),
            timeout: Duration::from_millis(200),
            ..OpenAiCompatConfig::default()
        };
        let mut req = ChatRequest::new(
            ModelRef("mine/m".into()),
            vec![Message::text(MessageRole::User, "hi")],
        );
        req.tools = vec![serde_json::json!({
            "type": "function",
            "function": {"name": "t", "parameters": {"type": "object"}}
        })];

        let s = ProviderSettings::new("mine", 1);
        let err = s
            .build_provider(&entry, &store, cfg.clone())
            .unwrap()
            .stream(req.clone())
            .await
            .err()
            .unwrap();
        assert!(matches!(
            err,
            ProviderError::Unsupported { ref capability } if capability == "tools"
        ));

        let mut user = ProviderSettings::new("mine", 1);
        user.model_caps.insert(
            "m".into(),
            CapabilityOverrides {
                tools: Some(true),
                ..CapabilityOverrides::default()
            },
        );
        let err = user
            .build_provider(&entry, &store, cfg)
            .unwrap()
            .stream(req)
            .await
            .err()
            .unwrap();
        assert!(
            !matches!(err, ProviderError::Unsupported { .. }),
            "user tools=true must beat the catalog override, got {err:?}"
        );
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

#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn cli_enablement_roundtrips_without_secrets_or_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(&path, "[[provider]]\nid='cli:claude'\nadded_at_ms=0\nkind='cli'\nengine='claude'\ncommand='missing-fake'\n").unwrap();
        let cfg = ProviderConfig::load(&path).unwrap();
        let p = cfg.get("cli:claude").unwrap();
        assert_eq!(
            p.cli.as_ref().unwrap().parts(),
            (CliEngine::Claude, "missing-fake")
        );
        assert!(p.key.is_none());
        cfg.save(&path).unwrap();
        assert_eq!(ProviderConfig::load(&path).unwrap(), cfg);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("base_url"));
        assert!(!text.contains("key"));
    }
    #[test]
    fn cli_configuration_rejects_paths_unsupported_engines_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        for fields in [
            "engine='claude'\ncommand='C:/claude.exe'",
            "engine='cursor'\ncommand='cursor'",
            "engine='codex'\ncommand='codex'",
            "engine='claude'\ncommand='claude'\nkey='env:SECRET'",
        ] {
            std::fs::write(
                &path,
                format!("[[provider]]\nid='cli:claude'\nadded_at_ms=0\nkind='cli'\n{fields}\n"),
            )
            .unwrap();
            assert!(ProviderConfig::load(&path).is_err(), "{fields}");
        }
    }
    #[test]
    fn cli_factory_fails_before_resolving_a_secret() {
        let mut entry: ProviderEntry =
            toml::from_str(include_str!("../../../providers/catalog/anthropic.toml")).unwrap();
        entry.kind = crate::ProviderKind::Cli;
        assert!(matches!(
            build_chat_provider(&entry, OpenAiCompatConfig::default(), None),
            Err(ProviderError::CliSession)
        ));
        let mut s = ProviderSettings::new("cli:claude", 0);
        s.key = Some(secret_store::SecretRef::new("missing").unwrap());
        assert!(matches!(
            s.build_provider(
                &entry,
                &secret_store::MemoryStore::new(),
                OpenAiCompatConfig::default()
            ),
            Err(ProviderError::CliSession)
        ));
    }
}

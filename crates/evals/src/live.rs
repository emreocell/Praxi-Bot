//! Live-run plumbing: the opt-in gate, the real provider, and the price table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent_core::ModelPrice;
use async_trait::async_trait;
use futures::stream::BoxStream;
use provider_catalog::{
    Auth, Catalog, ChatChunk, ChatProvider, ChatRequest, ModelRef, OpenAiCompatConfig,
    ProviderConfig, ProviderError, ProviderKind, build_chat_provider, config_path,
};
use secret_store::{BackendChoice, open_store};
use serde::Deserialize;

use crate::{Error, Result};

/// Environment variable that opts into tier-2 runs.
pub const LIVE_ENV: &str = "BOT_EVAL_LIVE";
/// Environment variable naming the judge model (default: the task model).
pub const JUDGE_MODEL_ENV: &str = "BOT_EVAL_JUDGE_MODEL";

/// Tier 2 is opt-in and never runs in CI.
///
/// # Errors
/// A message saying why tier 2 may not run.
pub fn live_allowed(env: impl Fn(&str) -> Option<String>) -> std::result::Result<(), String> {
    if env("CI").is_some_and(|v| !v.is_empty() && v != "0" && v != "false") {
        return Err("refusing to run live tasks in CI (the CI variable is set)".into());
    }
    if env(LIVE_ENV).as_deref() != Some("1") {
        return Err(format!(
            "tier 2 drives your real browser/desktop and a real provider; set {LIVE_ENV}=1 to opt in"
        ));
    }
    Ok(())
}

/// A ready provider and the model reference to use with it.
pub struct LiveProvider {
    /// The provider.
    pub provider: Arc<dyn ChatProvider>,
    /// `provider/model`.
    pub model: String,
}

/// Directory with the provider catalog: explicit, `BOT_CATALOG_DIR`, the repo's
/// `providers/catalog` (found from this crate), or `./providers/catalog`.
fn catalog_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("BOT_CATALOG_DIR") {
        return PathBuf::from(p);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/catalog");
    if repo.is_dir() {
        return repo;
    }
    PathBuf::from("providers/catalog")
}

/// Build the real provider from the user's `providers.toml` and secret store.
///
/// `model` is `provider/model` (the provider must be configured) or a bare model
/// name used with the effective default provider; `None` uses the default
/// provider's default model.
///
/// # Errors
/// No configured provider / model, missing key, catalog problems.
pub fn build_live_provider(
    model: Option<&str>,
    data_dir: &Path,
    catalog: Option<&Path>,
) -> Result<LiveProvider> {
    let cfg = ProviderConfig::load(&config_path(data_dir))?;
    let wanted = model.map(|m| ModelRef(m.to_owned()));
    let wanted_provider = wanted.as_ref().and_then(ModelRef::provider);
    let configured = match wanted_provider {
        Some(p) => cfg.get(p),
        None => cfg.effective_default(),
    };
    if configured.is_none()
        && let (Some(p), Some(m)) = (wanted_provider, &wanted)
        && let Some(live) = native_gemini_preset(p, m, data_dir, catalog)?
    {
        return Ok(live);
    }
    let settings = configured.ok_or("no configured provider (run `bot providers add` first)")?;
    let model_ref = match &wanted {
        Some(m) if m.provider().is_some() => m.clone(),
        Some(m) => ModelRef(format!("{}/{}", settings.id, m.0)),
        None => settings
            .model_ref()
            .ok_or("the default provider has no default model; pass --model provider/model")?,
    };
    let entry = if let Some(c) = &settings.custom {
        c.clone()
    } else {
        let dir = catalog_dir(catalog);
        let cat = Catalog::load_dir(&dir).map_err(|errs| -> Error {
            errs.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
                .into()
        })?;
        cat.get(&settings.id).cloned().ok_or_else(|| {
            format!(
                "provider {:?} is not in the catalog {}",
                settings.id,
                dir.display()
            )
        })?
    };
    let (store, _) = open_store(BackendChoice::Auto, data_dir, &|_| None)?;
    let provider = settings.build_provider(&entry, &*store, compat_config(data_dir))?;
    Ok(LiveProvider {
        provider: Arc::from(provider),
        model: model_ref.0,
    })
}

/// Provider tunables for eval runs: failed HTTP 400 request bodies (redacted, no
/// key) are dumped to `<data_dir>/debug/`.
fn compat_config(data_dir: &Path) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        debug_dir: Some(data_dir.join("debug")),
        ..OpenAiCompatConfig::default()
    }
}

/// The `gemini` preset: `--model gemini/<model>` works without `providers.toml`,
/// through the native Gemini provider (catalog entry `gemini`), with the key read
/// from the entry's environment variable (`GEMINI_API_KEY`). This is the
/// documented default for Gemini runs: the OpenAI-compatible endpoint needs
/// thought-signature handling and leaks thought text on some models.
///
/// Returns `Ok(None)` when `provider` is not a catalog entry of kind `gemini`.
fn native_gemini_preset(
    provider: &str,
    model: &ModelRef,
    data_dir: &Path,
    catalog: Option<&Path>,
) -> Result<Option<LiveProvider>> {
    let dir = catalog_dir(catalog);
    // Lenient: one broken catalog file must not hide the valid presets.
    let (cat, errs) = Catalog::load_dir_lenient(&dir);
    for e in &errs {
        eprintln!("warning: provider catalog: {e}");
    }
    let Some(entry) = cat.get(provider).cloned() else {
        return Ok(None);
    };
    if entry.kind != ProviderKind::Gemini {
        return Ok(None);
    }
    let key = match &entry.auth {
        Auth::ApiKeyEnv(var) => match std::env::var(var) {
            Ok(k) if !k.trim().is_empty() => Some(k),
            _ => return Err(format!("environment variable {var} is not set").into()),
        },
        Auth::None => None,
    };
    let built = build_chat_provider(&entry, compat_config(data_dir), key)?;
    Ok(Some(LiveProvider {
        provider: Arc::from(built),
        model: model.0.clone(),
    }))
}

/// Wraps a provider to space requests at least `min_interval` apart (free-tier
/// rate limits) and to count them (reported on stderr as `[requests N]`).
pub struct Throttled {
    inner: Arc<dyn ChatProvider>,
    min_interval: Duration,
    next_slot: tokio::sync::Mutex<tokio::time::Instant>,
    count: AtomicU64,
}

impl Throttled {
    /// Wrap `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn ChatProvider>, min_interval: Duration) -> Self {
        Self {
            inner,
            min_interval,
            next_slot: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            count: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl ChatProvider for Throttled {
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
        {
            let mut slot = self.next_slot.lock().await;
            tokio::time::sleep_until(*slot).await;
            *slot = tokio::time::Instant::now() + self.min_interval;
        }
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!("[requests {n}]");
        self.inner.stream(req).await
    }

    async fn list_models(&self) -> std::result::Result<Vec<String>, ProviderError> {
        self.inner.list_models().await
    }
}

#[derive(Deserialize)]
struct PriceFile {
    #[serde(default)]
    prices: HashMap<String, PriceRow>,
}

#[derive(Deserialize)]
#[allow(clippy::struct_field_names)]
struct PriceRow {
    input_per_mtok: f64,
    output_per_mtok: f64,
    #[serde(default)]
    cached_per_mtok: Option<f64>,
}

/// Parse a price table (`[prices."provider/model"]` tables, USD per million tokens).
///
/// # Errors
/// Malformed TOML.
pub fn parse_prices(text: &str) -> Result<HashMap<String, ModelPrice>> {
    let f: PriceFile = toml::from_str(text)?;
    Ok(f.prices
        .into_iter()
        .map(|(k, r)| {
            (
                k,
                ModelPrice {
                    input_per_mtok: r.input_per_mtok,
                    output_per_mtok: r.output_per_mtok,
                    cached_per_mtok: r.cached_per_mtok,
                },
            )
        })
        .collect())
}

/// Load the price table; a missing file is an empty table.
///
/// # Errors
/// Unreadable or malformed file.
pub fn load_prices(path: &Path) -> Result<HashMap<String, ModelPrice>> {
    match std::fs::read_to_string(path) {
        Ok(t) => parse_prices(&t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn live_needs_opt_in_and_refuses_ci() {
        assert!(live_allowed(env(&[])).is_err());
        assert!(live_allowed(env(&[(LIVE_ENV, "1")])).is_ok());
        assert!(live_allowed(env(&[(LIVE_ENV, "yes")])).is_err());
        let e = live_allowed(env(&[(LIVE_ENV, "1"), ("CI", "true")])).unwrap_err();
        assert!(e.contains("CI"));
        assert!(live_allowed(env(&[(LIVE_ENV, "1"), ("CI", "")])).is_ok());
    }

    #[test]
    fn prices_parse_and_missing_file_is_empty() {
        let p = parse_prices(
            "[prices.\"deepseek/deepseek-chat\"]\ninput_per_mtok = 0.5\noutput_per_mtok = 1.5\ncached_per_mtok = 0.1\n",
        )
        .unwrap();
        let m = p["deepseek/deepseek-chat"];
        assert!((m.cost(1_000_000, 1_000_000, None) - 2.0).abs() < 1e-9);
        assert!(
            load_prices(Path::new("definitely-missing-prices.toml"))
                .unwrap()
                .is_empty()
        );
        assert!(parse_prices("").unwrap().is_empty());
    }

    #[test]
    fn gemini_preset_only_applies_to_native_gemini_catalog_entries() {
        let d = tempfile::tempdir().unwrap();
        let m = |p: &str| ModelRef(format!("{p}/x"));
        // Not in the catalog / not of kind `gemini`: no preset.
        assert!(
            native_gemini_preset("nope", &m("nope"), d.path(), None)
                .unwrap()
                .is_none()
        );
        assert!(
            native_gemini_preset("openrouter", &m("openrouter"), d.path(), None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn no_configured_provider_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let e = build_live_provider(None, d.path(), None).err().unwrap();
        assert!(e.to_string().contains("no configured provider"));
    }
}

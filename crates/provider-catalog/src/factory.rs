//! Construct the right [`ChatProvider`] for a catalog entry's [`ProviderKind`].

use crate::{
    anthropic::AnthropicProvider,
    entry::{ProviderEntry, ProviderKind},
    error::ProviderError,
    gemini::GeminiProvider,
    openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider},
    provider::ChatProvider,
};

/// Build the provider that speaks `entry.kind`, with an explicit API key (or
/// none).
///
/// # Errors
/// [`ProviderError::InvalidRequest`] for [`ProviderKind::Other`];
/// [`ProviderError::Connection`] if the HTTP client cannot be built.
pub fn build_chat_provider(
    entry: &ProviderEntry,
    config: OpenAiCompatConfig,
    key: Option<String>,
) -> Result<Box<dyn ChatProvider>, ProviderError> {
    let entry = entry.clone();
    Ok(match &entry.kind {
        ProviderKind::OpenAiCompatible => {
            Box::new(OpenAiCompatProvider::with_api_key(entry, config, key)?)
        }
        ProviderKind::Anthropic => Box::new(AnthropicProvider::with_api_key(entry, config, key)?),
        ProviderKind::Gemini => Box::new(GeminiProvider::with_api_key(entry, config, key)?),
        ProviderKind::Other(k) => {
            return Err(ProviderError::InvalidRequest(format!(
                "unsupported provider kind {k:?}"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Auth, CompatFlags, ModelCapabilities};

    fn entry(kind: ProviderKind) -> ProviderEntry {
        ProviderEntry {
            id: "x".into(),
            kind,
            base_url: "http://localhost:1".into(),
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
    fn builds_every_known_kind() {
        for k in [
            ProviderKind::OpenAiCompatible,
            ProviderKind::Anthropic,
            ProviderKind::Gemini,
        ] {
            let p = build_chat_provider(&entry(k), OpenAiCompatConfig::default(), None).unwrap();
            assert_eq!(p.id(), "x");
        }
        let err = build_chat_provider(
            &entry(ProviderKind::Other("zzz".into())),
            OpenAiCompatConfig::default(),
            None,
        )
        .err();
        assert!(matches!(err, Some(ProviderError::InvalidRequest(_))));
    }

    #[test]
    fn builtin_native_catalog_files_parse_with_prices() {
        for (src, kind, id) in [
            (
                include_str!("../../../providers/catalog/anthropic.toml"),
                ProviderKind::Anthropic,
                "claude-sonnet-5-5",
            ),
            (
                include_str!("../../../providers/catalog/gemini.toml"),
                ProviderKind::Gemini,
                "gemini-3.1-flash-lite",
            ),
        ] {
            let e: ProviderEntry = toml::from_str(src).unwrap();
            assert_eq!(e.kind, kind);
            let m = e.model(id).unwrap();
            assert!(m.prices_as_of.is_some());
            let u = crate::provider::Usage {
                prompt_tokens: 1_000_000,
                completion_tokens: 1_000_000,
                cached_tokens: Some(500_000),
                cache_write_tokens: None,
            };
            assert!(m.cost_usd(&u).unwrap() > 0.0);
        }
        // Unknown prices stay unknown.
        let e: ProviderEntry =
            toml::from_str(include_str!("../../../providers/catalog/gemini.toml")).unwrap();
        let pro = e.model("gemini-3.1-pro-preview").unwrap();
        assert!(pro.input_per_mtok.is_none());
        assert!(pro.cost_usd(&crate::provider::Usage::default()).is_none());
    }

    #[test]
    fn cost_splits_read_write_and_plain_input() {
        let m = crate::entry::ModelInfo {
            id: "m".into(),
            input_per_mtok: Some(2.0),
            output_per_mtok: Some(10.0),
            cached_per_mtok: Some(0.2),
            cache_write_per_mtok: Some(2.5),
            ..Default::default()
        };
        let u = crate::provider::Usage {
            prompt_tokens: 3_000_000,
            completion_tokens: 1_000_000,
            cached_tokens: Some(1_000_000),
            cache_write_tokens: Some(1_000_000),
        };
        // 1M plain * 2 + 1M read * 0.2 + 1M write * 2.5 + 1M out * 10
        assert!((m.cost_usd(&u).unwrap() - 14.7).abs() < 1e-9);
    }
}

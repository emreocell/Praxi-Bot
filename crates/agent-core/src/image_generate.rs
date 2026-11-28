//! Host wiring for [`builtin_tools::ImageGenerateBackend`] (ledger + providers).

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use builtin_tools::{GeneratedImage, ImageGenerateBackend};
use protocol::RunId;
use provider_catalog::{
    Catalog, ImageGenerateRequest, ModelRef, OpenAiCompatConfig, ProviderConfig, ProviderError,
    Usage, config_path,
};
use secret_store::SecretStore;

use crate::shared::Shared;
use crate::usage::{CallGuard, CallPurpose};

pub(crate) struct LoopImageGenerate {
    pub(crate) sh: Weak<Shared>,
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) store: Arc<dyn SecretStore>,
}

#[async_trait]
impl ImageGenerateBackend for LoopImageGenerate {
    fn prompt_allowed(&self, prompt: &str) -> Result<(), String> {
        let Some(sh) = self.sh.upgrade() else {
            return Err("agent loop is not available".into());
        };
        if let Some(redactor) = &sh.cfg.redactor
            && redactor.contains_secret(prompt)
        {
            return Err(
                "prompt was refused because it looks like a stored secret; describe the image without secrets"
                    .into(),
            );
        }
        Ok(())
    }

    async fn generate(
        &self,
        run_id: &str,
        bot_id: Option<&str>,
        model: &str,
        prompt: &str,
        size: Option<&str>,
    ) -> Result<GeneratedImage, String> {
        let sh = self
            .sh
            .upgrade()
            .ok_or_else(|| "agent loop is not available".to_owned())?;
        let run_id = uuid::Uuid::parse_str(run_id)
            .map(RunId)
            .map_err(|_| "invalid run id".to_owned())?;
        let ctx = sh
            .runs
            .lock()
            .get(&run_id)
            .cloned()
            .ok_or_else(|| "run context is not available".to_owned())?;
        if let Some(bot) = bot_id
            && ctx.bot_id.to_string() != bot
        {
            return Err("bot id does not match this run".into());
        }
        let model_ref = ModelRef(model.to_owned());
        let provider_id = model_ref.provider().ok_or_else(|| {
            "model must be provider/model (for example openrouter/openai/gpt-image-1)".to_owned()
        })?;
        let mut guard = CallGuard::start(&sh, &ctx, model, CallPurpose::ImageGeneration)?;
        let provider_cfg =
            ProviderConfig::load(&config_path(sh.storage.data_dir())).map_err(|e| e.to_string())?;
        let settings = provider_cfg
            .providers
            .iter()
            .find(|p| p.enabled && p.id == provider_id)
            .ok_or_else(|| format!("provider {provider_id} is not configured or enabled"))?;
        let entry = settings
            .custom
            .clone()
            .or_else(|| self.catalog.get(provider_id).cloned())
            .ok_or_else(|| format!("provider {provider_id} is not in the catalog"))?;
        let image = settings
            .build_image_provider(&entry, self.store.as_ref(), OpenAiCompatConfig::default())
            .map_err(|e: ProviderError| e.to_string())?;
        let wire = image.default_wire_format(model_ref.model());
        let req = ImageGenerateRequest {
            model: model_ref.clone(),
            prompt: prompt.to_owned(),
            size: size.map(str::to_owned),
            format: wire,
        };
        let response = image.generate(req).await.map_err(|e| e.to_string())?;
        guard.usage(&Usage {
            prompt_tokens: 0,
            completion_tokens: 1,
            cached_tokens: None,
            cache_write_tokens: None,
        });
        drop(guard);
        Ok(GeneratedImage {
            bytes: response.bytes,
            mime_type: response.mime_type,
            model: model.to_owned(),
            revised_prompt: response.revised_prompt,
        })
    }
}

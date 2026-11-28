//! Image generation requests, independent of chat capabilities.
//!
//! Adapters perform exactly one HTTP attempt per call. Ledger-aware callers
//! record spend around these calls; these modules never log prompts.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ModelRef, ProviderError};

pub const MAX_IMAGE_PROMPT_CHARS: usize = 8 * 1024;
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Which wire format to use for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageWireFormat {
    OpenAiImages,
    GeminiImagen,
}

/// Generation request (prompt is untrusted user/model text).
#[derive(Clone)]
pub struct ImageGenerateRequest {
    pub model: ModelRef,
    pub prompt: String,
    pub size: Option<String>,
    pub format: ImageWireFormat,
}

impl fmt::Debug for ImageGenerateRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageGenerateRequest")
            .field("model", &self.model.0)
            .field("prompt_chars", &self.prompt.len())
            .field("size", &self.size)
            .field("format", &self.format)
            .finish()
    }
}

impl ImageGenerateRequest {
    /// Check bounds before any network or serialization work.
    ///
    /// # Errors
    /// Invalid model, prompt, or size.
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.model.0.is_empty()
            || self.model.0.len() > 512
            || self.model.0.chars().any(char::is_control)
        {
            return Err(crate::embedding::invalid("invalid image model reference"));
        }
        let prompt = self.prompt.trim();
        if prompt.is_empty() || prompt.len() > MAX_IMAGE_PROMPT_CHARS {
            return Err(crate::embedding::invalid(
                "image prompt is empty or exceeds its size limit",
            ));
        }
        if let Some(size) = &self.size
            && (size.is_empty() || size.len() > 32 || !size.bytes().all(|b| b.is_ascii()))
        {
            return Err(crate::embedding::invalid("invalid image size"));
        }
        Ok(())
    }

    pub(crate) fn model_for<'a>(&'a self, provider: &str) -> &'a str {
        if self.model.provider() == Some(provider) {
            self.model.model()
        } else {
            &self.model.0
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageGenerateResponse {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub revised_prompt: Option<String>,
}

#[async_trait]
pub trait ImageProvider: Send + Sync {
    fn id(&self) -> &str;
    /// Canonical endpoint identity, without secrets.
    ///
    /// # Errors
    /// Invalid endpoint configuration.
    fn endpoint_identity(&self) -> Result<String, ProviderError> {
        crate::embedding::endpoint_identity(self.endpoint_base())
    }
    fn endpoint_base(&self) -> &str;
    fn is_remote(&self) -> bool {
        true
    }
    fn default_wire_format(&self, model: &str) -> ImageWireFormat;
    /// Exactly one attempt.
    async fn generate(
        &self,
        request: ImageGenerateRequest,
    ) -> Result<ImageGenerateResponse, ProviderError>;
}

pub(crate) fn detect_format(provider_kind: &crate::ProviderKind, model: &str) -> ImageWireFormat {
    if matches!(provider_kind, crate::ProviderKind::Gemini) || model.contains("imagen") {
        ImageWireFormat::GeminiImagen
    } else {
        ImageWireFormat::OpenAiImages
    }
}

#[cfg(test)]
mod tests;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};

use super::{GeminiProvider, classify};
use crate::embedding::client;
use crate::embedding::{invalid, json_response};
use crate::image::{ImageGenerateRequest, ImageGenerateResponse, ImageProvider, ImageWireFormat};
use crate::{ProviderError, util::map_reqwest_error};

#[async_trait]
impl ImageProvider for GeminiProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    fn endpoint_base(&self) -> &str {
        &self.entry.base_url
    }

    fn default_wire_format(&self, model: &str) -> ImageWireFormat {
        crate::image::detect_format(&self.entry.kind, model)
    }

    async fn generate(
        &self,
        request: ImageGenerateRequest,
    ) -> Result<ImageGenerateResponse, ProviderError> {
        request.validate()?;
        if request.format != ImageWireFormat::GeminiImagen {
            return Err(invalid(
                "Gemini image generation requires the Imagen wire format",
            ));
        }
        let model = request.model_for(&self.entry.id);
        let model = model.strip_prefix("models/").unwrap_or(model);
        if model.is_empty()
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(invalid("invalid Gemini Imagen model identifier"));
        }
        let body = json!({
            "instances": [{"prompt": request.prompt.trim()}],
            "parameters": {"sampleCount": 1},
        });
        let url = format!(
            "{}/models/{model}:predict",
            crate::embedding::endpoint_identity(&self.entry.base_url)?
        );
        let client = client(&self.config, &self.entry.base_url)?;
        let mut req = client.post(url).json(&body);
        if let Some(key) = &self.key {
            req = req.header("x-goog-api-key", &key.0);
        }
        let response = req.send().await.map_err(map_reqwest_error)?;
        let value = json_response(response, classify).await?;
        decode_gemini_image(&value)
    }
}

fn decode_gemini_image(value: &Value) -> Result<ImageGenerateResponse, ProviderError> {
    let pred = value
        .get("predictions")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| invalid("Gemini image response missing predictions"))?;
    let b64 = pred
        .get("bytesBase64Encoded")
        .or_else(|| pred.pointer("/image/bytesBase64Encoded"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Gemini image response missing image bytes"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| invalid("Gemini image response is not valid base64"))?;
    if bytes.is_empty() || bytes.len() > crate::image::MAX_IMAGE_BYTES {
        return Err(invalid("image payload is empty or too large"));
    }
    Ok(ImageGenerateResponse {
        bytes,
        mime_type: "image/png".into(),
        revised_prompt: None,
    })
}

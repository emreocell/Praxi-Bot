use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};

use super::{OpenAiCompatProvider, classify};
use crate::embedding::client;
use crate::embedding::{invalid, json_response};
use crate::image::{ImageGenerateRequest, ImageGenerateResponse, ImageProvider, ImageWireFormat};
use crate::{ProviderError, util::map_reqwest_error};

#[async_trait]
impl ImageProvider for OpenAiCompatProvider {
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
        if request.format != ImageWireFormat::OpenAiImages {
            return Err(invalid(
                "this OpenAI-compatible endpoint requires the OpenAI images wire format",
            ));
        }
        let model = request.model_for(&self.entry.id);
        let mut body = json!({
            "model": model,
            "prompt": request.prompt.trim(),
            "response_format": "b64_json",
        });
        if let Some(size) = &request.size {
            body["size"] = json!(size);
        }
        let url = format!(
            "{}/images/generations",
            crate::embedding::endpoint_identity(&self.entry.base_url)?
        );
        let client = client(&self.config, &self.entry.base_url)?;
        let mut req = client.post(url).json(&body);
        if let Some(key) = &self.key {
            req = req.bearer_auth(&key.0);
        }
        let response = req.send().await.map_err(map_reqwest_error)?;
        let value = json_response(response, classify).await?;
        decode_openai_image(&value)
    }
}

fn decode_openai_image(value: &Value) -> Result<ImageGenerateResponse, ProviderError> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| invalid("image response missing data"))?;
    let b64 = data
        .get("b64_json")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("image response missing b64_json"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| invalid("image response is not valid base64"))?;
    if bytes.is_empty() || bytes.len() > crate::image::MAX_IMAGE_BYTES {
        return Err(invalid("image payload is empty or too large"));
    }
    Ok(ImageGenerateResponse {
        bytes,
        mime_type: "image/png".into(),
        revised_prompt: data
            .get("revised_prompt")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

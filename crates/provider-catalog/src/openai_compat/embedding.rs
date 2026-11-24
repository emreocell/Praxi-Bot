use async_trait::async_trait;
use serde_json::{Value, json};

use super::{OpenAiCompatProvider, classify};
use crate::embedding::{
    EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, EmbeddingTaskFormat, client,
    endpoint_identity, invalid, json_response, vector,
};
use crate::{ProviderError, util::map_reqwest_error};

impl OpenAiCompatProvider {
    async fn embedding_send(
        &self,
        url: String,
        body: Option<Value>,
    ) -> Result<Value, ProviderError> {
        let client = client(&self.config, &self.entry.base_url)?;
        let mut request = if let Some(body) = body {
            client.post(url).json(&body)
        } else {
            client.get(url)
        };
        if let Some(key) = &self.key {
            request = request.bearer_auth(&key.0);
        }
        let response = request.send().await.map_err(map_reqwest_error)?;
        json_response(response, classify).await
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    fn endpoint_identity(&self) -> Result<String, ProviderError> {
        endpoint_identity(&self.entry.base_url)
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, ProviderError> {
        request.validate()?;
        if request.format != EmbeddingTaskFormat::Plain {
            return Err(invalid(
                "OpenAI-compatible embedding requires plain task format",
            ));
        }
        let model = request.model_for(&self.entry.id);
        let mut body = json!({
            "model": model,
            "input": request.inputs.iter().map(|input| &input.text).collect::<Vec<_>>(),
            "encoding_format": "float",
        });
        if request.request_dimensions {
            body["dimensions"] = json!(request.dimensions);
        }
        let response = self
            .embedding_send(
                format!("{}/embeddings", self.endpoint_identity()?),
                Some(body),
            )
            .await?;
        let data = response
            .get("data")
            .and_then(Value::as_array)
            .filter(|values| values.len() == request.inputs.len())
            .ok_or_else(|| invalid("embedding response count mismatch"))?;
        let mut vectors = vec![None; request.inputs.len()];
        for item in data {
            let index = item
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .filter(|&index| index < vectors.len())
                .ok_or_else(|| invalid("embedding response index out of range"))?;
            if vectors[index].is_some() {
                return Err(invalid("embedding response has duplicate indices"));
            }
            vectors[index] = Some(vector(
                item.get("embedding").unwrap_or(&Value::Null),
                request.dimensions,
            )?);
        }
        let returned_model = response.get("model").and_then(Value::as_str);
        // Gateways may return the resolved deployment name for an alias.
        // Preserve it as metadata; vector identity is the configured profile.
        Ok(EmbeddingResponse {
            vectors: vectors
                .into_iter()
                .map(|vector| vector.ok_or_else(|| invalid("embedding response missing index")))
                .collect::<Result<_, _>>()?,
            model: returned_model.map(str::to_owned),
            input_tokens: response
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64),
        })
    }

    async fn list_embedding_models(&self) -> Result<Vec<String>, ProviderError> {
        // This endpoint does not identify model capabilities. These are choices
        // to verify explicitly, never evidence that every listed model embeds.
        let response = self
            .embedding_send(format!("{}/models", self.endpoint_identity()?), None)
            .await?;
        let models = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("embedding provider model list is missing data"))?;
        Ok(models
            .iter()
            .filter_map(|model| model.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect())
    }
}

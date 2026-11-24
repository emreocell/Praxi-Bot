use async_trait::async_trait;
use serde_json::{Value, json};

use super::{GeminiProvider, classify};
use crate::embedding::{
    EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, EmbeddingTask, EmbeddingTaskFormat,
    client, endpoint_identity, invalid, json_response, vector,
};
use crate::{ProviderError, util::map_reqwest_error};

#[async_trait]
impl EmbeddingProvider for GeminiProvider {
    fn id(&self) -> &str {
        &self.entry.id
    }

    fn endpoint_identity(&self) -> Result<String, ProviderError> {
        endpoint_identity(&self.entry.base_url)
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, ProviderError> {
        request.validate()?;
        if !matches!(
            request.format,
            EmbeddingTaskFormat::Gemini001 | EmbeddingTaskFormat::Gemini2
        ) {
            return Err(invalid(
                "Gemini requires an explicit Gemini embedding task format",
            ));
        }
        let model = request.model_for(&self.entry.id);
        let model = model.strip_prefix("models/").unwrap_or(model);
        if model.is_empty()
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(invalid("invalid Gemini embedding model identifier"));
        }
        let requests: Vec<Value> = request
            .inputs
            .iter()
            .map(|input| {
                let text = match (request.format, request.task) {
                    (EmbeddingTaskFormat::Gemini2, EmbeddingTask::Query) => {
                        format!("task: search result | query: {}", input.text)
                    }
                    (EmbeddingTaskFormat::Gemini2, EmbeddingTask::Document) => {
                        format!(
                            "title: {} | text: {}",
                            input.title.as_deref().unwrap_or("none"),
                            input.text
                        )
                    }
                    _ => input.text.clone(),
                };
                let mut item = json!({
                    "model": format!("models/{model}"),
                    "content": {"parts": [{"text": text}]},
                });
                if request.request_dimensions {
                    item["outputDimensionality"] = json!(request.dimensions);
                }
                if request.format == EmbeddingTaskFormat::Gemini001 {
                    item["taskType"] = json!(match request.task {
                        EmbeddingTask::Query => "RETRIEVAL_QUERY",
                        EmbeddingTask::Document => "RETRIEVAL_DOCUMENT",
                    });
                    if request.task == EmbeddingTask::Document
                        && let Some(title) = &input.title
                    {
                        item["title"] = json!(title);
                    }
                }
                item
            })
            .collect();
        // Each chunk is its own EmbedContentRequest. Multiple parts in a single
        // Embedding 2 request would silently aggregate the chunks into one vector.
        let url = format!(
            "{}/models/{model}:batchEmbedContents",
            self.endpoint_identity()?
        );
        let response = self
            .authed(
                client(&self.config, &self.entry.base_url)?
                    .post(url)
                    .json(&json!({"requests": requests})),
            )
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let response = json_response(response, classify).await?;
        let embeddings = response
            .get("embeddings")
            .and_then(Value::as_array)
            .filter(|values| values.len() == request.inputs.len())
            .ok_or_else(|| invalid("Gemini embedding response count mismatch"))?;
        let vectors = embeddings
            .iter()
            .map(|item| {
                vector(
                    item.get("values").unwrap_or(&Value::Null),
                    request.dimensions,
                )
            })
            .collect::<Result<_, _>>()?;
        Ok(EmbeddingResponse {
            vectors,
            model: Some(model.to_owned()),
            input_tokens: response
                .pointer("/usageMetadata/promptTokenCount")
                .and_then(Value::as_u64)
                .or_else(|| {
                    response
                        .pointer("/usageMetadata/totalTokenCount")
                        .and_then(Value::as_u64)
                }),
        })
    }

    async fn list_embedding_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = format!("{}/models?pageSize=1000", self.endpoint_identity()?);
        let response = self
            .authed(client(&self.config, &self.entry.base_url)?.get(url))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let response = json_response(response, classify).await?;
        let models = response
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("Gemini model list is missing models"))?;
        Ok(models
            .iter()
            .filter(|model| {
                model
                    .get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_some_and(|methods| {
                        methods
                            .iter()
                            .any(|method| method.as_str() == Some("embedContent"))
                    })
            })
            .filter_map(|model| model.get("name").and_then(Value::as_str))
            .map(|model| model.strip_prefix("models/").unwrap_or(model).to_owned())
            .collect())
    }
}

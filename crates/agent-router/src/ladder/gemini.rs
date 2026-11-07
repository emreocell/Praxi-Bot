//! Direct Gemini HTTP clients for K2 embeddings and the K3 classifier.
//!
//! Small and standalone on purpose (one request each, no streaming, no
//! retries: the ladder is fail-open and has its own timeouts). The API key
//! travels in the `x-goog-api-key` header and is never logged or put in a URL;
//! `Debug` output redacts it.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::BackendError;
use super::classifier::Classifier;
use super::semantic::{EmbedTask, Embedder};

/// Default public endpoint.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Default embedding model.
pub const DEFAULT_EMBED_MODEL: &str = "gemini-embedding-001";
/// Maximum requests per `batchEmbedContents` call (Gemini API limit).
const MAX_BATCH: usize = 100;

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    body: &Value,
) -> Result<Value, BackendError> {
    let resp = client
        .post(url)
        .header("x-goog-api-key", key)
        .json(body)
        .send()
        .await
        .map_err(|e| BackendError::new(format!("request failed: {}", e.without_url())))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(BackendError::new(format!("HTTP {status}")));
    }
    resp.json()
        .await
        .map_err(|e| BackendError::new(format!("bad response body: {}", e.without_url())))
}

/// Gemini embeddings (`batchEmbedContents`).
#[derive(Clone)]
pub struct GeminiEmbedder {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    dimensions: Option<u32>,
}

impl fmt::Debug for GeminiEmbedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiEmbedder")
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl GeminiEmbedder {
    /// New embedder for `model` (e.g. [`DEFAULT_EMBED_MODEL`]).
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: http(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: api_key.into(),
            model: model.into(),
            dimensions: Some(256),
        }
    }

    /// Override the endpoint (tests, proxies).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let url: String = base_url.into();
        url.trim_end_matches('/').clone_into(&mut self.base_url);
        self
    }

    /// Truncate vectors to `dimensions` (`None` = model default).
    #[must_use]
    pub fn with_dimensions(mut self, dimensions: Option<u32>) -> Self {
        self.dimensions = dimensions;
        self
    }
}

#[async_trait]
impl Embedder for GeminiEmbedder {
    async fn embed(
        &self,
        texts: &[String],
        task: EmbedTask,
    ) -> Result<Vec<Vec<f32>>, BackendError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let task_type = match task {
            EmbedTask::Document => "RETRIEVAL_DOCUMENT",
            EmbedTask::Query => "RETRIEVAL_QUERY",
        };
        let url = format!("{}/models/{}:batchEmbedContents", self.base_url, self.model);
        let mut out = Vec::with_capacity(texts.len());
        // `batchEmbedContents` accepts at most `MAX_BATCH` requests per call;
        // chunks are sent in order so the output keeps the input order.
        for chunk in texts.chunks(MAX_BATCH) {
            match self.embed_chunk(&url, chunk, task_type).await {
                Ok(mut v) => out.append(&mut v),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        chunk_size = chunk.len(),
                        total = texts.len(),
                        "Gemini embedding chunk failed"
                    );
                    return Err(e);
                }
            }
        }
        Ok(out)
    }
}

impl GeminiEmbedder {
    async fn embed_chunk(
        &self,
        url: &str,
        texts: &[String],
        task_type: &str,
    ) -> Result<Vec<Vec<f32>>, BackendError> {
        let requests: Vec<Value> = texts
            .iter()
            .map(|t| {
                let mut r = json!({
                    "model": format!("models/{}", self.model),
                    "content": {"parts": [{"text": t}]},
                    "taskType": task_type,
                });
                if let Some(d) = self.dimensions {
                    r["outputDimensionality"] = json!(d);
                }
                r
            })
            .collect();
        let v = post(
            &self.client,
            url,
            &self.api_key,
            &json!({ "requests": requests }),
        )
        .await?;
        let embeddings = v
            .get("embeddings")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendError::new("response has no embeddings"))?;
        if embeddings.len() != texts.len() {
            return Err(BackendError::new("embedding count mismatch"));
        }
        embeddings
            .iter()
            .map(|e| {
                e.get("values")
                    .and_then(Value::as_array)
                    .ok_or_else(|| BackendError::new("embedding without values"))?
                    .iter()
                    .map(|n| {
                        #[allow(clippy::cast_possible_truncation)]
                        n.as_f64()
                            .map(|f| f as f32)
                            .ok_or_else(|| BackendError::new("non-numeric embedding value"))
                    })
                    .collect()
            })
            .collect()
    }
}

/// Gemini `generateContent` as a [`Classifier`] (JSON mime type, temperature 0).
#[derive(Clone)]
pub struct GeminiClassifier {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl fmt::Debug for GeminiClassifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiClassifier")
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl GeminiClassifier {
    /// New classifier for `model` (the profile's `classifier` role).
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: http(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// Override the endpoint (tests, proxies).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let url: String = base_url.into();
        url.trim_end_matches('/').clone_into(&mut self.base_url);
        self
    }
}

#[async_trait]
impl Classifier for GeminiClassifier {
    async fn classify(&self, prompt: &str, max_tokens: u32) -> Result<String, BackendError> {
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "generationConfig": {
                "maxOutputTokens": max_tokens,
                "temperature": 0,
                "responseMimeType": "application/json",
            },
        });
        let url = format!("{}/models/{}:generateContent", self.base_url, self.model);
        let v = post(&self.client, &url, &self.api_key, &body).await?;
        let parts = v
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendError::new("response has no candidate text"))?;
        let text: String = parts
            .iter()
            .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        if text.is_empty() {
            return Err(BackendError::new("empty candidate text"));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;

    /// Echoes one 1-d vector per request: the number in the request text.
    struct Echo;

    impl Respond for Echo {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let embeddings: Vec<Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let n: f64 = r["content"]["parts"][0]["text"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap();
                    json!({ "values": [n] })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "embeddings": embeddings }))
        }
    }

    #[tokio::test]
    async fn embeds_250_texts_in_three_ordered_requests() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/m:batchEmbedContents"))
            .respond_with(Echo)
            .mount(&server)
            .await;
        let e = GeminiEmbedder::new("k", "m").with_base_url(server.uri());
        let texts: Vec<String> = (0..250).map(|i| i.to_string()).collect();
        let out = e.embed(&texts, EmbedTask::Document).await.unwrap();
        assert_eq!(out.len(), 250);
        for (i, v) in out.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let want = i as f32;
            assert_eq!(v, &vec![want]);
        }
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 3);
        let sizes: Vec<usize> = reqs
            .iter()
            .map(|r| {
                let b: Value = serde_json::from_slice(&r.body).unwrap();
                b["requests"].as_array().unwrap().len()
            })
            .collect();
        assert_eq!(sizes, [100, 100, 50]);
    }

    #[tokio::test]
    async fn a_failing_chunk_fails_the_whole_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let e = GeminiEmbedder::new("k", "m").with_base_url(server.uri());
        let texts: Vec<String> = (0..150).map(|i| i.to_string()).collect();
        let err = e.embed(&texts, EmbedTask::Document).await.unwrap_err();
        assert!(err.to_string().contains("500"));
        // Stops at the first failing chunk.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

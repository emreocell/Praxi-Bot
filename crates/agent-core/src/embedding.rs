//! Adapter: the shared embedding hub as the decision router's (K2) embedder.

use std::sync::Arc;
use std::time::Duration;

use agent_router::ladder::{BackendError, EmbedTask, Embedder};
use async_trait::async_trait;
use embedding_hub::{EmbedItem, EmbeddingHub, Purpose, Task};

/// Router embedder backed by the hub. A turn must not wait on an embedding
/// provider, so it waits at most a quarter of a second for a rate-limit slot
/// and answers with BM25 only when the pinned provider cannot serve it.
pub(crate) struct HubRouterEmbedder {
    hub: Arc<EmbeddingHub>,
}

impl HubRouterEmbedder {
    pub(crate) fn new(hub: Arc<EmbeddingHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl Embedder for HubRouterEmbedder {
    fn model_name(&self) -> String {
        self.hub.preferred_space(Purpose::Router).key()
    }

    fn max_batch_size(&self) -> usize {
        64
    }

    async fn embed(
        &self,
        texts: &[String],
        task: EmbedTask,
    ) -> Result<Vec<Vec<f32>>, BackendError> {
        let pin = self.hub.preferred_space(Purpose::Router);
        let items: Vec<EmbedItem> = texts.iter().map(EmbedItem::new).collect();
        let task = match task {
            EmbedTask::Document => Task::Document,
            EmbedTask::Query => Task::Query,
        };
        self.hub
            .embed(
                Purpose::Router,
                &items,
                task,
                None,
                Some(&pin),
                Duration::from_millis(250),
            )
            .await
            .map(|e| e.vectors)
            .map_err(|_| BackendError::new("the embedding provider is not available right now"))
    }
}

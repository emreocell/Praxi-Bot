//! Resolving a stored secret off the async runtime threads.

use std::sync::Arc;

use secret_store::{SecretRef, SecretStore, resolve};

use crate::error::EmailError;

/// Fetch the secret `r` points at. The value lives only as long as the caller
/// keeps the returned `String`; errors never contain it.
pub(crate) async fn resolve_secret(
    store: &Arc<dyn SecretStore>,
    r: &SecretRef,
) -> Result<String, EmailError> {
    let store = Arc::clone(store);
    let r = r.clone();
    tokio::task::spawn_blocking(move || resolve(store.as_ref(), &r).map(|v| v.expose().to_string()))
        .await
        .map_err(|e| EmailError::Credential(e.to_string()))?
        .map_err(|e| EmailError::Credential(e.to_string()))
}

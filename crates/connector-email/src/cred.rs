//! Resolving a stored secret off the async runtime threads.

use std::sync::Arc;

use secret_store::{SecretRef, SecretStore, resolve};

use crate::error::EmailError;

/// What every failure to read a stored secret says. The secret store's own text is not
/// forwarded: it can carry the reference's name and platform details, and reaches the model
/// and the owner through the tool error.
const UNREADABLE: &str = "the stored secret could not be read";

/// Fetch the secret `r` points at. The value lives only as long as the caller
/// keeps the returned `String`; errors never contain it or the store's own text.
pub(crate) async fn resolve_secret(
    store: &Arc<dyn SecretStore>,
    r: &SecretRef,
) -> Result<String, EmailError> {
    let store = Arc::clone(store);
    let r = r.clone();
    tokio::task::spawn_blocking(move || resolve(store.as_ref(), &r).map(|v| v.expose().to_string()))
        .await
        .map_err(|_| EmailError::Credential(UNREADABLE.into()))?
        .map_err(|_| EmailError::Credential(UNREADABLE.into()))
}

//! Error types for the provider catalog and chat provider trait.

use thiserror::Error;

/// Errors from catalog loading or provider operations.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// A TOML catalog file could not be parsed.
    #[error("catalog parse error in {path}: {source}")]
    CatalogParse {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    /// No provider matched the selection criteria.
    #[error("no provider available for model ref: {0}")]
    NoProvider(String),

    /// The provider returned an error response.
    #[error("provider error ({status}): {message}")]
    Api { status: u16, message: String },

    /// The connection to the provider failed.
    #[error("connection error: {0}")]
    Connection(String),

    /// A streaming response could not be decoded.
    #[error("stream error: {0}")]
    Stream(String),
}

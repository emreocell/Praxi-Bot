//! Provider-independent, memory-only screen transport. Never log input or pixels.
pub mod dto;
mod source;
pub mod wire;
pub use dto::*;
pub use source::*;

/// Protocol failures contain only fixed codes, never backend or user payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewError {
    #[error("unsupported")]
    Unsupported,
    #[error("unavailable")]
    Unavailable,
    #[error("invalid_record")]
    InvalidRecord,
    #[error("limit_exceeded")]
    LimitExceeded,
    #[error("unsupported_version")]
    UnsupportedVersion,
    #[error("partial_injection")]
    PartialInjection,
}

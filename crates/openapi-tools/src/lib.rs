//! Turn `OpenAPI` 3.0 / 3.1 specs into agent tools at runtime.
//!
//! ```ignore
//! let cfg = OpenApiConfig::new(secret_store);          // + cfg.auth = Auth::Bearer { .. }
//! let (api, tools) = import_tools(&spec_text, &ImportOptions::default(), &cfg)?;
//! // or: openapi_tools::register(&registry, &api, &cfg)?;
//! ```
//!
//! * One tool per operation; name from `operationId` (or method + path), JSON
//!   Schema input merged from path / query / header parameters (flat) and the
//!   request body (under `body`), with local `$ref`s fully inlined.
//! * Effects: GET / HEAD / OPTIONS are [`tool_registry::Effect::Read`], DELETE is
//!   `Destructive`, everything else `Write`, so the permission engine can ask.
//! * Credentials are [`Auth`] secret references resolved at call time; the
//!   parameter carrying them is hidden from the schema and the values are
//!   redacted from errors and response bodies.
//! * Limits: operation count, spec size, request timeout, response size, no
//!   redirects, private-network hosts blocked unless allowed.

#![allow(clippy::too_many_lines)]

pub mod error;
pub mod import;
pub mod tool;

pub use error::OpenApiError;
pub use import::{
    ApiSpec, BodyKind, BodySpec, HttpMethod, ImportOptions, Operation, ParamBinding, ParamLocation,
    SecuritySchemeInfo, SkippedOperation,
};
pub use tool::{Auth, OpenApiConfig, effect_for, import_tools, register, tools};

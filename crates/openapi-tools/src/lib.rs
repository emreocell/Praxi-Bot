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
//!   Money-like paths and operation ids (pay, charge, checkout, ...) are in the
//!   `Money` category and always ask ([`risk`]).
//! * Per-bot access through [`OpenApiConfig::gate`].
//! * Credentials are [`Auth`] secret references resolved at call time; the
//!   parameter carrying them is hidden from the schema and the values are
//!   redacted from errors and response bodies.
//! * Limits: operation count, spec size, request timeout, response size, no
//!   redirects, private-network hosts blocked unless allowed.

#![allow(clippy::too_many_lines)]

pub mod error;
pub mod import;
pub mod risk;
pub mod spec_fetch;
pub mod tool;

pub use error::OpenApiError;
pub use import::{
    ApiSpec, BodyKind, BodySpec, HttpMethod, ImportOptions, Operation, ParamBinding, ParamLocation,
    SecuritySchemeInfo, SkippedOperation,
};
pub use risk::{Risk, is_money_like, risk_of};
pub use spec_fetch::{fetch_spec, fetch_spec_allowing_private};
pub use tool::{AccessGate, Auth, OpenApiConfig, effect_for, import_tools, register, tools};

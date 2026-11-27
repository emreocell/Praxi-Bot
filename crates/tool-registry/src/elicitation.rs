//! An owner-input callback scoped to one tool invocation.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use protocol::elicitation::ElicitationField;
use serde_json::{Map, Value};

/// Validated display metadata from an MCP server.
pub struct Form {
    pub server: String,
    pub message: String,
    pub fields: Vec<ElicitationField>,
}

/// The owner's decision. Answers must never be formatted or journaled.
pub enum Decision {
    Accept(Map<String, Value>),
    Decline,
    Cancel,
    Parked(String),
}

type Answer = Pin<Box<dyn Future<Output = Decision> + Send>>;
type Callback = dyn Fn(Form) -> Answer + Send + Sync;

/// A host-supplied callback. Tool callers without one cannot solicit input.
#[derive(Clone)]
pub struct ElicitationHandler(Arc<Callback>);

impl ElicitationHandler {
    #[must_use]
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(Form) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Decision> + Send + 'static,
    {
        Self(Arc::new(move |form| Box::pin(f(form))))
    }

    pub async fn ask(&self, form: Form) -> Decision {
        (self.0)(form).await
    }
}

impl fmt::Debug for ElicitationHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ElicitationHandler(..)")
    }
}

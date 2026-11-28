//! MCP elicitation: an MCP server asks the owner for input while one of its
//! tool calls runs. The bridge turns the server's request into an
//! [`ElicitationRequest`]; the agent loop (an [`Elicitor`]) shows the form and
//! returns the owner's [`ElicitationAnswer`], which goes back to the server
//! only: never into a tool output, an event, the journal or a log.

use async_trait::async_trait;
use protocol::event::ElicitationField;
use serde_json::{Map, Value};

/// A form a server wants the owner to fill in. Every text is the server's own:
/// untrusted.
#[derive(Debug, Clone, PartialEq)]
pub struct ElicitationRequest {
    /// The asking server's configured id.
    pub server: String,
    pub message: String,
    pub fields: Vec<ElicitationField>,
}

/// How the owner answered. `Debug` never prints the content.
#[derive(Clone, PartialEq)]
pub enum ElicitationAnswer {
    /// Submitted, with values checked against the form.
    Accept(Map<String, Value>),
    /// The owner refused to give the information.
    Decline,
    /// The form was dismissed, stopped or expired.
    Cancel,
    /// Nobody is at the screen (an unattended mission session): no form was
    /// shown and the work item is parked for the owner. The server gets
    /// `cancel`; the text is a note for the model, appended to the tool result.
    Parked(String),
}

impl std::fmt::Debug for ElicitationAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Accept(_) => "Accept(<redacted>)",
            Self::Decline => "Decline",
            Self::Cancel => "Cancel",
            Self::Parked(_) => "Parked",
        })
    }
}

/// Shows an elicitation form to the owner of a run and waits for the answer.
#[async_trait]
pub trait Elicitor: Send + Sync {
    /// Ask on behalf of run `run_id` (the [`crate::ToolContext::run_id`] of the
    /// tool call in flight). A run that cannot ask gets [`ElicitationAnswer::Decline`].
    async fn elicit(&self, run_id: &str, request: ElicitationRequest) -> ElicitationAnswer;

    /// The bridge declined a request of `server` on behalf of run `run_id`
    /// before it reached [`Self::elicit`] (rate limit, or a form over the size
    /// limits). `reason` is a fixed sentence: never a server text. The host
    /// records it in the audit log.
    fn refused(&self, _run_id: &str, _server: &str, _reason: &str) {}
}

/// The longest an open form waits for the owner, whatever `approval_timeout`
/// says (0 there means "no limit" for approvals, never for a server's form).
/// When it runs out the server gets `cancel`.
pub const FORM_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

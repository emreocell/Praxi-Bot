//! Client-side handler for tool discovery signals and owner-only forms.

use std::sync::Arc;

use rmcp::{
    ClientHandler, RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
        CustomRequest, CustomResult, ElicitRequestParams, ElicitResult, ElicitationAction,
        ElicitationCapability, ErrorCode, FormElicitationCapability, InputRequest,
    },
    service::{NotificationContext, Peer, RequestContext, RunningService, ServiceError},
};
use tool_registry::elicitation::Decision;

use crate::{
    diagnostics::Redactor,
    elicitation::{Call, InFlight, Outcome, Rate, project},
};

/// Signals and budgets kept by the server across reconnects.
#[derive(Default)]
pub(crate) struct Signals {
    pub(crate) tools_changed: tokio::sync::Notify,
    pub(crate) elicitation_rate: Rate,
}

/// Handler installed on every session.
#[derive(Clone, Default)]
pub(crate) struct BridgeClient {
    pub(crate) signals: Arc<Signals>,
    pub(crate) calls: Arc<InFlight>,
    pub(crate) server: String,
    pub(crate) redactor: Redactor,
}

pub(crate) type Running = RunningService<RoleClient, BridgeClient>;

impl ClientHandler for BridgeClient {
    fn get_info(&self) -> ClientConfig {
        let capabilities = ClientCapabilities::builder()
            .enable_elicitation_with(
                ElicitationCapability::new().with_form(FormElicitationCapability::new()),
            )
            .build();
        ClientConfig::new(capabilities, ClientConfig::default().client_info)
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        // One permit collapses a burst while the watcher is listing.
        self.signals.tools_changed.notify_one();
    }

    /// The 2025 revisions send `elicitation/create` as a request of its own,
    /// naming no tool call. It is tied to the one call running on this
    /// server's session; with none, several, or an overlap it is declined.
    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        let Some(call) = self.calls.sole() else {
            return Ok(ElicitResult::new(ElicitationAction::Decline));
        };
        tokio::select! {
            biased;
            () = context.ct.cancelled() => Ok(ElicitResult::new(ElicitationAction::Cancel)),
            result = self.elicit(request, &call) => Ok(result),
        }
    }

    fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<CustomResult, rmcp::ErrorData>> + '_ {
        let result = if request.method == "elicitation/create" {
            // rmcp preserves requests whose typed payload could not decode as
            // custom requests (for example an unsupported nested schema).
            Ok(CustomResult(serde_json::json!({"action": "decline"})))
        } else {
            Err(rmcp::ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                request.method,
                None,
            ))
        };
        std::future::ready(result)
    }
}

impl BridgeClient {
    /// Ask the owner for one form that belongs to `call`.
    async fn elicit(&self, request: ElicitRequestParams, call: &Arc<Call>) -> ElicitResult {
        let decline = || ElicitResult::new(ElicitationAction::Decline);
        let Some(handler) = call.claim(&self.server) else {
            return decline();
        };
        // Before the card exists: neither its text nor the answer may reach a log.
        call.mute(&self.redactor);
        let Some(form) = self
            .signals
            .elicitation_rate
            .allow()
            .then(|| project(&call.server, request))
            .flatten()
        else {
            *call.outcome.lock() = Some(Outcome::Declined);
            return decline();
        };
        let mut done = call.done.subscribe();
        let answer = tokio::select! {
            biased;
            _ = done.wait_for(|done| *done) => Decision::Cancel,
            answer = call.ask(handler, form) => answer,
        };
        // The outer call can finish or be cancelled before its nested request.
        // In that case drop the answer without transmitting it.
        let (outcome, result) = match answer {
            Decision::Accept(_) if *done.borrow() => (
                Outcome::Cancelled,
                ElicitResult::new(ElicitationAction::Cancel),
            ),
            Decision::Accept(content) => (
                Outcome::Accepted,
                ElicitResult::new(ElicitationAction::Accept)
                    .with_content(serde_json::Value::Object(content)),
            ),
            Decision::Decline => (Outcome::Declined, decline()),
            Decision::Cancel => (
                Outcome::Cancelled,
                ElicitResult::new(ElicitationAction::Cancel),
            ),
            Decision::Parked(reason) => (Outcome::Parked(reason), decline()),
        };
        *call.outcome.lock() = Some(outcome);
        result
    }

    pub(crate) async fn call_tool(
        &self,
        peer: &Peer<RoleClient>,
        mut params: CallToolRequestParams,
        call: Arc<Call>,
    ) -> Result<CallToolResult, ServiceError> {
        const MAX_ROUNDS: usize = 4;
        let _lease = self.calls.enter(Arc::clone(&call));
        // MCP 2026-07-28 carries these requests in an intermediate tools/call
        // result, so the form is tied to this call by construction. Earlier
        // revisions dispatch via `create_elicitation`.
        for _ in 0..MAX_ROUNDS {
            match peer.call_tool_once(params.clone()).await? {
                CallToolResponse::Complete(result) => return Ok(result),
                CallToolResponse::InputRequired(input) => {
                    let requests = input.input_requests.unwrap_or_default();
                    if requests.len() > MAX_ROUNDS {
                        return Err(ServiceError::UnexpectedResponse);
                    }
                    // Several forms at once: the owner could not tell which one
                    // an answer belongs to, so none is shown.
                    let ambiguous = requests.len() > 1;
                    let mut answers = std::collections::BTreeMap::new();
                    for (key, request) in requests {
                        let InputRequest::Elicitation(request) = request else {
                            return Err(ServiceError::UnexpectedResponse);
                        };
                        let answer = if ambiguous {
                            ElicitResult::new(ElicitationAction::Decline)
                        } else {
                            self.elicit(request.params, &call).await
                        };
                        answers.insert(
                            key,
                            serde_json::to_value(answer)
                                .map_err(|_| ServiceError::UnexpectedResponse)?,
                        );
                    }
                    params.input_responses = Some(answers);
                    params.request_state = input.request_state;
                }
                _ => return Err(ServiceError::UnexpectedResponse),
            }
        }
        Err(ServiceError::InputRequiredRoundsExceeded {
            max_rounds: MAX_ROUNDS,
        })
    }
}

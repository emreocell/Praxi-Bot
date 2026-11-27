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

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        tokio::select! {
            biased;
            () = context.ct.cancelled() => Ok(ElicitResult::new(ElicitationAction::Cancel)),
            result = self.elicit(request, None) => Ok(result),
        }
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleClient>,
    ) -> Result<CustomResult, rmcp::ErrorData> {
        if request.method == "elicitation/create" {
            // rmcp preserves requests whose typed payload could not decode as
            // custom requests (for example an unsupported nested schema).
            return Ok(CustomResult(serde_json::json!({"action": "decline"})));
        }
        Err(rmcp::ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            request.method,
            None,
        ))
    }
}

impl BridgeClient {
    async fn elicit(
        &self,
        request: ElicitRequestParams,
        expected: Option<&Arc<Call>>,
    ) -> ElicitResult {
        let decline = || ElicitResult::new(ElicitationAction::Decline);
        let Some(call) = self.calls.take(expected) else {
            return decline();
        };
        let Some(handler) = &call.handler else {
            return decline();
        };
        let Some(form) = self
            .signals
            .elicitation_rate
            .allow()
            .then(|| project(&self.server, request))
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
            Decision::Accept(content) => {
                // The private session runtime may print or reformat the answer in
                // transport diagnostics. Stop forwarding them before sending it.
                self.redactor.mute();
                (
                    Outcome::Accepted,
                    ElicitResult::new(ElicitationAction::Accept)
                        .with_content(serde_json::Value::Object(content)),
                )
            }
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
        let _lease = self.calls.enter(Arc::clone(&call));
        // MCP 2026-07-28 carries these same requests in an intermediate
        // tools/call result. Earlier revisions dispatch via ClientHandler.
        const MAX_ROUNDS: usize = 4;
        for _ in 0..MAX_ROUNDS {
            match peer.call_tool_once(params.clone()).await? {
                CallToolResponse::Complete(result) => return Ok(result),
                CallToolResponse::InputRequired(input) => {
                    let requests = input.input_requests.unwrap_or_default();
                    if requests.len() > MAX_ROUNDS {
                        return Err(ServiceError::UnexpectedResponse);
                    }
                    let mut answers = std::collections::BTreeMap::new();
                    for (key, request) in requests {
                        let InputRequest::Elicitation(request) = request else {
                            return Err(ServiceError::UnexpectedResponse);
                        };
                        let answer = self.elicit(request.params, Some(&call)).await;
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

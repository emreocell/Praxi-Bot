//! One-shot owner decisions for MCP forms. Submitted content only travels on
//! the private verdict channel, then back to the server.

use std::{sync::Arc, time::Duration};

use protocol::{
    Event, InitiatorKind, RunId, ToolCallId,
    elicitation::{
        ElicitationClosed, ElicitationField, ElicitationFieldKind as Kind,
        ElicitationFormat as Format, ElicitationLink, ElicitationRequested, MAX_FIELDS,
        MAX_VALUE_BYTES,
    },
};
use serde_json::{Map, Value, json};
use tool_registry::elicitation::{Decision, ElicitationHandler, Form};

use crate::{
    AgentLoop,
    shared::{PendingKind, RunCtx, Shared, Verdict},
};

const MAX_PER_RUN: u32 = 3;
const MAX_OPEN: usize = 16;
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);

/// Why the daemon could not record an elicitation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationAnswerError {
    /// The form is expired, closed, or unknown.
    Gone,
    /// The answer fails the schema or contains a suspected credential.
    Invalid,
}

impl Shared {
    pub(crate) fn elicitation_handler(
        self: &Arc<Self>,
        run_id: RunId,
        tool_name: &str,
    ) -> Option<ElicitationHandler> {
        if !tool_name.starts_with("mcp:") {
            return None;
        }
        let sh = Arc::downgrade(self);
        Some(ElicitationHandler::new(move |form| {
            let sh = sh.upgrade();
            async move {
                let Some(sh) = sh else {
                    return Decision::Cancel;
                };
                let ctx = sh.runs.lock().get(&run_id).cloned();
                let Some(ctx) = ctx else {
                    return Decision::Cancel;
                };
                sh.elicit(&ctx, form).await
            }
        }))
    }

    async fn elicit(self: &Arc<Self>, ctx: &RunCtx, mut form: Form) -> Decision {
        if self.run_is_stopped(ctx.run_id) {
            return Decision::Cancel;
        }
        {
            let mut state = ctx.state.lock();
            if state.elicitations >= MAX_PER_RUN {
                return Decision::Decline;
            }
            state.elicitations += 1;
        }
        let server = self.redact(form.server);
        if let Some(decision) = self.park_unattended_elicitation(ctx, &server) {
            return decision;
        }
        // The chat endpoint records Telegram and Discord as user-message
        // sources even when the run itself has the Human initiator kind.
        let via_channel = ctx.initiator == InitiatorKind::Channel
            || self
                .storage
                .run_user_message_sources(ctx.run_id)
                .is_ok_and(|sources| {
                    sources.first().is_some_and(|source| {
                        matches!(source.as_str(), "telegram" | "telegram_owner" | "discord")
                    })
                });

        let limit = match self.cfg.approval_timeout {
            t if t.is_zero() => MAX_WAIT,
            t => t.min(MAX_WAIT),
        };
        let call_id = ToolCallId::new();
        form.message = self.redact(form.message);
        self.redact_form_fields(&mut form.fields);
        let (approval_id, mut guard, rx) = {
            let mut forms = self.elicitations.lock();
            if forms.len() >= MAX_OPEN {
                return Decision::Decline;
            }
            let park = self.park(
                ctx,
                call_id,
                &format!("MCP form from {server}"),
                true,
                false,
                PendingKind::Elicitation,
            );
            forms.insert(call_id, form.fields.clone());
            park
        };
        if self.run_is_stopped(ctx.run_id) {
            return Decision::Cancel;
        }
        self.emit(
            ctx,
            Event::ElicitationRequested(ElicitationRequested {
                run_id: ctx.run_id,
                call_id,
                server,
                message: form.message,
                fields: form.fields,
                expires_in_secs: limit.as_secs().max(1),
            }),
        );
        if via_channel {
            self.emit(
                ctx,
                Event::ElicitationLink(ElicitationLink {
                    run_id: ctx.run_id,
                    chat_id: ctx.chat_id,
                }),
            );
        }
        let (answer, status) = match tokio::time::timeout(limit, rx).await {
            Ok(Ok(Verdict::Elicitation(Decision::Accept(values)))) => {
                (Decision::Accept(values), "approved")
            }
            Ok(Ok(Verdict::Elicitation(Decision::Decline))) => (Decision::Decline, "declined"),
            Err(_) => (Decision::Cancel, "expired"),
            _ => (Decision::Cancel, "cancelled"),
        };
        self.elicitations.lock().remove(&call_id);
        let _ = self.storage.decide_approval(approval_id, status);
        self.audit(
            ctx,
            if status == "expired" {
                "system"
            } else {
                "user"
            },
            "elicitation.decided",
            &json!({"decision": status}),
        );
        guard.resume();
        self.emit(
            ctx,
            Event::ElicitationClosed(ElicitationClosed {
                run_id: ctx.run_id,
                call_id,
            }),
        );
        answer
    }

    fn park_unattended_elicitation(&self, ctx: &RunCtx, server: &str) -> Option<Decision> {
        if let Some(mission) = &ctx.mission {
            let reason =
                format!("needs owner input for MCP server {server}. Run this step from the app");
            let item = self.mission_store.read().clone().and_then(|store| {
                store
                    .block_sole_doing_item(mission, &reason, false, storage::now_ms())
                    .ok()
                    .flatten()
            });
            self.audit(
                ctx,
                "system",
                "elicitation.parked",
                &json!({"server": server, "item": item}),
            );
            return Some(Decision::Parked(crate::approval::mission_park_text(
                item.as_deref(),
                &reason,
            )));
        }
        if !matches!(ctx.initiator, InitiatorKind::Human | InitiatorKind::Channel) {
            self.audit(
                ctx,
                "system",
                "elicitation.parked",
                &json!({"server": server}),
            );
            return Some(Decision::Parked(format!(
                "This step needs the owner to answer a form from MCP server {server} in the app. Leave it blocked, continue independent work, and report the blocked step. Do not retry or guess the answer."
            )));
        }

        None
    }

    fn redact_form_fields(&self, fields: &mut [ElicitationField]) {
        // Display labels are untrusted. If a stored secret matches a schema
        // key or enum value, block the field so the shown and sent data agree.
        for (i, field) in fields.iter_mut().enumerate() {
            field.label = self.redact(std::mem::take(&mut field.label));
            field.description = field.description.take().map(|s| self.redact(s));
            field.blocked |= self.redact(field.name.clone()) != field.name
                || field
                    .options
                    .iter()
                    .any(|o| self.redact(o.value.clone()) != o.value);
            if field.blocked {
                field.name = format!("blocked-{i}");
                field.description = None;
                field.options.clear();
            }
            for option in &mut field.options {
                option.label = self.redact(std::mem::take(&mut option.label));
            }
        }
    }
}

impl AgentLoop {
    /// Answer a live MCP form with values that match its schema.
    ///
    /// # Errors
    /// The form is gone, or the values fail validation.
    pub fn submit_elicitation(
        &self,
        call_id: ToolCallId,
        values: Map<String, Value>,
    ) -> Result<(), ElicitationAnswerError> {
        self.decide_elicitation(call_id, Decision::Accept(values))
    }

    /// Close a live form without an answer.
    ///
    /// # Errors
    /// The form is gone.
    pub fn close_elicitation(
        &self,
        call_id: ToolCallId,
        declined: bool,
    ) -> Result<(), ElicitationAnswerError> {
        self.decide_elicitation(
            call_id,
            if declined {
                Decision::Decline
            } else {
                Decision::Cancel
            },
        )
    }

    fn decide_elicitation(
        &self,
        call_id: ToolCallId,
        action: Decision,
    ) -> Result<(), ElicitationAnswerError> {
        use ElicitationAnswerError::{Gone, Invalid};
        let mut forms = self.sh.elicitations.lock();
        let fields = forms.get(&call_id).ok_or(Gone)?;
        if let Decision::Accept(values) = &action
            && !valid_answer(fields, values, self.sh.cfg.redactor.as_ref())
        {
            return Err(Invalid);
        }
        let mut pending = self.sh.pending.lock();
        let p = pending.get(&call_id).ok_or(Gone)?;
        if p.kind != PendingKind::Elicitation || self.sh.run_is_stopped(p.run_id) {
            return Err(Gone);
        }
        let p = pending.remove(&call_id).ok_or(Gone)?;
        forms.remove(&call_id);
        p.tx.send(Verdict::Elicitation(action)).map_err(|_| Gone)
    }
}

fn valid_answer(
    fields: &[ElicitationField],
    values: &Map<String, Value>,
    redactor: Option<&crate::SharedRedactor>,
) -> bool {
    values.len() <= MAX_FIELDS
        && fields
            .iter()
            .all(|field| !field.required || (!field.blocked && values.contains_key(&field.name)))
        && values.iter().all(|(key, value)| {
            fields
                .iter()
                .any(|f| !f.blocked && f.name == *key && matches_field(f, value))
                && value.as_str().is_none_or(|s| {
                    secret_store::detect_secrets(s).is_empty()
                        && redactor.is_none_or(|r| !r.contains_secret(s))
                })
        })
}

fn matches_field(field: &ElicitationField, value: &Value) -> bool {
    match field.kind {
        Kind::Boolean => value.is_boolean(),
        Kind::Enum => value
            .as_str()
            .is_some_and(|s| field.options.iter().any(|o| o.value == s)),
        Kind::String => value.as_str().is_some_and(|s| {
            let length = s.chars().count();
            s.len() <= MAX_VALUE_BYTES
                && field.min_length.is_none_or(|min| length >= min as usize)
                && field.max_length.is_none_or(|max| length <= max as usize)
                && string_format(field.format, s)
        }),
        Kind::Number | Kind::Integer => value.as_f64().is_some_and(|n| {
            (field.kind == Kind::Number || (n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0))
                && field.minimum.is_none_or(|min| n >= min)
                && field.maximum.is_none_or(|max| n <= max)
        }),
    }
}

fn string_format(format: Option<Format>, value: &str) -> bool {
    match format {
        None => true,
        Some(Format::Email) => value.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty()
                && !host.is_empty()
                && !host.contains('@')
                && !value.chars().any(char::is_whitespace)
        }),
        Some(Format::Uri) => url::Url::parse(value)
            .is_ok_and(|uri| uri.username().is_empty() && uri.password().is_none()),
        Some(Format::Date) => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
        Some(Format::DateTime) => chrono::DateTime::parse_from_rfc3339(value).is_ok(),
    }
}

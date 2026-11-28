//! MCP elicitation: an MCP server asks the owner for input while one of its
//! tool calls runs (the bridge finds the run, see `mcp-bridge`'s `client.rs`).
//!
//! The run parks like a `secret_request`: a form card (`ElicitationRequested`)
//! and a wait bounded by the approval timeout, and never by more than
//! [`FORM_MAX_WAIT`] (an `approval_timeout` of 0 means no limit for approvals,
//! not for a server's form). Only a run the owner started at the desktop may
//! ask; any other run declines at once and the refusal is audited. An
//! unattended mission session parks its item instead of asking. Over the caps
//! below a request is declined and audited. The owner's answer is checked
//! against the form here and goes back to the server only: it is never put into
//! a tool output, an event, the journal, the audit log or a log line. If the
//! server echoes it in its tool result, the value is masked there (see
//! [`Shared::redact_elicited`]).

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use protocol::event::{ElicitationField, ElicitationFieldType, ElicitationRequested};
use protocol::{Event, InitiatorKind, RunId, ToolCallId};
use serde_json::{Map, Value, json};
use tool_registry::elicitation::FORM_MAX_WAIT;
use tool_registry::{ElicitationAnswer, ElicitationRequest, Elicitor};

use crate::approval::Waited;
use crate::history::truncate_utf8;
use crate::shared::{PendingKind, RunCtx, Shared, Verdict};

/// Forms one run may be asked for (shown or refused after the owner check).
pub(crate) const MAX_PER_RUN: u32 = 3;
/// Forms open at once, across every run.
pub(crate) const MAX_OPEN: usize = 16;

/// A parked form: what an answer is checked against.
#[derive(Debug, Clone)]
pub(crate) struct ElicitationInfo {
    pub fields: Vec<ElicitationField>,
}

/// Why [`crate::AgentLoop::finish_elicitation`] did not take an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElicitationRefusal {
    /// No form waits under this id (answered, expired, stopped).
    Gone,
    /// The answer does not fit the form; the form keeps waiting. The message
    /// names the field, never a value.
    Invalid(String),
}

impl Shared {
    /// Whether the run was started, and has been steered only, by the owner at
    /// this computer's desktop: the one kind of run a form may be shown for.
    /// Fails closed (a run with no user message, a message from a channel, a
    /// paired computer or a peer all fail).
    fn desktop_owner_run(&self, ctx: &RunCtx) -> bool {
        ctx.initiator == InitiatorKind::Human
            && self
                .storage
                .run_user_message_sources(ctx.run_id)
                .is_ok_and(|sources| {
                    !sources.is_empty() && sources.iter().all(|source| source == "owner")
                })
    }

    fn refuse_elicitation(&self, ctx: &RunCtx, server: &str, reason: &str) -> ElicitationAnswer {
        self.audit(
            ctx,
            "system",
            "elicitation.refused",
            &json!({"server": truncate_utf8(server, 100), "reason": reason}),
        );
        ElicitationAnswer::Decline
    }

    /// An unattended mission session cannot show a form: the item it is working
    /// on is parked as blocked (the owner sees it in the blocked list) and the
    /// model is told so next to the server's result. The server gets `cancel`.
    fn park_elicitation(&self, ctx: &RunCtx, mission: &str, server: &str) -> ElicitationAnswer {
        let server = truncate_utf8(server, 100);
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
        ElicitationAnswer::Parked(crate::approval::mission_park_text(item.as_deref(), &reason))
    }

    /// A form's texts and keys, with anything a stored secret matches masked.
    /// A field whose name or choice is such a secret is blocked: what is shown
    /// and what could be sent must agree.
    fn redact_form_fields(&self, fields: &mut [ElicitationField]) {
        for (i, field) in fields.iter_mut().enumerate() {
            field.title = field.title.take().map(|s| self.redact(s));
            field.description = field.description.take().map(|s| self.redact(s));
            let name_is_secret = self.redact(field.name.clone()) != field.name;
            field.blocked |= name_is_secret
                || field
                    .options
                    .iter()
                    .any(|o| self.redact(o.value.clone()) != o.value);
            if field.blocked {
                if name_is_secret {
                    field.name = format!("blocked-{i}");
                }
                field.description = None;
                field.options.clear();
                field.default = None;
            }
            for option in &mut field.options {
                option.label = self.redact(std::mem::take(&mut option.label));
            }
            if field
                .default
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|s| self.redact(s.to_owned()) != s)
            {
                field.default = None;
            }
        }
    }

    /// Remember what the owner typed so the rest of the run can mask it. Scoped
    /// to this run (a fresh [`secret_store::Redactor`] in its context, gone with
    /// it): the value is not registered with the app-wide redactor.
    fn remember_elicited(ctx: &RunCtx, values: &Map<String, Value>) {
        let mut state = ctx.state.lock();
        let redactor = state
            .elicited
            .get_or_insert_with(secret_store::Redactor::new);
        for value in values.values() {
            let text = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            for variant in secret_store::secret_variants(&text) {
                redactor.add_secret(&variant);
            }
            // A server may echo the value trimmed.
            redactor.add_secret(text.trim());
        }
    }

    /// Mask in `text` (a tool result of the run) every value the owner typed
    /// into one of this run's forms, in the same way the app masks a secret.
    /// Values under [`secret_store::MIN_REDACTABLE_SECRET_BYTES`] cannot be
    /// masked reliably (they would hit unrelated text) and are left alone.
    pub(crate) fn redact_elicited(ctx: &RunCtx, text: String) -> String {
        match &ctx.state.lock().elicited {
            Some(r) if r.contains_secret(&text) => r.redact(&text),
            _ => text,
        }
    }

    /// Show an MCP server's form to the owner of `ctx` and wait for the answer.
    #[allow(clippy::too_many_lines)] // one ordered flow: checks, park, wait, record
    pub(crate) async fn request_elicitation(
        self: &Arc<Self>,
        ctx: &RunCtx,
        request: ElicitationRequest,
    ) -> ElicitationAnswer {
        let server = request.server;
        if let Some(mission) = ctx.mission.as_deref() {
            return self.park_elicitation(ctx, mission, &server);
        }
        if !self.desktop_owner_run(ctx) {
            return self.refuse_elicitation(
                ctx,
                &server,
                "not a run the owner started at this computer's desktop",
            );
        }
        {
            let mut state = ctx.state.lock();
            if state.elicitations >= MAX_PER_RUN {
                drop(state);
                return self.refuse_elicitation(ctx, &server, "this run asked for forms too often");
            }
            state.elicitations += 1;
        }
        let call_id = ToolCallId::new();
        let mut fields = request.fields;
        self.redact_form_fields(&mut fields);
        // Reserve the slot first so two runs cannot both take the last one.
        {
            let mut forms = self.elicitations.lock();
            if forms.len() >= MAX_OPEN {
                drop(forms);
                return self.refuse_elicitation(ctx, &server, "too many forms are open");
            }
            forms.insert(
                call_id,
                ElicitationInfo {
                    fields: fields.clone(),
                },
            );
        }
        let summary = format!("mcp form: {}", truncate_utf8(&server, 80));
        let (approval_id, mut guard, rx) = self.park(
            ctx,
            call_id,
            &summary,
            true,
            false,
            PendingKind::Elicitation,
        );
        let limit = match self.cfg.approval_timeout {
            t if t.is_zero() => FORM_MAX_WAIT,
            t => t.min(FORM_MAX_WAIT),
        };
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        self.audit(
            ctx,
            "system",
            "elicitation.requested",
            &json!({"server": truncate_utf8(&server, 100), "fields": names}),
        );
        self.emit(
            ctx,
            Event::ElicitationRequested(ElicitationRequested {
                run_id: ctx.run_id,
                call_id,
                server: server.clone(),
                message: self.redact(request.message),
                fields,
                expires_in_secs: limit.as_secs().max(1),
            }),
        );
        let waited = match tokio::time::timeout(limit, self.wait_verdict(ctx, rx, false)).await {
            Ok(waited) => waited,
            // The hard cap: `approval_timeout` 0 (no limit) must not hold the
            // call, the owner's screen and the server forever.
            Err(_) => Waited::Expired,
        };
        guard.resume();
        self.elicitations.lock().remove(&call_id);
        let (answer, outcome) = match waited {
            Waited::Verdict(Ok(Verdict::Elicited(answer))) => {
                let outcome = match &answer {
                    ElicitationAnswer::Accept(values) => {
                        Self::remember_elicited(ctx, values);
                        "answered"
                    }
                    ElicitationAnswer::Decline => "declined",
                    ElicitationAnswer::Cancel | ElicitationAnswer::Parked(_) => "cancelled",
                };
                (answer, outcome)
            }
            Waited::Paused | Waited::Parked | Waited::Expired => {
                (ElicitationAnswer::Cancel, "expired")
            }
            // Stop, a deny through another route, or a dropped card.
            Waited::Verdict(_) => (ElicitationAnswer::Cancel, "cancelled"),
        };
        crate::approval::settle_approval(&self.storage, approval_id, outcome);
        self.audit(
            ctx,
            "user",
            "elicitation.finished",
            &json!({"server": truncate_utf8(&server, 100), "outcome": outcome}),
        );
        answer
    }
}

/// The [`Elicitor`] the daemon installs into the MCP bridge.
pub(crate) struct LoopElicitor {
    pub sh: Weak<Shared>,
}

#[async_trait]
impl Elicitor for LoopElicitor {
    async fn elicit(&self, run_id: &str, request: ElicitationRequest) -> ElicitationAnswer {
        let Some(sh) = self.sh.upgrade() else {
            return ElicitationAnswer::Decline;
        };
        let Ok(id) = run_id.parse::<uuid::Uuid>() else {
            return ElicitationAnswer::Decline;
        };
        let Some(ctx) = sh.runs.lock().get(&RunId(id)).cloned() else {
            return ElicitationAnswer::Decline;
        };
        sh.request_elicitation(&ctx, request).await
    }

    fn refused(&self, run_id: &str, server: &str, reason: &str) {
        let Some(sh) = self.sh.upgrade() else { return };
        let Ok(id) = run_id.parse::<uuid::Uuid>() else {
            return;
        };
        let Some(ctx) = sh.runs.lock().get(&RunId(id)).cloned() else {
            return;
        };
        sh.refuse_elicitation(&ctx, server, reason);
    }
}

/// Check an answer against its form: only the form's fields, every required one,
/// each of the right type, inside its bounds and choices. Empty (`null`)
/// optional fields are dropped. A blocked (credential-looking) field takes no
/// value, ever, and a text that is itself a credential (a known key shape, a
/// labelled password, or a value the app knows as a secret) is refused. An error
/// names the field, never the value.
pub(crate) fn validate(
    fields: &[ElicitationField],
    content: Map<String, Value>,
    redactor: Option<&crate::SharedRedactor>,
) -> Result<Map<String, Value>, String> {
    if content.keys().any(|k| !fields.iter().any(|f| &f.name == k)) {
        return Err("the answer has a field this form does not ask for".into());
    }
    for f in fields {
        if f.blocked {
            // Never filled, so a required one can never be answered: the owner
            // declines or cancels the form instead.
            let filled = !matches!(content.get(&f.name), None | Some(Value::Null));
            if filled || f.required {
                return Err(format!(
                    "`{}` is blocked: it looks like a credential and is never sent",
                    f.name
                ));
            }
            continue;
        }
        match content.get(&f.name) {
            None | Some(Value::Null) if f.required => {
                return Err(format!("`{}` is required", f.name));
            }
            None | Some(Value::Null) => {}
            Some(v) => check(f, v, redactor)?,
        }
    }
    Ok(content.into_iter().filter(|(_, v)| !v.is_null()).collect())
}

/// The longest text a field takes when the server set no `maxLength`.
const DEFAULT_MAX_TEXT_CHARS: u32 = 2000;

fn check(
    f: &ElicitationField,
    v: &Value,
    redactor: Option<&crate::SharedRedactor>,
) -> Result<(), String> {
    let name = &f.name;
    let bounded = |n: f64| {
        if f.minimum.is_some_and(|min| n < min) || f.maximum.is_some_and(|max| n > max) {
            Err(format!("`{name}` is out of range"))
        } else {
            Ok(())
        }
    };
    match f.kind {
        ElicitationFieldType::String => {
            let s = v.as_str().ok_or_else(|| format!("`{name}` must be text"))?;
            if !f.options.is_empty() && !f.options.iter().any(|o| o.value == s) {
                return Err(format!("`{name}` must be one of the offered choices"));
            }
            let len = u32::try_from(s.chars().count()).unwrap_or(u32::MAX);
            if f.min_length.is_some_and(|m| len < m)
                || len > f.max_length.unwrap_or(DEFAULT_MAX_TEXT_CHARS)
            {
                return Err(format!("`{name}` has the wrong length"));
            }
            if !secret_store::detect_secrets(s).is_empty()
                || redactor.is_some_and(|r| r.contains_secret(s))
            {
                return Err(format!(
                    "`{name}` looks like a credential, which a form never takes"
                ));
            }
            match f.format.as_deref() {
                Some(format) if !format_ok(format, s) => {
                    Err(format!("`{name}` is not a valid {format}"))
                }
                _ => Ok(()),
            }
        }
        ElicitationFieldType::Number => bounded(
            v.as_f64()
                .ok_or_else(|| format!("`{name}` must be a number"))?,
        ),
        ElicitationFieldType::Integer => {
            if !(v.is_i64() || v.is_u64()) {
                return Err(format!("`{name}` must be a whole number"));
            }
            bounded(v.as_f64().unwrap_or_default())
        }
        ElicitationFieldType::Boolean if v.is_boolean() => Ok(()),
        ElicitationFieldType::Boolean => Err(format!("`{name}` must be yes or no")),
    }
}

fn format_ok(format: &str, s: &str) -> bool {
    match format {
        "email" => s.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
                && !s.chars().any(char::is_whitespace)
        }),
        "uri" => url::Url::parse(s).is_ok(),
        "date" => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok(),
        "date-time" => chrono::DateTime::parse_from_rfc3339(s).is_ok(),
        // The protocol allows only the four above; an unknown one is not checked.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::event::ElicitationOption;

    fn field(name: &str, kind: ElicitationFieldType, required: bool) -> ElicitationField {
        ElicitationField {
            name: name.into(),
            title: None,
            description: None,
            kind,
            required,
            options: Vec::new(),
            minimum: None,
            maximum: None,
            min_length: None,
            max_length: None,
            format: None,
            default: None,
            blocked: false,
        }
    }

    fn form() -> Vec<ElicitationField> {
        let mut email = field("email", ElicitationFieldType::String, true);
        email.format = Some("email".into());
        let mut age = field("age", ElicitationFieldType::Integer, false);
        age.minimum = Some(0.0);
        age.maximum = Some(150.0);
        let mut size = field("size", ElicitationFieldType::String, false);
        size.options = vec![ElicitationOption {
            value: "s".into(),
            label: "Small".into(),
        }];
        let mut note = field("note", ElicitationFieldType::String, false);
        note.max_length = Some(3);
        vec![
            email,
            age,
            size,
            note,
            field("ratio", ElicitationFieldType::Number, false),
            field("ok", ElicitationFieldType::Boolean, false),
        ]
    }

    fn answer(v: &Value) -> Result<Map<String, Value>, String> {
        validate(&form(), v.as_object().unwrap().clone(), None)
    }

    #[test]
    fn a_fitting_answer_is_taken_and_empty_optional_fields_dropped() {
        let ok = answer(&json!({
            "email": "a@example.com", "age": 30, "size": "s", "note": "abc",
            "ratio": 0.5, "ok": true, "unused": null
        }));
        assert!(ok.is_err(), "unknown keys are refused even when null");
        let ok = answer(&json!({"email": "a@example.com", "age": null, "ok": false})).unwrap();
        assert_eq!(ok.len(), 2);
        assert!(!ok.contains_key("age"));
    }

    #[test]
    fn answers_that_do_not_fit_are_refused_without_echoing_the_value() {
        let secret = "s3cret-value";
        for bad in [
            json!({}),
            json!({"email": null}),
            json!({"email": secret}),
            json!({"email": "a@example.com", "age": 151}),
            json!({"email": "a@example.com", "age": 1.5}),
            json!({"email": "a@example.com", "age": "3"}),
            json!({"email": "a@example.com", "size": secret}),
            json!({"email": "a@example.com", "note": secret}),
            json!({"email": "a@example.com", "ratio": "x"}),
            json!({"email": "a@example.com", "ok": "yes"}),
            json!({"email": "a@example.com", secret: 1}),
        ] {
            let err = answer(&bad).expect_err(&bad.to_string());
            assert!(!err.contains(secret), "{err}");
        }
    }

    #[test]
    fn string_formats_are_checked() {
        assert!(format_ok("email", "me@example.org"));
        assert!(!format_ok("email", "me@example"));
        assert!(!format_ok("email", "m e@example.org"));
        assert!(format_ok("uri", "https://example.com/x"));
        assert!(!format_ok("uri", "not a uri"));
        assert!(format_ok("date", "2026-11-26"));
        assert!(!format_ok("date", "2026-13-01"));
        assert!(format_ok("date-time", "2026-11-26T10:00:00Z"));
        assert!(!format_ok("date-time", "2026-11-26"));
    }

    #[test]
    fn a_blocked_field_takes_no_value_and_a_required_one_can_only_be_declined() {
        let mut blocked = field("password", ElicitationFieldType::String, false);
        blocked.blocked = true;
        let fields = vec![blocked.clone()];
        let ask = |v: &Value| validate(&fields, v.as_object().unwrap().clone(), None);
        assert!(ask(&json!({"password": "hunter2"})).is_err());
        assert!(ask(&json!({"password": ""})).is_err());
        assert_eq!(ask(&json!({})).unwrap().len(), 0);
        assert_eq!(ask(&json!({"password": null})).unwrap().len(), 0);
        blocked.required = true;
        let fields = vec![blocked];
        assert!(validate(&fields, Map::new(), None).is_err());
    }

    #[test]
    fn a_text_that_is_a_credential_is_refused_without_echoing_it() {
        let fields = vec![field("note", ElicitationFieldType::String, false)];
        let key = "sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let err = validate(
            &fields,
            json!({"note": key}).as_object().unwrap().clone(),
            None,
        )
        .unwrap_err();
        assert!(!err.contains(key), "{err}");
        let labelled = validate(
            &fields,
            json!({"note": "my password: Zx9!kQ2mPw7"})
                .as_object()
                .unwrap()
                .clone(),
            None,
        );
        assert!(labelled.is_err());
        // A value the app knows as a secret is refused too.
        let redactor = crate::SharedRedactor::default();
        redactor.register("vault-secret-value-31337");
        let known = validate(
            &fields,
            json!({"note": "x vault-secret-value-31337"})
                .as_object()
                .unwrap()
                .clone(),
            Some(&redactor),
        );
        assert!(known.is_err());
        assert!(
            validate(
                &fields,
                json!({"note": "Springfield"}).as_object().unwrap().clone(),
                Some(&redactor)
            )
            .is_ok()
        );
    }

    #[test]
    fn a_text_field_without_a_max_length_still_has_one() {
        let fields = vec![field("note", ElicitationFieldType::String, false)];
        let long = "a".repeat(2001);
        assert!(
            validate(
                &fields,
                json!({"note": long}).as_object().unwrap().clone(),
                None
            )
            .is_err()
        );
        let ok = "a".repeat(2000);
        assert!(
            validate(
                &fields,
                json!({"note": ok}).as_object().unwrap().clone(),
                None
            )
            .is_ok()
        );
    }
}

//! MCP form limits, schema projection, and per-connection tool attribution.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use protocol::elicitation::{
    ElicitationField, ElicitationFieldKind as Kind, ElicitationFormat as Format, ElicitationOption,
    MAX_FIELDS, MAX_VALUE_BYTES,
};
use rmcp::model::{ElicitRequestParams, ElicitationSchema};
use serde_json::Value;
use tokio::{sync::watch, time::Instant};
use tool_registry::elicitation::{Decision, ElicitationHandler, Form};

const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_REQUESTS_PER_WINDOW: usize = 4;
const MAX_OPTIONS: usize = 20;
const MAX_MESSAGE_BYTES: usize = 1024;
const MAX_LABEL_BYTES: usize = 160;
const MAX_DESCRIPTION_BYTES: usize = 512;

/// One server's budget, shared across reconnects.
#[derive(Default)]
pub(crate) struct Rate(Mutex<VecDeque<Instant>>);

impl Rate {
    pub(crate) fn allow(&self) -> bool {
        let now = Instant::now();
        let mut recent = self.0.lock();
        while recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= MAX_REQUESTS_PER_WINDOW {
            return false;
        }
        recent.push_back(now);
        true
    }
}

pub(crate) struct Call {
    pub(crate) handler: Option<ElicitationHandler>,
    pub(crate) requested: AtomicBool,
    ambiguous: AtomicBool,
    asking: watch::Sender<bool>,
    pub(crate) done: watch::Sender<bool>,
    pub(crate) outcome: Mutex<Option<Outcome>>,
}

pub(crate) enum Outcome {
    Accepted,
    Declined,
    Cancelled,
    Parked(String),
}

impl Outcome {
    pub(crate) fn message(self, server: &str) -> String {
        match self {
            Self::Accepted => format!("The owner answered the form from MCP server {server}."),
            Self::Declined => format!("The form from MCP server {server} was declined."),
            Self::Cancelled => format!("The form from MCP server {server} was cancelled."),
            Self::Parked(reason) => reason,
        }
    }
}

impl Call {
    pub(crate) fn new(handler: Option<ElicitationHandler>) -> Self {
        Self {
            handler,
            requested: AtomicBool::new(false),
            ambiguous: AtomicBool::new(false),
            asking: watch::Sender::new(false),
            done: watch::Sender::new(false),
            outcome: Mutex::new(None),
        }
    }

    pub(crate) async fn ask(&self, handler: &ElicitationHandler, form: Form) -> Decision {
        self.asking.send_replace(true);
        let _waiting = Waiting(&self.asking);
        // Guard the bridge even when a host callback fails to enforce a deadline.
        tokio::time::timeout(Duration::from_secs(15 * 60 + 5), handler.ask(form))
            .await
            .unwrap_or(Decision::Cancel)
    }

    /// Count tool execution time while the server is working, excluding the
    /// owner's bounded decision window. The watch transition also wakes a tool
    /// whose normal deadline would fall while the card is open.
    pub(crate) async fn timeout<T>(
        &self,
        mut remaining: Duration,
        work: impl Future<Output = T>,
    ) -> Option<T> {
        let mut waiting = self.asking.subscribe();
        tokio::pin!(work);
        loop {
            let paused = *waiting.borrow_and_update();
            let start = Instant::now();
            let timer = tokio::time::sleep(remaining);
            tokio::pin!(timer);
            tokio::select! {
                biased;
                result = &mut work => return Some(result),
                _ = waiting.changed() => {}
                () = &mut timer, if !paused => {
                    return None;
                }
            }
            if !paused {
                remaining = remaining.saturating_sub(start.elapsed());
            }
        }
    }
}

struct Waiting<'a>(&'a watch::Sender<bool>);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

#[derive(Default)]
pub(crate) struct InFlight(Mutex<Vec<Arc<Call>>>);

impl InFlight {
    pub(crate) fn enter(&self, call: Arc<Call>) -> Lease<'_> {
        call.done.send_replace(false);
        let mut active = self.0.lock();
        if !active.is_empty() {
            call.ambiguous.store(true, Ordering::Release);
            for other in &*active {
                other.ambiguous.store(true, Ordering::Release);
            }
        }
        active.push(Arc::clone(&call));
        Lease { calls: self, call }
    }

    pub(crate) fn take(&self, expected: Option<&Arc<Call>>) -> Option<Arc<Call>> {
        let calls = self.0.lock();
        let [only] = calls.as_slice() else {
            return None;
        };
        if only.handler.is_none()
            || only.ambiguous.load(Ordering::Acquire)
            || expected.is_some_and(|call| !Arc::ptr_eq(call, only))
            || only.requested.swap(true, Ordering::AcqRel)
        {
            return None;
        }
        Some(Arc::clone(only))
    }
}

pub(crate) struct Lease<'a> {
    calls: &'a InFlight,
    call: Arc<Call>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.calls.0.lock().retain(|c| !Arc::ptr_eq(c, &self.call));
        self.call.done.send_replace(true);
    }
}

pub(crate) fn project(server: &str, request: ElicitRequestParams) -> Option<Form> {
    let ElicitRequestParams::FormElicitationParams {
        message,
        requested_schema,
        ..
    } = request
    else {
        return None;
    };
    if !bounded_text(&message, MAX_MESSAGE_BYTES) {
        return None;
    }
    let block_all = looks_sensitive(&message);
    let fields = fields(requested_schema, block_all)?;
    Some(Form {
        server: server.to_owned(),
        message,
        fields,
    })
}

fn bounded_text(text: &str, max: usize) -> bool {
    text.len() <= max
        && !protocol::approval_text::hides_characters(text)
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

fn looks_sensitive(text: &str) -> bool {
    let key = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    [
        "password",
        "passwd",
        "passcode",
        "passphrase",
        "secret",
        "apikey",
        "accesskey",
        "privatekey",
        "token",
        "bearer",
        "credential",
        "sessioncookie",
        "sessionid",
        "authcode",
        "otp",
        "2fa",
        "mfa",
        "verificationcode",
        "securitycode",
        "recoverycode",
        "onetimecode",
        "cardnumber",
        "creditcard",
        "cvv",
        "cvc",
        "pinnumber",
        "parola",
        "şifre",
        "apianahtarı",
        "erişimanahtarı",
        "girişbilgisi",
        "doğrulamakodu",
        "kartnumarası",
    ]
    .iter()
    .any(|word| key.contains(word))
        || matches!(key.as_str(), "pin" | "pwd")
        || !secret_store::detect_secrets(text).is_empty()
}

fn fields(schema: ElicitationSchema, block_all: bool) -> Option<Vec<ElicitationField>> {
    let required = schema.required.unwrap_or_default();
    if schema.properties.len() > MAX_FIELDS
        || required.len() > MAX_FIELDS
        || required
            .iter()
            .any(|name| !schema.properties.contains_key(name))
    {
        return None;
    }
    let names = schema
        .property_order
        .unwrap_or_else(|| schema.properties.keys().cloned().collect());
    names
        .into_iter()
        .map(|name| {
            let field = serde_json::to_value(schema.properties.get(&name)?).ok()?;
            let is_required = required.contains(&name);
            project_field(name, &field, is_required, block_all)
        })
        .collect()
}

fn project_field(
    name: String,
    field: &Value,
    required: bool,
    block_all: bool,
) -> Option<ElicitationField> {
    if name.is_empty() || !bounded_text(&name, MAX_LABEL_BYTES) {
        return None;
    }
    let label = field["title"].as_str().unwrap_or(&name);
    let description = field["description"].as_str();
    if !bounded_text(label, MAX_LABEL_BYTES)
        || description.is_some_and(|s| !bounded_text(s, MAX_DESCRIPTION_BYTES))
    {
        return None;
    }
    let options = options(field)?;
    let kind = match field["type"].as_str()? {
        "string" if !options.is_empty() => Kind::Enum,
        "string" => Kind::String,
        "number" => Kind::Number,
        "integer" => Kind::Integer,
        "boolean" => Kind::Boolean,
        _ => return None,
    };
    let blocked = block_all
        || looks_sensitive(&name)
        || looks_sensitive(label)
        || description.is_some_and(looks_sensitive)
        || options
            .iter()
            .any(|o| looks_sensitive(&o.value) || looks_sensitive(&o.label));
    let min_length = field["minLength"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok());
    let max_length = field["maxLength"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok());
    let minimum = field["minimum"].as_f64();
    let maximum = field["maximum"].as_f64();
    if min_length
        .is_some_and(|n| n as usize > MAX_VALUE_BYTES || max_length.is_some_and(|max| n > max))
        || minimum.zip(maximum).is_some_and(|(min, max)| min > max)
    {
        return None;
    }
    let format = match field["format"].as_str() {
        Some("email") => Some(Format::Email),
        Some("uri") => Some(Format::Uri),
        Some("date") => Some(Format::Date),
        Some("date-time") => Some(Format::DateTime),
        Some(_) => return None,
        None => None,
    };
    Some(ElicitationField {
        label: label.to_owned(),
        name,
        description: description.filter(|_| !blocked).map(str::to_owned),
        required,
        blocked,
        kind,
        min_length,
        max_length,
        format,
        minimum,
        maximum,
        options: if blocked { Vec::new() } else { options },
    })
}

fn options(field: &Value) -> Option<Vec<ElicitationOption>> {
    let options: Vec<ElicitationOption> = if let Some(values) = field["enum"].as_array() {
        if values.is_empty() || values.len() > MAX_OPTIONS {
            return None;
        }
        let labels = field["enumNames"].as_array();
        if labels.is_some_and(|l| l.len() != values.len()) {
            return None;
        }
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value.as_str()?;
                let label = labels.and_then(|l| l[index].as_str()).unwrap_or(value);
                Some(ElicitationOption {
                    value: value.into(),
                    label: label.into(),
                })
            })
            .collect::<Option<_>>()?
    } else if let Some(values) = field["oneOf"].as_array() {
        if values.is_empty() || values.len() > MAX_OPTIONS {
            return None;
        }
        values
            .iter()
            .map(|option| {
                Some(ElicitationOption {
                    value: option["const"].as_str()?.into(),
                    label: option["title"].as_str()?.into(),
                })
            })
            .collect::<Option<_>>()?
    } else {
        Vec::new()
    };
    options
        .iter()
        .enumerate()
        .all(|(i, o)| {
            bounded_text(&o.value, MAX_LABEL_BYTES)
                && bounded_text(&o.label, MAX_LABEL_BYTES)
                && !options[..i].iter().any(|other| other.value == o.value)
        })
        .then_some(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsolicited_and_overlapping_calls_cannot_take_a_form() {
        let calls = InFlight::default();
        let call = || {
            Arc::new(Call::new(Some(ElicitationHandler::new(|_| async {
                Decision::Decline
            }))))
        };
        assert!(calls.take(None).is_none());
        let a = call();
        let first = calls.enter(Arc::clone(&a));
        let b = call();
        let second = calls.enter(b);
        assert!(calls.take(None).is_none());
        drop(second);
        assert!(
            calls.take(None).is_none(),
            "a late request may belong to either call"
        );
        drop(first);
        let c = call();
        let _fresh = calls.enter(Arc::clone(&c));
        assert!(calls.take(Some(&a)).is_none());
        assert!(calls.take(Some(&c)).is_some());
        assert!(calls.take(None).is_none(), "one form per tool call");
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_recovers_its_rate_budget_after_the_window() {
        let rate = Rate::default();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(rate.allow());
        }
        assert!(!rate.allow());
        tokio::time::advance(RATE_WINDOW).await;
        assert!(rate.allow());
    }

    #[test]
    fn schema_bounds_and_plain_text_are_checked() {
        let request = |properties: Value| {
            serde_json::from_value(serde_json::json!({
                "message": "<b>Choose details</b>",
                "requestedSchema": {"type": "object", "properties": properties}
            }))
            .unwrap()
        };
        let form = project(
            "calendar",
            request(serde_json::json!({
                "region": {"type": "string", "oneOf": [{"const": "eu", "title": "Europe"}]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10},
                "start": {"type": "string", "format": "date"},
                "code": {"type": "number", "description": "Doğrulama kodu"},
            })),
        )
        .unwrap();
        assert_eq!(form.message, "<b>Choose details</b>");
        assert!(form.fields.iter().any(|f| f.name == "code" && f.blocked));
        let region = form.fields.iter().find(|f| f.name == "region").unwrap();
        assert_eq!(region.options[0].label, "Europe");
        assert!(
            project(
                "calendar",
                request(serde_json::json!({
                    "large": {"type": "string", "title": "x".repeat(MAX_LABEL_BYTES + 1)}
                }))
            )
            .is_none()
        );
    }
}

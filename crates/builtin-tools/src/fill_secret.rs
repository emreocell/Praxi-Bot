//! Shared pieces of the `secret` parameter of `browser_fill`, `desktop_type`
//! and `desktop_set_value`, and the `secrets_list` tool.
//!
//! The model names an owner-stored secret; it never holds the value. The value
//! is read from the store only inside the dispatch (after the approval, after
//! the live-field check), lives in memory only and is wiped on drop. Every
//! string that leaves this module toward the model, an event or a log is first
//! scrubbed of the value and its encoded forms.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use secret_store::{
    FillError, FillSecrets, Redactor, SecretValue, sanitize_label, secret_variants,
};
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::spec;

/// Name of the model-facing tool that lists secret names and allowed places.
pub const SECRETS_LIST: &str = "secrets_list";

/// Longest secret name / origin shown to the model or on a card.
const SHOWN_CAP: usize = 80;
/// Most secrets `secrets_list` shows.
const LIST_CAP: usize = 50;

/// What a fill tool was asked to type.
pub(crate) enum Source {
    /// Ordinary text: shown to the user exactly as the model wrote it, never masked.
    Text(String),
    /// The NAME of an owner-stored secret.
    Secret(String),
}

/// Exactly one of `text_key` | `secret` must be present.
pub(crate) fn source(input: &Value, text_key: &str) -> Result<Source, ToolError> {
    let present = |k: &str| input.get(k).filter(|v| !v.is_null());
    match (present(text_key), present("secret")) {
        (Some(_), Some(_)) => Err(ToolError::InvalidInput(format!(
            "give exactly one of `{text_key}` or `secret`, not both"
        ))),
        (None, None) => Err(ToolError::InvalidInput(format!(
            "give exactly one of `{text_key}` or `secret`"
        ))),
        (Some(Value::String(t)), None) => Ok(Source::Text(t.clone())),
        (None, Some(Value::String(n))) if !n.is_empty() => Ok(Source::Secret(n.clone())),
        _ => Err(ToolError::InvalidInput(format!(
            "`{text_key}` and `secret` must be non-empty strings"
        ))),
    }
}

/// Whether `input` asks to type a secret (the per-call always-ask test).
pub(crate) fn names_a_secret(input: &Value) -> bool {
    input.get("secret").is_some_and(|v| !v.is_null())
}

/// `text` without the value or any encoded form of it.
pub(crate) fn scrub(text: &str, value: &SecretValue) -> String {
    let mut r = Redactor::new();
    for v in secret_variants(value.expose()) {
        r.add_secret(&v);
    }
    r.redact(text)
}

/// A refusal as a tool error. `FillError` messages carry names and origins only.
pub(crate) fn refused(e: &FillError) -> ToolError {
    match e {
        FillError::InvalidName(_)
        | FillError::Reserved(_)
        | FillError::InvalidEntry(_)
        | FillError::Invalid(_)
        | FillError::Exists(_) => ToolError::InvalidInput(e.to_string()),
        _ => ToolError::Execution(e.to_string()),
    }
}

/// `secrets_list`: the owner-chosen secret names and where each may be typed.
/// Values are never read. Output is capped, single-line per entry, and sorted,
/// so it is deterministic.
struct SecretsList {
    spec: ToolSpec,
    secrets: Arc<FillSecrets>,
}

/// The `secrets_list` tool over `secrets`.
#[must_use]
pub fn secrets_list_tool(secrets: Arc<FillSecrets>) -> Arc<dyn Tool> {
    Arc::new(SecretsList {
        spec: spec(
            SECRETS_LIST,
            "List the secrets the owner stored for you to type: each name with the web origins and programs it may be typed into. You never see a value. To type one, pass its name as `secret` (instead of `text`) to browser_fill, desktop_type or desktop_set_value. Example: {}",
            json!({"type":"object","properties":{}}),
            Effect::Read,
            PermissionCategory::Meta,
        ),
        secrets,
    })
}

#[async_trait]
impl Tool for SecretsList {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, _input: Value) -> Result<ToolOutput, ToolError> {
        // A bot sees the owner's secrets and its own accounts, not another bot's.
        let list = self
            .secrets
            .list_for_bot(ctx.bot_id.as_deref())
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        if list.is_empty() {
            return Ok(ToolOutput::text(
                "The owner has stored no secrets for you to type. Ask the owner to add one in Settings; never ask for the value in chat.",
            ));
        }
        let mut out = String::from(
            "Secrets you may type (pass the name as `secret`; the value is never shown to you):\n",
        );
        for s in list.iter().take(LIST_CAP) {
            let places: Vec<String> = s
                .origins
                .iter()
                .map(|o| sanitize_label(o, SHOWN_CAP))
                .chain(
                    s.programs
                        .iter()
                        .map(|p| format!("program {}", sanitize_label(p, SHOWN_CAP))),
                )
                .collect();
            let _ = writeln!(
                out,
                "- {}{}: {}",
                sanitize_label(&s.name, SHOWN_CAP),
                if s.has_value {
                    ""
                } else {
                    " (no value stored)"
                },
                if places.is_empty() {
                    "nowhere yet (the owner has not allowed any place)".to_owned()
                } else {
                    places.join(", ")
                }
            );
        }
        if list.len() > LIST_CAP {
            let _ = writeln!(out, "... and {} more", list.len() - LIST_CAP);
        }
        Ok(ToolOutput::text(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_of_text_or_secret() {
        let k = "text";
        assert!(matches!(
            source(&json!({"text":"a"}), k),
            Ok(Source::Text(_))
        ));
        assert!(matches!(
            source(&json!({"secret":"login/x"}), k),
            Ok(Source::Secret(_))
        ));
        for bad in [
            json!({}),
            json!({"text":"a","secret":"b"}),
            json!({"secret":""}),
            json!({"secret":5}),
            json!({"text":5}),
        ] {
            assert!(source(&bad, k).is_err(), "{bad}");
        }
        // JSON null counts as absent.
        assert!(matches!(
            source(&json!({"text":null,"secret":"x"}), k),
            Ok(Source::Secret(_))
        ));
    }

    #[test]
    fn scrub_removes_the_value_and_its_encodings() {
        let v = SecretValue::new("p@ss w0rd/1");
        let s = format!(
            "a {} b {} c {}",
            v.expose(),
            serde_json::to_string(v.expose()).unwrap(),
            "p%40ss%20w0rd%2F1"
        );
        let out = scrub(&s, &v);
        assert!(!out.contains("w0rd"), "{out}");
    }
}

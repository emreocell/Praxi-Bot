//! `secret_request`: the model asks the OWNER for a credential through a card
//! the app draws, and the value goes straight into the vault.
//!
//! This tool never sees, receives or returns a value. It validates the request
//! (a `fill/` entry name, a short plain-text purpose, which fields) and returns
//! a card description the agent loop turns into a parked request. The owner's
//! app posts the value to the daemon's owner-only route, which writes it to the
//! store; the model only ever learns `stored` or `cancelled` and the name.
//! See `docs/design/secret-request.md`.

use std::sync::Arc;

use async_trait::async_trait;
use secret_store::{FillSecrets, validate_fill_name};
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::spec;

/// Name of the model-facing tool.
pub const SECRET_REQUEST: &str = "secret_request";

/// `kind` of the JSON part the tool returns; the loop parks the run on it.
pub const SECRET_REQUEST_CARD_KIND: &str = "secret_request_card";

/// Longest purpose text, in characters.
pub const PURPOSE_CAP: usize = 200;

/// Longest entry name for a `login` request (`<name>/username` must still fit).
const LOGIN_NAME_CAP: usize = 50;

/// What the request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fields {
    /// One secret value.
    Secret,
    /// A username and a password.
    Login,
}

impl Fields {
    /// The wire name (`secret` | `login`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Login => "login",
        }
    }
}

/// A validated request, as the loop reads it back from the card part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretCard {
    /// Entry name (the part after `fill/`).
    pub name: String,
    /// Plain text, single line, at most [`PURPOSE_CAP`] characters.
    pub purpose: String,
    /// What to ask for.
    pub fields: Fields,
    /// The bot asked to replace an existing entry.
    pub replace: bool,
}

impl SecretCard {
    /// Read the card part [`secret_request_tool`] produced. `None` for any other JSON.
    #[must_use]
    pub fn from_part(data: &Value) -> Option<Self> {
        Some(Self {
            name: data.get("name")?.as_str()?.to_owned(),
            purpose: data.get("purpose")?.as_str()?.to_owned(),
            fields: match data.get("fields")?.as_str()? {
                "secret" => Fields::Secret,
                "login" => Fields::Login,
                _ => return None,
            },
            replace: data.get("replace")?.as_bool()?,
        })
    }
}

/// The store names an entry of `fields` occupies.
#[must_use]
pub fn entry_names(name: &str, fields: Fields) -> Vec<String> {
    match fields {
        Fields::Secret => vec![name.to_owned()],
        Fields::Login => vec![format!("{name}/username"), format!("{name}/password")],
    }
}

struct SecretRequest {
    spec: ToolSpec,
    secrets: Arc<FillSecrets>,
}

/// The `secret_request` tool over `secrets` (used only to see whether the name
/// is taken; no value is ever read).
#[must_use]
pub fn secret_request_tool(secrets: Arc<FillSecrets>) -> Arc<dyn Tool> {
    Arc::new(SecretRequest {
        spec: spec(
            SECRET_REQUEST,
            "Ask the owner to enter a credential in a form the app shows. The value goes straight into the owner's vault; you never see it, so NEVER ask for it in chat. Use it only when the owner's task needs a login or key that `secrets_list` does not have. `name` is the entry name (letters, digits, . _ - and /), `purpose` is one short plain-text sentence shown to the owner (no links or markup), `fields` is `secret` (one value) or `login` (username + password). If the name exists, pass `replace`:true to ask for a new value. Returns `stored` or `cancelled`; afterwards use `secrets_list` and the `secret` parameter of browser_fill. A request is refused outside the owner's own desktop session. Example: {\"name\":\"login/example\",\"purpose\":\"Sign in to example.com to download the invoice\",\"fields\":\"login\"}",
            json!({"type":"object","properties":{
                "name":{"type":"string","description":"Entry name, for example login/example."},
                "purpose":{"type":"string","maxLength":PURPOSE_CAP,"description":"One short plain-text sentence for the owner."},
                "fields":{"type":"string","enum":["secret","login"],"description":"One value, or username + password (default secret)."},
                "replace":{"type":"boolean","description":"Ask to replace an existing entry (default false)."}
            },"required":["name","purpose"]}),
            Effect::Write,
            PermissionCategory::Meta,
        ),
        secrets,
    })
}

/// Reject text that could pass as a link or markup on the owner's card.
fn plain_purpose(raw: &str) -> Result<String, ToolError> {
    let bad = |why: &str| ToolError::InvalidInput(format!("`purpose` {why}"));
    let text = secret_store::sanitize_label(raw, usize::MAX);
    if text.is_empty() {
        return Err(bad("must not be empty"));
    }
    if text.chars().count() > PURPOSE_CAP {
        return Err(bad(&format!("must be at most {PURPOSE_CAP} characters")));
    }
    let lower = text.to_ascii_lowercase();
    if text.contains(['<', '>', '`', '[', ']', '*', '_', '|'])
        || lower.contains("://")
        || lower.contains("www.")
        || lower.contains("mailto:")
    {
        return Err(bad(
            "must be plain text: no links, markup or formatting characters",
        ));
    }
    Ok(text)
}

#[async_trait]
impl Tool for SecretRequest {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidInput("`name` must be a string".into()))?;
        validate_fill_name(name).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let purpose = plain_purpose(
            input
                .get("purpose")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidInput("`purpose` must be a string".into()))?,
        )?;
        let fields = match input.get("fields").and_then(Value::as_str) {
            None | Some("secret") => Fields::Secret,
            Some("login") => Fields::Login,
            Some(_) => {
                return Err(ToolError::InvalidInput(
                    "`fields` must be `secret` or `login`".into(),
                ));
            }
        };
        if fields == Fields::Login && name.len() > LOGIN_NAME_CAP {
            return Err(ToolError::InvalidInput(format!(
                "a `login` name must be at most {LOGIN_NAME_CAP} bytes"
            )));
        }
        let replace = match input.get("replace") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                return Err(ToolError::InvalidInput(
                    "`replace` must be a boolean".into(),
                ));
            }
        };
        let taken = entry_names(name, fields)
            .iter()
            .any(|n| self.secrets.exists(n));
        if taken && !replace {
            return Err(ToolError::InvalidInput(format!(
                "`{name}` already exists. Use it (see secrets_list), pick another name, or pass `replace`:true to ask the owner for a new value"
            )));
        }
        let card = json!({
            "kind": SECRET_REQUEST_CARD_KIND,
            "name": name,
            "purpose": purpose,
            "fields": fields.as_str(),
            "replace": replace,
        });
        // Not a result the model can use: the loop replaces it with the owner's
        // answer. Called anywhere else (a script, a bridge) it asks nobody.
        Ok(ToolOutput {
            content: vec![
                ContentPart::Json { data: card },
                ContentPart::Text {
                    text: "No request was made: only the agent loop can show the owner a form."
                        .into(),
                },
            ],
            spill: None,
            exposure: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;

    fn tool() -> (Arc<FillSecrets>, Arc<dyn Tool>) {
        let s = Arc::new(FillSecrets::in_memory(Arc::new(MemoryStore::new())));
        (Arc::clone(&s), secret_request_tool(s))
    }

    async fn run(t: &Arc<dyn Tool>, v: Value) -> Result<ToolOutput, ToolError> {
        t.call(&ToolContext::default(), v).await
    }

    #[tokio::test]
    async fn valid_request_returns_a_card_and_no_value() {
        let (_s, t) = tool();
        let out = run(
            &t,
            json!({"name":"login/example","purpose":"Sign in to download the invoice","fields":"login"}),
        )
        .await
        .unwrap();
        let ContentPart::Json { data } = &out.content[0] else {
            panic!("card part first");
        };
        let card = SecretCard::from_part(data).unwrap();
        assert_eq!(card.name, "login/example");
        assert_eq!(card.fields, Fields::Login);
        assert!(!card.replace);
    }

    #[tokio::test]
    async fn refuses_reserved_names_markup_and_silent_overwrite() {
        let (s, t) = tool();
        for bad in [
            json!({"name":"composio/gmail","purpose":"x"}),
            json!({"name":"mcp/x","purpose":"x"}),
            json!({"name":"login/x","purpose":"see https://evil.example"}),
            json!({"name":"login/x","purpose":"<b>bank</b>"}),
            json!({"name":"login/x","purpose":"a".repeat(PURPOSE_CAP + 1)}),
            json!({"name":"login/x","purpose":"x","fields":"card"}),
            json!({"name":"login/x","purpose":""}),
        ] {
            assert!(run(&t, bad.clone()).await.is_err(), "{bad}");
        }
        s.upsert("login/taken", Some("existing-value-1"), None, None)
            .unwrap();
        let err = run(&t, json!({"name":"login/taken","purpose":"x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("replace"), "{err}");
        assert!(
            run(
                &t,
                json!({"name":"login/taken","purpose":"x","replace":true})
            )
            .await
            .is_ok()
        );
    }
}

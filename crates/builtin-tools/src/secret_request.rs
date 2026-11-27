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
use secret_store::{FillSecrets, canonical_origin, validate_fill_name};
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
    /// Canonical exact HTTPS origin the owner is asked to authorize.
    pub origin: String,
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
            origin: data.get("origin")?.as_str()?.to_owned(),
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
            "Use this form whenever the owner's task needs an existing login, API key or backup code for a browser origin that secrets_list does not have. Never ask for credentials in chat or request payment credentials. The owner sees the exact HTTPS origin and a short plain-text purpose. `fields` is `secret` (one value) or `login` (username + password). If the existing entry is scoped to this bot and origin, pass `replace`:true to ask for a new value. After `stored`, use secrets_list and browser_fill's `secret` parameter. A request requires the owner's local desktop. Example: {\"name\":\"login/example\",\"origin\":\"https://example.com\",\"purpose\":\"Sign in to download the invoice\",\"fields\":\"login\"}",
            json!({"type":"object","additionalProperties":false,"properties":{
                "name":{"type":"string","description":"Entry name, for example login/example."},
                "origin":{"type":"string","description":"The exact HTTPS origin of the field, such as https://example.com, with no path, query, fragment or wildcard."},
                "purpose":{"type":"string","maxLength":PURPOSE_CAP,"description":"One short plain-text sentence for the owner."},
                "fields":{"type":"string","enum":["secret","login"],"description":"One value, or username + password (default secret)."},
                "replace":{"type":"boolean","description":"Ask to replace an existing entry (default false)."}
            },"required":["name","origin","purpose"]}),
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

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        if input.as_object().is_none_or(|object| {
            object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "name" | "origin" | "purpose" | "fields" | "replace"
                )
            })
        }) {
            return Err(ToolError::InvalidInput(
                "only `name`, `origin`, `purpose`, `fields` and `replace` are accepted; never send a credential to this tool".into(),
            ));
        }
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidInput("`name` must be a string".into()))?;
        validate_fill_name(name).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let origin = input
            .get("origin")
            .and_then(Value::as_str)
            .and_then(|raw| canonical_origin(raw).ok())
            .filter(|origin| origin.starts_with("https://"))
            .ok_or_else(|| {
                ToolError::InvalidInput("`origin` must be an exact HTTPS origin".into())
            })?;
        let purpose = plain_purpose(
            input
                .get("purpose")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidInput("`purpose` must be a string".into()))?,
        )?;
        let fields = match input.get("fields") {
            None => Fields::Secret,
            Some(Value::String(kind)) if kind == "secret" => Fields::Secret,
            Some(Value::String(kind)) if kind == "login" => Fields::Login,
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
        // Only an entry this bot may use counts as taken. One it may not use (another
        // bot's account, an owner-made or owner-granted entry) answers exactly like a
        // free name, so the name cannot be probed; the submit route refuses the write
        // to the owner under the write lock, and nothing of that entry is ever changed.
        let taken = entry_names(name, fields)
            .iter()
            .any(|n| self.secrets.exists_for_bot(n, ctx.bot_id.as_deref()));
        if taken && !replace {
            return Err(ToolError::InvalidInput(format!(
                "`{name}` already exists. Use it (see secrets_list), pick another name, or pass `replace`:true to ask the owner for a new value"
            )));
        }
        // The scope check at the root: the owner is only ever asked to REPLACE an
        // entry this bot may use. For a name this bot may not use (another bot's
        // account, an owner-made entry) the request is an ordinary new-value
        // form, exactly like a free name, so bot B can never put "replace bot A's
        // account" in front of the owner. The submit route and the write refuse it
        // again under the lock.
        let card = json!({
            "kind": SECRET_REQUEST_CARD_KIND,
            "name": name,
            "origin": origin,
            "purpose": purpose,
            "fields": fields.as_str(),
            "replace": replace && taken,
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
            json!({"name":"login/example","origin":"HTTPS://Example.com:443/","purpose":"Sign in to download the invoice","fields":"login"}),
        )
        .await
        .unwrap();
        let ContentPart::Json { data } = &out.content[0] else {
            panic!("card part first");
        };
        let card = SecretCard::from_part(data).unwrap();
        assert_eq!(card.name, "login/example");
        assert_eq!(card.fields, Fields::Login);
        assert_eq!(card.origin, "https://example.com");
        assert!(!card.replace);
    }

    #[tokio::test]
    async fn rejects_missing_non_https_and_non_origin_destinations() {
        let (_s, tool) = tool();
        let mut input = json!({"name":"login/example","purpose":"Sign in"});
        assert!(run(&tool, input.clone()).await.is_err());
        for bad in [
            "http://example.com",
            "https://example.com/reset",
            "https://example.com?key=value",
            "https://value@example.com",
            "https://*.example.com",
        ] {
            input["origin"] = json!(bad);
            let error = run(&tool, input.clone()).await.unwrap_err().to_string();
            assert!(error.contains("exact HTTPS origin"));
            assert!(!error.contains(bad));
        }
        input["origin"] = json!("https://example.com");
        input["value"] = json!("canary-input-7842");
        let error = run(&tool, input).await.unwrap_err().to_string();
        assert!(!error.contains("canary-input-7842"));
    }

    #[tokio::test]
    async fn refuses_reserved_names_markup_and_silent_overwrite() {
        let (s, t) = tool();
        for bad in [
            json!({"name":"composio/gmail","origin":"https://example.com","purpose":"x"}),
            json!({"name":"mcp/x","origin":"https://example.com","purpose":"x"}),
            json!({"name":"login/x","origin":"https://example.com","purpose":"see https://evil.example"}),
            json!({"name":"login/x","origin":"https://example.com","purpose":"<b>bank</b>"}),
            json!({"name":"login/x","origin":"https://example.com","purpose":"a".repeat(PURPOSE_CAP + 1)}),
            json!({"name":"login/x","origin":"https://example.com","purpose":"x","fields":"card"}),
            json!({"name":"login/x","origin":"https://example.com","purpose":"x","fields":true}),
            json!({"name":"login/x","origin":"https://example.com","purpose":""}),
        ] {
            assert!(run(&t, bad.clone()).await.is_err(), "{bad}");
        }
        s.upsert_with(
            "login/taken",
            secret_store::FillSecretUpdate {
                value: Some("existing-value-1"),
                bot_ids: Some(&["bot-a".to_owned()]),
                ..secret_store::FillSecretUpdate::default()
            },
        )
        .unwrap();
        let ctx = ToolContext {
            bot_id: Some("bot-a".into()),
            ..ToolContext::default()
        };
        let err = t
            .call(
                &ctx,
                json!({"name":"login/taken","origin":"https://example.com","purpose":"x"}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("replace"), "{err}");
        assert!(
            t.call(
                &ctx,
                json!({"name":"login/taken","origin":"https://example.com","purpose":"x","replace":true})
            )
            .await
            .is_ok()
        );
    }

    /// Bot B must not learn that bot A's account or an owner-made entry exists: both
    /// answer like a free name (a card; the submit route refuses the write to the
    /// owner). The bot that may use an entry still gets the "exists" hint.
    #[tokio::test]
    async fn an_entry_a_bot_may_not_use_answers_like_a_free_name() {
        let (s, t) = tool();
        s.register_account(&secret_store::NewAccount {
            name: "login/example.com-aaaa1111",
            site: "https://example.com",
            login_id: "me",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            label: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
        s.upsert_with(
            "login/owner-made",
            secret_store::FillSecretUpdate {
                value: Some("x"),
                bot_ids: Some(&["bot-a".to_owned()]),
                ..secret_store::FillSecretUpdate::default()
            },
        )
        .unwrap();
        let ctx = |bot: Option<&str>| ToolContext {
            bot_id: bot.map(str::to_owned),
            ..ToolContext::default()
        };
        let ask = |name: &str, replace: bool| json!({"name": name, "origin":"https://example.com","purpose": "Sign in", "replace": replace});
        let card = |out: Result<ToolOutput, ToolError>| {
            let out = out.expect("a card, like for a free name");
            let ContentPart::Json { data } = &out.content[0] else {
                panic!("card part first");
            };
            SecretCard::from_part(data).unwrap()
        };
        for bot in [Some("bot-b"), None] {
            for name in [
                "login/example.com-aaaa1111",
                "login/owner-made",
                "login/never-existed",
            ] {
                for replace in [true, false] {
                    let c = card(t.call(&ctx(bot), ask(name, replace)).await);
                    // Never a "replace" card for an entry this bot may not use.
                    assert_eq!((c.name.as_str(), c.replace), (name, false), "{bot:?}");
                }
            }
        }
        // The bot that may use the entry is told it exists, and may ask to replace.
        for name in ["login/example.com-aaaa1111", "login/owner-made"] {
            let err = t
                .call(&ctx(Some("bot-a")), ask(name, false))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("replace"), "{err}");
            let c = card(t.call(&ctx(Some("bot-a")), ask(name, true)).await);
            assert!(c.replace, "the owning bot may ask to replace its own entry");
        }
    }
}

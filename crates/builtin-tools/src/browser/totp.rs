//! `account_totp_save`: the two-factor setup key a site shows for an account THIS bot
//! registered goes from the page element straight into the secret store. The model
//! points at the element (`ref` from `browser_snapshot`, or a CSS `selector`) and gets
//! a fixed sentence back: it never holds the key, so the key is never in a tool input,
//! a card, a log, a result or a page snapshot it read (see [`secret_store::mask_setup_secrets`]
//! for the part of the page text that shows a key before it is saved).
//!
//! The same discipline as a blind fill, run the other way: the browser reports the origin
//! of the frame that holds the element and of the top page (twice, with the document
//! unchanged in between), both must be the account's own site, and only then is the element
//! read. The element stays masked in later snapshots until the page navigates. Typing the
//! codes later is `browser_fill {secret: "totp:<name>"}`.

use std::sync::Arc;

use browser_cdp::ReadTarget;
use secret_store::{FillSecrets, SecretValue, TotpSpec, sanitize_label};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{ToolContext, ToolError, ToolOutput, ToolSpec};

use super::{BrowserTool, Op, Shared, origin_of};
use crate::fill_secret::refused;
use crate::util::{exec_err, parse, spec};

/// Name of the tool.
pub const ACCOUNT_TOTP_SAVE: &str = "account_totp_save";

const SHOWN_CAP: usize = 80;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TotpSaveIn {
    name: String,
    #[serde(default)]
    r#ref: Option<String>,
    #[serde(default)]
    selector: Option<String>,
}

impl TotpSaveIn {
    fn target(&self) -> Result<ReadTarget<'_>, ToolError> {
        match (
            self.r#ref.as_deref().map(str::trim).filter(|r| !r.is_empty()),
            self.selector
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty()),
        ) {
            (Some(r), None) => Ok(ReadTarget::Ref(r)),
            (None, Some(s)) => Ok(ReadTarget::Selector(s)),
            _ => Err(ToolError::InvalidInput(
                "give exactly one of `ref` (the element that holds the setup key, from browser_snapshot) or `selector` (CSS, top page only). Never type the key into this call".into(),
            )),
        }
    }
}

/// The tool's spec: `ref` / `selector` point at the page element, never a key.
pub(super) fn tool_spec() -> ToolSpec {
    spec(
        ACCOUNT_TOTP_SAVE,
        "Keep the two-factor (authenticator app) setup key a site shows for an account YOU made with account_register. Point `ref` (from browser_snapshot) or `selector` at the element that holds the key or its otpauth:// address: Praxi reads it itself and keeps it in the owner's vault, limited to that site and to you. You never see or type the key (it is masked in snapshots): never copy it into any call. Then type each code with browser_fill `secret` \"totp:<name>\". Only your own accounts, never the owner's; for an SMS or e-mail code, a phone number or a CAPTCHA, hand over to the owner. Example: {\"name\":\"login/example.com-1a2b3c4d\",\"ref\":\"e12\"}",
        json!({"type":"object","properties":{
            "name":{"type":"string","description":"Secret name of the account, from account_list."},
            "ref":{"type":"string","description":"Element ref from browser_snapshot, like e12."},
            "selector":{"type":"string","description":"CSS selector in the top page, instead of ref."}
        },"required":["name"]}),
        tool_registry::Effect::Write,
        tool_registry::PermissionCategory::Meta,
    )
}

impl BrowserTool {
    /// The bot-made account `name` and the page the browser is on now, refused before any
    /// card for another bot's or an owner-made secret and for a page that is not the
    /// account's own site.
    async fn totp_save_facts(
        &self,
        ctx: &ToolContext,
        shared: &Arc<Shared>,
        i: &TotpSaveIn,
    ) -> Result<(secret_store::AccountInfo, String), ToolError> {
        let secrets = shared
            .fill_secrets
            .clone()
            .ok_or_else(|| exec_err("no secrets are configured on this daemon"))?;
        let bot = ctx.bot_id.as_deref();
        secrets.check_bot(&i.name, bot).map_err(|e| refused(&e))?;
        let info = secrets
            .accounts(bot)
            .map_err(|e| ToolError::Execution(e.to_string()))?
            .into_iter()
            .find(|a| a.name == i.name && a.created_by == "bot")
            .ok_or_else(|| {
                ToolError::InvalidInput("there is no account of yours with that name".into())
            })?;
        let top = shared
            .driver
            .current_url()
            .await
            .and_then(|u| origin_of(&u))
            .ok_or_else(|| exec_err("the browser did not report which site this page is on"))?;
        secrets
            .check_totp_save_page(&i.name, bot, Some(&top), Some(&top))
            .map_err(|e| refused(&e))?;
        Ok((info, top))
    }

    /// What the approval card of a save shows. Names the account, the site and the element,
    /// never the key (it is not read before the owner agrees).
    pub(super) async fn totp_save_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Value, ToolError> {
        let i: TotpSaveIn = parse(input.clone())?;
        i.target()?;
        let shared = self.select_shared(ctx)?;
        let (info, top) = self.totp_save_facts(ctx, &shared, &i).await?;
        let element = match (&i.r#ref, &i.selector) {
            (Some(r), _) => format!("element {}", sanitize_label(r, 40)),
            (_, Some(s)) => format!("selector {}", sanitize_label(s, 80)),
            _ => String::new(),
        };
        Ok(json!({
            "kind": "account_totp_save",
            "tool": ACCOUNT_TOTP_SAVE,
            "secret": sanitize_label(&info.name, SHOWN_CAP),
            "site": sanitize_label(&info.site, SHOWN_CAP),
            "login_id": sanitize_label(&info.login_id, SHOWN_CAP),
            "page": top,
            "element": element,
            "replaces_existing": info.has_totp,
            "note": format!("Praxi reads the two-factor key from {element} on {top} and keeps it in the vault for {} (login {}), limited to that site and this bot. The bot can then type the codes there; it never sees the key or any code.", sanitize_label(&info.site, SHOWN_CAP), sanitize_label(&info.login_id, SHOWN_CAP)),
        }))
    }

    /// `account_totp_save`.
    pub(super) async fn totp_save(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<ToolOutput, ToolError> {
        debug_assert_eq!(self.op, Op::TotpSave);
        let i: TotpSaveIn = parse(input.clone())?;
        let target = i.target()?;
        let shared = self.select_shared(ctx)?;
        let (info, _) = self.totp_save_facts(ctx, &shared, &i).await?;
        let secrets: Arc<FillSecrets> = shared
            .fill_secrets
            .clone()
            .ok_or_else(|| exec_err("no secrets are configured on this daemon"))?;
        let bot = ctx.bot_id.clone();
        // The owner may have paused or taken over the page: read nothing then.
        self.refuse_observation_while_latched()?;
        let accept = {
            let secrets = Arc::clone(&secrets);
            let name = i.name.clone();
            let bot = bot.clone();
            move |frame: Option<String>, top: Option<String>| {
                secrets
                    .check_totp_save_page(&name, bot.as_deref(), frame.as_deref(), top.as_deref())
                    .is_ok()
            }
        };
        let read = shared
            .driver
            .read_value(target, &accept)
            .await
            .map_err(exec_err)?;
        let text = SecretValue::new(read.value);
        // Looked for in what the element holds (a bare key, a labelled one, an otpauth
        // address); the error is a fixed sentence, never the text.
        let spec = TotpSpec::find_in_text(text.expose()).map_err(|e| refused(&e))?;
        secrets
            .set_totp(&i.name, bot.as_deref(), text.expose())
            .map_err(|e| refused(&e))?;
        Ok(ToolOutput::text(format!(
            "Two-factor key saved for {} ({} digits, every {} s), read from the page by Praxi. It is stored in the vault and you never see it. To sign in: type the password with browser_fill `secret` = \"{}\", then when the site asks for the code, browser_fill the code field with `secret` = \"totp:{}\" (you never see the code). Do not type the key anywhere.",
            sanitize_label(&info.name, SHOWN_CAP),
            spec.digits(),
            spec.period(),
            sanitize_label(&info.name, SHOWN_CAP),
            sanitize_label(&info.name, SHOWN_CAP),
        )))
    }
}

//! Bot management tools: a bot creates, configures and deletes other bots.
//!
//! `bot_list`, `bot_create`, `bot_update` and `bot_delete` talk to a
//! [`BotHost`], which the daemon implements over its bot store (this crate
//! must not depend on the daemon). The host enforces the security rules; the
//! tools only carry structured input and the approval hooks:
//!
//! * a bot can never create or change a bot to be broader than itself
//!   (permissions, tools, models, computer target), never touch itself, and
//!   only manages bots it created;
//! * `bot_create`, `bot_update` and `bot_delete` are [`Tool::always_ask`]: the
//!   user sees the complete resulting profile first, and no standing grant
//!   ("always allow", "this chat") waives it. The one exception is decided by the loop, not
//!   here: on the owner's own Full-level chat nothing asks but money, so a create or an update
//!   arrives with [`ToolContext::full_waived_preview`] set (a flag of its own, never
//!   `unattended_mail`) and the tool freezes the preview itself before it applies it (the host's
//!   rules are the same). A delete is never waived: it cannot be undone, so
//!   [`Tool::full_access_asks_for`] keeps its card on the Full level too;
//!   Without a card and without that flag nothing froze a preview, and the host refuses;
//! * the tools are visible only to bots whose profile enables them
//!   (`tools.manage_bots`, the "Can create bots" switch) and to bots at the Full level, where
//!   the loop offers them to the owner's own chat turns only (never to a schedule, a
//!   mission, a channel, a subagent or a helper bot, and not once the run read untrusted
//!   content). Visibility is not an authorisation: the host decides again on every call.
//!
//! The tools use category `System` with effect `Write` (create / update) or
//! `Destructive` (delete); never `Meta`, which the resolver always allows.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{exec_err, parse, spec};

/// Names of the bot tools (all start with `bot_`).
pub const BOT_TOOL_NAMES: [&str; 4] = ["bot_list", "bot_create", "bot_update", "bot_delete"];

/// Permission presets a bot may hand out (never `full`).
pub const BOT_PRESETS: [&str; 3] = ["read-only", "read-and-draft", "approve-destructive"];

/// The character shapes of the app (`packages/ui/src/botShape.ts`; a test keeps the lists equal).
pub const BOT_SHAPES: [&str; 16] = [
    "pebble", "cube", "diamond", "cloud", "tri", "flower", "gem", "drop", "star", "ghost",
    "capsule", "ears", "gumdrop", "heart", "clover", "boulder",
];

/// Who is calling: the bot whose authority is the ceiling and the run it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotCaller {
    /// Id of the calling bot.
    pub bot_id: String,
    /// Id of the calling run (may be empty outside a run).
    pub run_id: String,
}

/// The fields a bot may set. `create` needs `name` and `persona`; `update`
/// changes only what is given. Anything else (a `full` preset, MCP access,
/// skills, a computer target) is not a field: it cannot be asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRequest {
    /// Display name.
    pub name: Option<String>,
    /// Persona / system prompt.
    pub persona: Option<String>,
    /// Executor model, `provider/model`.
    pub executor: Option<String>,
    /// Optional planner model.
    pub planner: Option<String>,
    /// Optional classifier model.
    pub classifier: Option<String>,
    /// `read-only`, `read-and-draft` or `approve-destructive`.
    pub permission_preset: Option<String>,
    /// Tool allow list (names or globs).
    pub tools: Option<Vec<String>>,
    /// Colour as `#rrggbb`.
    pub color: Option<String>,
    /// Character shape, one of [`BOT_SHAPES`].
    pub shape: Option<String>,
    /// Avatar as an image data URL.
    pub avatar: Option<String>,
}

/// One proposed change, previewed before the user is asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum BotChange {
    /// `bot_create`.
    Create(BotRequest),
    /// `bot_update` of bot `id`.
    Update {
        /// Bot id.
        id: String,
        /// Fields to change.
        req: BotRequest,
    },
    /// `bot_delete` of bot `id`.
    Delete {
        /// Bot id.
        id: String,
    },
}

/// A bot as the model sees it (no persona text, no avatar bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotView {
    /// Bot id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Executor model.
    pub executor: String,
    /// Permission preset (`custom` when the policy is not a preset).
    pub permission_preset: String,
    /// Tool allow list (empty: every tool).
    pub tools: Vec<String>,
    /// Colour, when set.
    pub color: Option<String>,
    /// `bot` (you made it, you may change it) or `user`.
    pub origin: String,
    /// `false`: you may only read it (made by the user, or yourself).
    pub editable: bool,
}

/// One changed field of an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    /// Dotted path in the profile, e.g. `permissions.preset`.
    pub field: String,
    /// Before (`None`: not set).
    pub from: Option<String>,
    /// After (`None`: removed).
    pub to: Option<String>,
}

/// What an approval card shows about a proposed change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPreview {
    /// `create`, `update` or `delete`.
    pub action: String,
    /// Id of the bot being changed (not for `create`).
    pub bot_id: Option<String>,
    /// Name after the change (before, for `delete`).
    pub name: String,
    /// The complete resulting profile (for `delete`: the profile that goes
    /// away). Avatar bytes are replaced by a size note.
    pub profile: Value,
    /// For `update`: every field that changes.
    pub changes: Vec<FieldChange>,
    /// Avatar after the change as an image data URL, for the card to draw.
    pub avatar: Option<String>,
    /// Identifies this preview (see `ScheduleHost`): handed back when the
    /// approval ends without the call running. Never shown to the user.
    pub approval_ref: String,
}

/// The daemon side of the bot tools. Errors are complete sentences for the
/// model.
#[async_trait]
pub trait BotHost: Send + Sync {
    /// Whether `bot_id`'s profile has the "Can create bots" switch. Fails closed.
    fn may_manage_bots(&self, bot_id: &str) -> bool;

    /// Whether `bot_id`'s profile may SEE the bot tools: the switch, or the Full level (where
    /// the loop offers them to the owner's own chat turns only). Visibility only, never an
    /// authorisation: every call is judged again for its own run (the switch, or the owner's
    /// own Full chat turn as the loop knows it). Fails closed.
    fn may_see_bot_tools(&self, bot_id: &str) -> bool {
        self.may_manage_bots(bot_id)
    }

    /// The bots the caller can see.
    ///
    /// # Errors
    /// Storage failures, bot tools not enabled for the caller.
    async fn list(&self, caller: &BotCaller) -> Result<Vec<BotView>, String>;

    /// Validate `change` (ceiling included) and describe the result without
    /// changing anything. Freezes the result: the later call applies exactly
    /// that, or fails.
    ///
    /// # Errors
    /// Invalid input, ceiling exceeded, unknown / foreign / own bot.
    async fn preview(&self, caller: &BotCaller, change: &BotChange) -> Result<BotPreview, String>;

    /// Forget what [`Self::preview`] froze (approval ended without the call).
    fn discard(&self, caller: &BotCaller, change: &BotChange, approval_ref: &str);

    /// Create a bot. Audited. Fails when the call was not previewed or the
    /// result differs from what was previewed.
    ///
    /// # Errors
    /// As [`Self::preview`].
    async fn create(&self, caller: &BotCaller, req: &BotRequest) -> Result<BotView, String>;

    /// Change a bot. Audited. As [`Self::create`].
    ///
    /// # Errors
    /// As [`Self::preview`].
    async fn update(
        &self,
        caller: &BotCaller,
        id: &str,
        req: &BotRequest,
    ) -> Result<BotView, String>;

    /// Delete a bot. Audited. As [`Self::create`].
    ///
    /// # Errors
    /// As [`Self::preview`]; the bot still has chats or runs.
    async fn delete(&self, caller: &BotCaller, id: &str) -> Result<BotView, String>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    List,
    Create,
    Update,
    Delete,
}

/// One bot tool.
pub struct BotTool {
    spec: ToolSpec,
    op: Op,
    host: Arc<dyn BotHost>,
}

#[derive(Deserialize)]
struct IdIn {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateIn {
    id: String,
    #[serde(flatten)]
    req: BotRequest,
}

/// Build the four bot tools over `host`.
#[must_use]
pub fn tools(host: &Arc<dyn BotHost>) -> Vec<Arc<dyn Tool>> {
    let cat = PermissionCategory::System;
    let props = json!({
        "name": {"type":"string","description":"Display name."},
        "persona": {"type":"string","description":"The bot's system prompt: who it is and what it does."},
        "executor": {"type":"string","description":"Model that runs the bot, `provider/model`. Default: yours. Only providers you use yourself are accepted."},
        "planner": {"type":"string","description":"Optional planner model, `provider/model`."},
        "classifier": {"type":"string","description":"Optional classifier model, `provider/model`."},
        "permission_preset": {"type":"string","enum":BOT_PRESETS,"description":"Permission level. Default and maximum: approve-destructive, and never more than you have yourself. Raising it later is only possible in the app."},
        "tools": {"type":"array","items":{"type":"string"},"description":"Tool allow list: exact tool names you have yourself (globs only when identical to one of yours). Default: your own list."},
        "color": {"type":"string","description":"Colour as #rrggbb."},
        "shape": {"type":"string","enum":BOT_SHAPES,"description":"Character shape in the app."},
        "avatar": {"type":"string","description":"Avatar image as a data:image/png|jpeg|gif|webp;base64 URL (at most 200 KB)."}
    });
    let mut update_props = props.clone();
    if let Some(o) = update_props.as_object_mut() {
        o.insert(
            "id".into(),
            json!({"type":"string","description":"Bot id from bot_list."}),
        );
    }
    let id_schema = json!({"type":"object","properties":{
        "id":{"type":"string","description":"Bot id from bot_list."}
    },"required":["id"]});
    let mk = |name: &str, desc: &str, schema: Value, effect: Effect, op: Op| -> Arc<dyn Tool> {
        Arc::new(BotTool {
            spec: spec(name, desc, schema, effect, cat),
            op,
            host: Arc::clone(host),
        })
    };
    vec![
        mk(
            "bot_list",
            "List the user's bots (id, name, model, permission preset, tools). Bots with editable=true were created by you; you cannot change the others.",
            json!({"type":"object","properties":{}}),
            Effect::Read,
            Op::List,
        ),
        mk(
            "bot_create",
            "Create a real Praxi bot (it appears in the user's bot list). It can never have more permissions, tools or model access than you, and never more than approve-destructive. Unless the user is on the Full level, they approve the complete profile first. Example: {\"name\":\"Kralinho\",\"persona\":\"A witty chat companion.\",\"color\":\"#7c3aed\",\"shape\":\"star\"}",
            json!({"type":"object","properties":props,"required":["name","persona"],"additionalProperties":false}),
            Effect::Write,
            Op::Create,
        ),
        mk(
            "bot_update",
            "Change a bot you created (name, persona, models, tools, colour, shape, avatar, or lower its permission preset). It can never become broader. You cannot update yourself. Unless the user is on the Full level, they approve the change.",
            json!({"type":"object","properties":update_props,"required":["id"],"additionalProperties":false}),
            Effect::Write,
            Op::Update,
        ),
        mk(
            "bot_delete",
            "Delete a bot you created that has no more permissions, tools, files or providers than you. Refused when it already has chats or runs. The user always approves it, on the Full level too.",
            id_schema,
            Effect::Destructive,
            Op::Delete,
        ),
    ]
}

fn caller(ctx: &ToolContext) -> Result<BotCaller, ToolError> {
    let bot_id = ctx
        .bot_id
        .clone()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| exec_err("bot tools only work inside a bot's run"))?;
    Ok(BotCaller {
        bot_id,
        run_id: ctx.run_id.clone(),
    })
}

impl BotTool {
    fn change(&self, input: &Value) -> Result<Option<BotChange>, ToolError> {
        Ok(match self.op {
            Op::Create => Some(BotChange::Create(parse(input.clone())?)),
            Op::Update => {
                let u: UpdateIn = parse(input.clone())?;
                Some(BotChange::Update {
                    id: u.id,
                    req: u.req,
                })
            }
            Op::Delete => Some(BotChange::Delete {
                id: parse::<IdIn>(input.clone())?.id,
            }),
            Op::List => None,
        })
    }
}

fn view_text(v: &BotView) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

#[async_trait]
impl Tool for BotTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn visible_to(&self, bot_id: Option<&str>) -> bool {
        bot_id.is_some_and(|b| self.host.may_see_bot_tools(b))
    }

    fn always_ask(&self) -> bool {
        self.op != Op::List
    }

    /// Deleting a bot cannot be undone: the owner approves it on a card on the Full level too.
    fn full_access_asks_for(&self, _input: &Value) -> bool {
        self.op == Op::Delete
    }

    fn approval_closed(&self, ctx: &ToolContext, input: &Value, token: Option<&str>) {
        if let (Some(token), Ok(who), Ok(Some(change))) = (token, caller(ctx), self.change(input)) {
            self.host.discard(&who, &change, token);
        }
    }

    async fn approval_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        let Some(change) = self.change(input)? else {
            return Ok(None);
        };
        let p = self
            .host
            .preview(&caller(ctx)?, &change)
            .await
            .map_err(exec_err)?;
        let mut v = serde_json::to_value(&p).map_err(|e| exec_err(e.to_string()))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("kind".into(), json!("bot"));
        }
        Ok(Some(v))
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let who = caller(ctx)?;
        if !self.host.may_see_bot_tools(&who.bot_id) {
            return Err(exec_err(
                "bot management is not enabled for this bot; the user can turn on \"Can create bots\" in its settings",
            ));
        }
        let h = &self.host;
        // The loop admitted this call without a card because it is the owner's own Full chat
        // turn, so nothing froze the preview the host applies: freeze it now. The host refuses
        // what its ceiling forbids, as for a card. Only that flag does it: a call with no card
        // and no flag has no frozen preview and the host refuses it.
        // Never for a delete: it always has the owner's card (`full_access_asks_for`), so a
        // delete without one has no frozen preview and the host refuses it.
        if ctx.full_waived_preview
            && self.op != Op::Delete
            && let Some(change) = self.change(&input)?
        {
            h.preview(&who, &change).await.map_err(exec_err)?;
        }
        match self.op {
            Op::List => {
                let all = h.list(&who).await.map_err(exec_err)?;
                if all.is_empty() {
                    return Ok(ToolOutput::text("There are no bots."));
                }
                Ok(ToolOutput::text(
                    serde_json::to_string_pretty(&all).unwrap_or_default(),
                ))
            }
            Op::Create => {
                let req: BotRequest = parse(input)?;
                let v = h.create(&who, &req).await.map_err(exec_err)?;
                Ok(ToolOutput::text(format!("Bot created.\n{}", view_text(&v))))
            }
            Op::Update => {
                let u: UpdateIn = parse(input)?;
                let v = h.update(&who, &u.id, &u.req).await.map_err(exec_err)?;
                Ok(ToolOutput::text(format!("Bot updated.\n{}", view_text(&v))))
            }
            Op::Delete => {
                let id = parse::<IdIn>(input)?.id;
                let v = h.delete(&who, &id).await.map_err(exec_err)?;
                Ok(ToolOutput::text(format!("Bot deleted.\n{}", view_text(&v))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        enabled: bool,
        fail_preview: bool,
    }

    fn view() -> BotView {
        BotView {
            id: "b1".into(),
            name: "n".into(),
            executor: "p/m".into(),
            permission_preset: "approve-destructive".into(),
            tools: vec![],
            color: None,
            origin: "bot".into(),
            editable: true,
        }
    }

    #[async_trait]
    impl BotHost for Fake {
        fn may_manage_bots(&self, _: &str) -> bool {
            self.enabled
        }
        async fn list(&self, c: &BotCaller) -> Result<Vec<BotView>, String> {
            self.calls.lock().push(format!("list {}", c.bot_id));
            Ok(vec![])
        }
        async fn preview(&self, c: &BotCaller, ch: &BotChange) -> Result<BotPreview, String> {
            self.calls
                .lock()
                .push(format!("preview {} {ch:?}", c.bot_id));
            if self.fail_preview {
                return Err("ceiling".into());
            }
            Ok(BotPreview {
                action: "create".into(),
                bot_id: None,
                name: "n".into(),
                profile: json!({"bot":{"persona":"full text"}}),
                changes: vec![],
                avatar: None,
                approval_ref: "tok-1".into(),
            })
        }
        fn discard(&self, c: &BotCaller, ch: &BotChange, token: &str) {
            self.calls
                .lock()
                .push(format!("discard {} {ch:?} {token}", c.bot_id));
        }
        async fn create(&self, c: &BotCaller, _: &BotRequest) -> Result<BotView, String> {
            self.calls.lock().push(format!("create {}", c.bot_id));
            Ok(view())
        }
        async fn update(&self, c: &BotCaller, id: &str, _: &BotRequest) -> Result<BotView, String> {
            self.calls.lock().push(format!("update {} {id}", c.bot_id));
            Ok(view())
        }
        async fn delete(&self, c: &BotCaller, id: &str) -> Result<BotView, String> {
            self.calls.lock().push(format!("delete {} {id}", c.bot_id));
            Ok(view())
        }
    }

    fn ctx(bot: Option<&str>) -> ToolContext {
        ToolContext {
            run_id: "run-1".into(),
            bot_id: bot.map(str::to_owned),
            ..ToolContext::default()
        }
    }

    fn tool(host: &Arc<Fake>, name: &str) -> Arc<dyn Tool> {
        tools(&(host.clone() as Arc<dyn BotHost>))
            .into_iter()
            .find(|t| t.spec().name == name)
            .unwrap()
    }

    fn on() -> Arc<Fake> {
        Arc::new(Fake {
            enabled: true,
            ..Fake::default()
        })
    }

    #[test]
    fn specs_are_system_never_meta_and_delete_is_destructive() {
        let all = tools(&(on() as Arc<dyn BotHost>));
        let names: Vec<_> = all.iter().map(|t| t.spec().name.clone()).collect();
        assert_eq!(names, BOT_TOOL_NAMES);
        for t in &all {
            let s = t.spec();
            assert_eq!(s.category, PermissionCategory::System, "{}", s.name);
            assert!(s.is_valid(), "{}", s.name);
            let want = match s.name.as_str() {
                "bot_list" => Effect::Read,
                "bot_delete" => Effect::Destructive,
                _ => Effect::Write,
            };
            assert_eq!(s.effect, want, "{}", s.name);
        }
    }

    #[test]
    fn every_changing_tool_always_asks_and_list_does_not() {
        for t in tools(&(on() as Arc<dyn BotHost>)) {
            assert_eq!(
                t.always_ask(),
                t.spec().name != "bot_list",
                "{}",
                t.spec().name
            );
        }
    }

    #[test]
    fn the_schema_offers_no_way_to_ask_for_more_than_a_preset() {
        let host = on();
        let create = tool(&host, "bot_create");
        let props = &create.spec().input_schema["properties"];
        assert_eq!(
            props["permission_preset"]["enum"],
            json!(["read-only", "read-and-draft", "approve-destructive"])
        );
        assert!(props.get("mcp").is_none() && props.get("computer").is_none());
        assert_eq!(create.spec().input_schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn unknown_fields_are_refused_before_approval() {
        let host = on();
        for input in [
            json!({"name":"n","persona":"p","permission":"full"}),
            json!({"name":"n","persona":"p","mcp":["*"]}),
        ] {
            let e = tool(&host, "bot_create")
                .approval_details(&ctx(Some("b")), &input)
                .await
                .unwrap_err();
            assert!(matches!(e, ToolError::InvalidInput(_)), "{input}");
        }
        assert!(host.calls.lock().is_empty());
    }

    #[test]
    fn hidden_unless_the_host_enables_the_bot() {
        let off = Arc::new(Fake::default());
        let t = tool(&off, "bot_create");
        assert!(!t.visible_to(Some("b")));
        assert!(!t.visible_to(None));
        assert!(tool(&on(), "bot_create").visible_to(Some("b")));
        assert!(!tool(&on(), "bot_create").visible_to(None));
    }

    #[tokio::test]
    async fn a_call_from_a_bot_without_the_switch_is_refused() {
        let host = Arc::new(Fake::default());
        let e = tool(&host, "bot_create")
            .call(&ctx(Some("b")), json!({"name":"n","persona":"p"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not enabled"), "{e}");
        assert!(host.calls.lock().is_empty());
    }

    /// A call the Full level admitted without a card freezes its own preview first; a call
    /// that went through a card (the loop froze it) does not.
    #[tokio::test]
    async fn a_call_admitted_without_a_card_previews_before_it_applies() {
        let host = on();
        let c = ToolContext {
            full_waived_preview: true,
            ..ctx(Some("bot-a"))
        };
        tool(&host, "bot_create")
            .call(&c, json!({"name":"Kralinho","persona":"p","shape":"star"}))
            .await
            .unwrap();
        tool(&host, "bot_update")
            .call(&c, json!({"id":"x","name":"m"}))
            .await
            .unwrap();
        tool(&host, "bot_list").call(&c, json!({})).await.unwrap();
        let calls = host.calls.lock().clone();
        assert_eq!(calls.len(), 5, "{calls:?}");
        assert!(calls[0].starts_with("preview bot-a Create"), "{calls:?}");
        assert_eq!(calls[1], "create bot-a");
        assert!(calls[2].starts_with("preview bot-a Update"), "{calls:?}");
        assert_eq!(calls[3], "update bot-a x");
        assert_eq!(calls[4], "list bot-a");
        // a preview the host refuses (its ceiling) stops the call before anything is applied
        let strict = Arc::new(Fake {
            enabled: true,
            fail_preview: true,
            ..Fake::default()
        });
        let e = tool(&strict, "bot_create")
            .call(&c, json!({"name":"n","persona":"p"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("ceiling"), "{e}");
        assert!(!strict.calls.lock().iter().any(|l| l.starts_with("create")));
    }

    /// A delete always asks the owner, on the Full level too, and never freezes its own
    /// preview: a delete that arrives without a card has nothing frozen and the host refuses it.
    #[tokio::test]
    async fn a_delete_always_needs_the_owners_card() {
        let host = on();
        let delete = tool(&host, "bot_delete");
        assert!(delete.always_ask());
        assert!(delete.full_access_asks_for(&json!({"id": "x"})));
        for name in ["bot_create", "bot_update", "bot_list"] {
            assert!(
                !tool(&host, name).full_access_asks_for(&json!({})),
                "{name}"
            );
        }
        let c = ToolContext {
            full_waived_preview: true,
            ..ctx(Some("bot-a"))
        };
        delete.call(&c, json!({"id": "x"})).await.unwrap();
        let calls = host.calls.lock().clone();
        assert!(!calls.iter().any(|l| l.starts_with("preview")), "{calls:?}");
    }

    /// Only the loop's own flag freezes a preview. A call the Full level let through for another
    /// reason (`unattended_mail` is on for every call of a Full run, a schedule's included) and a
    /// call that went through a card get none from the tool: the host applies what a card froze,
    /// and refuses a change nothing froze.
    #[tokio::test]
    async fn unattended_mail_alone_never_stands_in_for_a_preview() {
        let host = on();
        let c = ToolContext {
            unattended_mail: true,
            ..ctx(Some("bot-a"))
        };
        tool(&host, "bot_create")
            .call(&c, json!({"name":"n","persona":"p"}))
            .await
            .unwrap();
        tool(&host, "bot_delete")
            .call(&c, json!({"id":"x"}))
            .await
            .unwrap();
        assert_eq!(
            host.calls.lock().clone(),
            ["create bot-a", "delete bot-a x"],
            "no preview was frozen by the tool"
        );
    }

    #[test]
    fn the_shape_is_one_of_the_apps_sixteen_and_the_lists_stay_equal() {
        let host = on();
        let props = tool(&host, "bot_create").spec().input_schema["properties"].clone();
        assert_eq!(props["shape"]["enum"], json!(BOT_SHAPES));
        // the app's own list is the source: ids in order
        let ts = include_str!("../../../packages/ui/src/botShape.ts");
        let ids: Vec<&str> = ts
            .split("const SHAPE_IDS = [")
            .nth(1)
            .and_then(|rest| rest.split("] as const").next())
            .unwrap()
            .split("{ id: '")
            .skip(1)
            .map(|chunk| chunk.split('\'').next().unwrap())
            .collect();
        assert_eq!(ids, BOT_SHAPES);
    }

    #[tokio::test]
    async fn calls_carry_the_context_bot() {
        let host = on();
        let c = ctx(Some("bot-a"));
        tool(&host, "bot_list").call(&c, json!({})).await.unwrap();
        tool(&host, "bot_create")
            .call(&c, json!({"name":"n","persona":"p"}))
            .await
            .unwrap();
        tool(&host, "bot_update")
            .call(&c, json!({"id":"x","name":"m"}))
            .await
            .unwrap();
        tool(&host, "bot_delete")
            .call(&c, json!({"id":"x"}))
            .await
            .unwrap();
        assert_eq!(
            host.calls.lock().clone(),
            [
                "list bot-a",
                "create bot-a",
                "update bot-a x",
                "delete bot-a x"
            ]
        );
        let e = tool(&host, "bot_list")
            .call(&ctx(None), json!({}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("inside a bot's run"), "{e}");
    }

    #[tokio::test]
    async fn approval_details_carry_the_full_profile() {
        let host = on();
        let d = tool(&host, "bot_create")
            .approval_details(&ctx(Some("b")), &json!({"name":"n","persona":"p"}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d["kind"], "bot");
        assert_eq!(d["profile"]["bot"]["persona"], "full text");
        assert!(
            tool(&host, "bot_delete")
                .approval_details(&ctx(Some("b")), &json!({"id":"x"}))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            tool(&host, "bot_list")
                .approval_details(&ctx(Some("b")), &json!({}))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_failed_preview_stops_before_the_user_is_asked() {
        let host = Arc::new(Fake {
            enabled: true,
            fail_preview: true,
            ..Fake::default()
        });
        let e = tool(&host, "bot_create")
            .approval_details(&ctx(Some("b")), &json!({"name":"n","persona":"p"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("ceiling"));
    }

    #[test]
    fn a_closed_approval_discards_what_the_host_froze() {
        let host = on();
        let t = tool(&host, "bot_delete");
        t.approval_closed(&ctx(Some("bot-a")), &json!({"id":"x"}), Some("tok-9"));
        let log = host.calls.lock().clone();
        assert_eq!(log.len(), 1);
        assert!(log[0].starts_with("discard bot-a Delete"), "{log:?}");
        assert!(log[0].ends_with("tok-9"), "{log:?}");
        t.approval_closed(&ctx(Some("bot-a")), &json!({"id":"x"}), None);
        t.approval_closed(&ctx(None), &json!({"id":"x"}), Some("tok"));
        assert_eq!(host.calls.lock().len(), 1);
    }
}

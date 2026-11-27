//! `mission_create`: a Full-level owner chat turns a long batch job into a mission.
//!
//! A plain chat turn stops at the step guard (40 model calls) and cannot go on without the
//! owner; a mission chains sessions from a ledger until the job is done. The owner already
//! asked for the job in the chat, at the level that works without asking, so no card is shown:
//! the tool is offered, and runs, only where the owner themself is at the keyboard on a Full
//! bot (the same rule as the Full level's waivers, see [`Shared::mission_create_offered`]).
//!
//! What the model controls is small on purpose. The goal is the owner's own message, copied by
//! the runtime from the run's stored user message, never text the model wrote (so a page or
//! mail the model read cannot start a mission about something else); budgets are the defaults
//! (only the owner changes them); a run that already read untrusted content cannot start one.
//! The model may name the first items, a "done when" count, a title and one line of context.

use std::sync::Weak;

use async_trait::async_trait;
use protocol::{ChatId, InitiatorKind, PermissionCategory, RunId};
use serde::Deserialize;
use serde_json::{Value, json};
use storage::{Chat, Message};
use tool_registry::{
    ContentClass, Effect, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};
use uuid::Uuid;

use crate::shared::{RunCtx, Shared};

/// Name of the tool.
pub(crate) const MISSION_CREATE: &str = "mission_create";

/// Longest goal a mission takes, in characters (the same limit as the "Start a mission" route).
const MAX_GOAL_CHARS: usize = 4_000;
/// Longest context line the model may add, in characters.
const MAX_CONTEXT_CHARS: usize = 400;
/// Active missions one bot may have next to each other.
const MAX_ACTIVE_MISSIONS: usize = 3;
/// Largest "done when" count the model may name: a ledger holds at most
/// [`missions::model::MAX_ITEMS`] (500) items, so a bigger target could never be reached.
const MAX_TARGET_DONE: u32 = 500;
/// Longest item kind label, in characters (the ledger's own limit).
const MAX_KIND_CHARS: usize = 40;

/// The `mission_create` tool. It reads the run through [`Shared`], so it holds it weakly.
pub(crate) struct MissionCreateTool {
    spec: ToolSpec,
    sh: Weak<Shared>,
}

/// The tool, ready to register.
pub(crate) fn tool(sh: Weak<Shared>) -> MissionCreateTool {
    MissionCreateTool {
        spec: ToolSpec {
            name: MISSION_CREATE.to_owned(),
            description: "Start a mission: use it when the owner asks for a long batch job that cannot fit one chat turn (dozens of sites to sign up on, many leads, a pile of documents). Call it FIRST, before browsing or reading anything. The runtime copies the owner's message as the mission's goal and runs the job in its own chat, session after session, without asking; the owner gets a report there. You can name the first items (one per site, lead or document), a \"done when\" count and one line of context the goal lacks. After it succeeds, tell the owner in one or two sentences what you started, and stop: do not do the work in this chat.".to_owned(),
            input_schema: json!({"type":"object","properties":{
                "title":{"type":"string","description":"Short label for the mission (optional)."},
                "target_done":{"type":"integer","minimum":1,"description":"Done when at least this many items are done, e.g. 20 backlinks. Leave out when every item has to be finished."},
                "context":{"type":"string","description":"One line the owner's message does not say but the mission needs, e.g. which address or site. No secrets."},
                "items":{"type":"array","maxItems":100,"description":"The first items, if you already know them; later sessions plan the rest.","items":{"type":"object","properties":{
                    "key":{"type":"string","description":"Short unique id, e.g. the site's domain."},
                    "title":{"type":"string","description":"What to do for this item, one line."},
                    "kind":{"type":"string","description":"Label such as site, lead, task (default task)."}
                },"required":["key","title"]}}
            }}),
            // Local state only, like the mission_* tools: it creates a record in the owner's own
            // database and a chat; nothing leaves the machine.
            effect: Effect::Write,
            category: PermissionCategory::Meta,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        },
        sh,
    }
}

#[derive(Deserialize)]
struct CreateIn {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    target_done: Option<u32>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    items: Vec<ItemIn>,
}

#[derive(Deserialize)]
struct ItemIn {
    key: String,
    title: String,
    #[serde(default)]
    kind: Option<String>,
}

impl ItemIn {
    /// A ledger item from what the model wrote: trimmed, key / title / kind clipped to the
    /// ledger's limits, empty or control-character text refused.
    fn into_new_item(self) -> Result<missions::NewItem, ToolError> {
        let field = |what: &str, s: &str, max: usize| -> Result<String, ToolError> {
            let s = s.trim();
            if s.is_empty() {
                return Err(ToolError::InvalidInput(format!(
                    "an item's {what} is empty"
                )));
            }
            if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                return Err(ToolError::InvalidInput(format!(
                    "an item's {what} contains control characters"
                )));
            }
            Ok(missions::model::clip(s, max))
        };
        Ok(missions::NewItem {
            key: field("key", &self.key, missions::model::MAX_KEY_CHARS)?,
            title: field("title", &self.title, missions::model::MAX_TITLE_CHARS)?,
            kind: match self
                .kind
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
            {
                Some(k) => field("kind", k, MAX_KIND_CHARS)?,
                None => "task".to_owned(),
            },
            payload: None,
        })
    }
}

#[async_trait]
impl Tool for MissionCreateTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let sh = self
            .sh
            .upgrade()
            .ok_or_else(|| ToolError::Execution("the agent loop is shutting down".into()))?;
        let run = Uuid::parse_str(&ctx.run_id)
            .map(RunId)
            .map_err(|_| ToolError::InvalidInput("unknown run".into()))?;
        let run_ctx = sh.runs.lock().get(&run).cloned().ok_or_else(|| {
            ToolError::InvalidInput("this tool only works inside a running chat".into())
        })?;
        let input: CreateIn =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        sh.start_mission_from_chat(&run_ctx, input)
            .map(ToolOutput::text)
    }
}

impl Shared {
    /// Whether `mission_create` is offered to, and allowed for, this run: the owner's own chat
    /// turn (typed in the app or from their paired Telegram chat) on a bot at the Full level,
    /// not a mission session, subagent or helper, with the missions ledger switched on.
    pub(crate) fn mission_create_offered(&self, ctx: &RunCtx) -> bool {
        ctx.mission.is_none()
            && ctx.initiator == InitiatorKind::Human
            && ctx.parent_run_id.is_none()
            && ctx.profile.permissions.mode() == permission_engine::AutonomyMode::Full
            && self.mission_store.read().is_some()
            && self.full_owner_started(ctx)
    }

    /// Start a mission from the owner's chat turn.
    fn start_mission_from_chat(&self, ctx: &RunCtx, input: CreateIn) -> Result<String, ToolError> {
        let refuse = |why: &str| ToolError::Execution(why.to_owned());
        if !self.mission_create_offered(ctx) {
            return Err(refuse(
                "a mission can only be started from the owner's own chat, on a bot at the Full level",
            ));
        }
        // A run that read a page, a mail or another bot's answer could have been steered: the
        // request has to be acted on before the run reads anything untrusted.
        if ctx.taint.lock().exposure.untrusted {
            return Err(refuse(
                "this chat already read outside content, so it cannot start a mission any more; ask the owner to start it with the Start a mission button, or ask again in a new message",
            ));
        }
        let store = self
            .mission_store
            .read()
            .clone()
            .ok_or_else(|| refuse("missions are not running on this computer"))?;
        let goal = self.owner_goal(ctx)?;
        if let Some(already) = Self::open_missions_verdict(&store, ctx, &goal)? {
            return Ok(already);
        }
        let title = input
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(
                || missions::model::clip(&goal, 60),
                |t| missions::model::clip(t, 80),
            );
        if input.items.len() > missions::model::MAX_ITEMS_PER_CALL {
            return Err(ToolError::InvalidInput(format!(
                "name at most {} items; later sessions plan the rest",
                missions::model::MAX_ITEMS_PER_CALL
            )));
        }
        // Everything the model typed is checked before the mission's chat is opened, so a bad
        // item leaves nothing behind.
        let target_done = match input.target_done {
            Some(n) if n > MAX_TARGET_DONE => {
                return Err(ToolError::InvalidInput(format!(
                    "target_done is at most {MAX_TARGET_DONE}; leave it out when every item has to be finished"
                )));
            }
            other => other.filter(|n| *n > 0),
        };
        let items = input
            .items
            .into_iter()
            .map(ItemIn::into_new_item)
            .collect::<Result<Vec<_>, _>>()?;
        let chat = self.open_mission_chat(ctx, &title, &goal)?;
        // The cap is checked again inside the store's write transaction: the check above is
        // only for a friendly answer, two calls at once could both pass it.
        let mission = match store.create_capped(
            &missions::NewMission {
                user_id: ctx.user_id.to_string(),
                bot_id: ctx.bot_id.to_string(),
                chat_id: chat.to_string(),
                title,
                goal_text: goal,
                target_done,
                budgets: missions::Budgets::default(),
                items,
            },
            storage::now_ms(),
            Some(MAX_ACTIVE_MISSIONS),
        ) {
            Ok(m) => m,
            Err(e) => {
                // No mission took the chat: do not leave an empty "Mission: ..." chat behind.
                let _ = self.storage.delete_chat(chat);
                return Err(ToolError::Execution(e.to_string()));
            }
        };
        if let Some(line) = input
            .context
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            // A pinned note: it appears in every brief, fenced as data like all ledger text.
            let _ = store.add_note(
                &mission.id,
                "learned",
                &missions::model::clip(line, MAX_CONTEXT_CHARS),
                true,
                storage::now_ms(),
            );
        }
        // Ids only, never the goal text.
        self.audit(
            ctx,
            "system",
            "mission.created",
            &json!({"mission_id": mission.id, "info": {"bot_id": ctx.bot_id.to_string(), "from": "chat"}}),
        );
        Ok(format!(
            "Mission started: \"{}\". It runs on its own, session after session, in its own chat (\"Mission: ...\"), and the owner gets a report there when it ends or needs them. Tell the owner in one or two sentences what you started and stop: do not do the work in this chat.",
            missions::model::clip(&mission.title, 80)
        ))
    }

    /// The goal: the owner's own message that started the run, as stored. Never text the model
    /// wrote. Fails closed: the first stored user message of the run has to be recorded as the
    /// owner's (typed in the app, or from their paired Telegram chat) and not written after
    /// untrusted content; anything else (an imported, peer, mail or webhook message, a missing
    /// or untagged source) is refused.
    fn owner_goal(&self, ctx: &RunCtx) -> Result<String, ToolError> {
        let not_owner = || {
            ToolError::Execution(
                "the owner's message is not available as a goal; ask the owner to start the mission with the Start a mission button".into(),
            )
        };
        let (text, source, untrusted) = self
            .storage
            .run_first_user_message(ctx.run_id)
            .map_err(|e| ToolError::Execution(e.to_string()))?
            .ok_or_else(not_owner)?;
        if !goal_origin_ok(&source, untrusted) {
            return Err(not_owner());
        }
        let goal = Some(text.trim().to_owned())
            .filter(|g| !g.is_empty())
            .ok_or_else(not_owner)?;
        if goal.chars().count() > MAX_GOAL_CHARS {
            return Err(ToolError::Execution(format!(
                "the owner's message is longer than {MAX_GOAL_CHARS} characters; ask them to shorten it or to use the Start a mission button"
            )));
        }
        Ok(goal)
    }

    /// `Ok(Some(answer))` when this request already is an open mission of the bot (the tool
    /// answers instead of starting a second one); an error when the bot has too many open.
    fn open_missions_verdict(
        store: &missions::MissionStore,
        ctx: &RunCtx,
        goal: &str,
    ) -> Result<Option<String>, ToolError> {
        let bot = ctx.bot_id.to_string();
        let open: Vec<_> = store
            .list(&ctx.user_id.to_string())
            .map_err(|e| ToolError::Execution(e.to_string()))?
            .into_iter()
            .filter(|m| {
                m.bot_id == bot
                    && matches!(
                        m.state,
                        missions::MissionState::Running
                            | missions::MissionState::Paused
                            | missions::MissionState::Blocked
                    )
            })
            .collect();
        if let Some(same) = open.iter().find(|m| m.goal_text == goal) {
            return Ok(Some(format!(
                "That mission is already {} (it started from this same request). Tell the owner and stop; do not start it again.",
                same.state
            )));
        }
        if open.len() >= MAX_ACTIVE_MISSIONS {
            return Err(ToolError::Execution(format!(
                "this bot already has {MAX_ACTIVE_MISSIONS} missions open; the owner has to finish or stop one first"
            )));
        }
        Ok(None)
    }

    /// The mission's own chat, where its sessions run and its report is posted, holding the
    /// goal as the owner's message.
    fn open_mission_chat(
        &self,
        ctx: &RunCtx,
        title: &str,
        goal: &str,
    ) -> Result<ChatId, ToolError> {
        let chat = Chat {
            id: ChatId::new(),
            user_id: ctx.user_id,
            bot_id: ctx.bot_id,
            title: format!("Mission: {title}"),
            created_at_ms: storage::now_ms(),
        };
        let goal_message = Uuid::now_v7();
        let store_err = |e: storage::StorageError| ToolError::Execution(e.to_string());
        self.storage.insert_chat(&chat).map_err(store_err)?;
        self.storage
            .insert_message(&Message {
                id: goal_message,
                user_id: ctx.user_id,
                chat_id: chat.id,
                run_id: None,
                role: "user".to_owned(),
                content: goal.to_owned(),
                created_at_ms: storage::now_ms(),
                exposure_untrusted: false,
                exposure_private: false,
                exposure_sources: Vec::new(),
            })
            .map_err(store_err)?;
        // The owner wrote it, in the chat the run started from: it counts as their words (named
        // sites, typed URLs) for the approval code, exactly as the goal of a mission started
        // from the app does. The briefs the supervisor writes later never do.
        self.storage
            .set_message_source(goal_message, "owner")
            .map_err(store_err)?;
        Ok(chat.id)
    }
}

/// Whether a stored user message may become a mission's goal: recorded as the owner's own
/// (typed in the app, or from their paired Telegram chat) and not written after untrusted
/// content. Any other source, including an empty (untagged) one, is refused.
pub(crate) fn goal_origin_ok(source: &str, exposure_untrusted: bool) -> bool {
    !exposure_untrusted && matches!(source, "owner" | "telegram_owner")
}

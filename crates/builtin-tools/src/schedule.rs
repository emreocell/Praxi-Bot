//! Schedule tools: a bot manages its own scheduled tasks.
//!
//! `schedule_list`, `schedule_create`, `schedule_update`, `schedule_pause`,
//! `schedule_resume` and `schedule_delete` talk to a [`ScheduleHost`], which
//! the daemon implements over its schedule store (this crate must not depend
//! on the scheduler). Every call is scoped to the calling bot
//! ([`ToolContext::bot_id`]): a bot never sees or changes another bot's schedule.
//!
//! A schedule is delegated *future* authority: every firing runs the stored
//! prompt with the bot's permissions, unattended. So `schedule_create`,
//! `schedule_update` and `schedule_resume` are [`Tool::always_ask`] (the user
//! confirms the exact prompt and the next firing times; no standing grant
//! waives it) and they report those facts through [`Tool::approval_details`].
//! The tools use category `System` (changing what the machine does later, a
//! level every policy preset covers) with effect `Write`; never `Meta`, which
//! the resolver always allows.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{exec_err, parse, spec};

/// Names of the schedule tools (all start with `schedule_`).
pub const SCHEDULE_TOOL_NAMES: [&str; 6] = [
    "schedule_list",
    "schedule_create",
    "schedule_update",
    "schedule_pause",
    "schedule_resume",
    "schedule_delete",
];

/// Who is calling: the bot whose schedules are in scope and the run it is in
/// (the new schedule posts into that run's chat; audit entries name the run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleCaller {
    /// Id of the calling bot.
    pub bot_id: String,
    /// Id of the calling run (may be empty outside a run).
    pub run_id: String,
}

/// The fields a bot may set. Create needs `name`, `prompt` and exactly one of
/// `when` / `cron`; update changes only what is given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRequest {
    /// Short name.
    pub name: Option<String>,
    /// What the bot is asked to do at each firing.
    pub prompt: Option<String>,
    /// English / Turkish phrase ("every weekday at 10:30").
    pub when: Option<String>,
    /// 5-field cron expression.
    pub cron: Option<String>,
    /// IANA timezone for `when` / `cron` (default: the user's).
    pub tz: Option<String>,
}

/// One proposed change, to be previewed before the user is asked.
///
/// The host freezes what the preview resolved (timing, the schedule's
/// revision); the later `create` / `update` / `set_enabled` / `delete` for the
/// same call applies exactly that, or fails if the schedule changed meanwhile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ScheduleChange {
    /// `schedule_create`.
    Create(ScheduleRequest),
    /// `schedule_update` of schedule `id`.
    Update {
        /// Schedule id.
        id: String,
        /// Fields to change.
        req: ScheduleRequest,
    },
    /// `schedule_resume` of schedule `id`.
    Resume {
        /// Schedule id.
        id: String,
    },
    /// `schedule_pause` of schedule `id`.
    Pause {
        /// Schedule id.
        id: String,
    },
    /// `schedule_delete` of schedule `id`.
    Delete {
        /// Schedule id.
        id: String,
    },
}

/// A schedule as the model sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleView {
    /// Schedule id.
    pub id: String,
    /// Name.
    pub name: String,
    /// The prompt that runs at each firing.
    pub prompt: String,
    /// Whether it is armed.
    pub enabled: bool,
    /// Human-readable timing.
    pub schedule: String,
    /// Timezone the timing is read in.
    pub timezone: String,
    /// Next firing times (RFC 3339, in `timezone`), at most 3.
    pub next_runs: Vec<String>,
    /// Last firing (RFC 3339, in `timezone`).
    pub last_run: Option<String>,
    /// `bot` (you made it, you may change it) or `user` (made by the user:
    /// read-only for you).
    pub origin: String,
    /// `false` for user-made schedules: you may only read them.
    pub editable: bool,
}

/// The schedule as it is before an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousSchedule {
    /// Name before.
    pub name: String,
    /// Prompt before.
    pub prompt: String,
    /// Human-readable timing before.
    pub schedule: String,
    /// Timezone before.
    pub timezone: String,
}

/// What an approval card shows about a proposed change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulePreview {
    /// `create`, `update`, `resume`, `pause` or `delete`.
    pub action: String,
    /// Id of the schedule being changed (not for `create`).
    pub schedule_id: Option<String>,
    /// Name after the change.
    pub name: String,
    /// The exact prompt that will run (empty for pause / delete).
    pub prompt: String,
    /// Length of `prompt` in characters.
    pub prompt_chars: usize,
    /// Human-readable timing after the change.
    pub schedule: String,
    /// Timezone the timing is read in.
    pub timezone: String,
    /// The next 3 firing times (RFC 3339, in the user's timezone).
    pub next_runs: Vec<String>,
    /// For `update`: what it is now (old to new).
    pub previous: Option<PreviousSchedule>,
    /// Identifies this preview: the approval loop hands it back when the
    /// approval ends without the call running, and only the entry created for
    /// it is dropped. Never shown to the user.
    pub approval_ref: String,
}

/// The daemon side of the schedule tools. Every method is scoped to
/// `caller.bot_id`; an id of another bot's schedule is "not found".
///
/// Errors are complete sentences for the model.
pub trait ScheduleHost: Send + Sync {
    /// The bot's schedules.
    ///
    /// # Errors
    /// Storage failures.
    fn list(&self, caller: &ScheduleCaller) -> Result<Vec<ScheduleView>, String>;

    /// Validate `change` (limits included) and describe what it would do,
    /// without changing anything.
    ///
    /// # Errors
    /// Invalid input, limit exceeded, unknown schedule.
    fn preview(
        &self,
        caller: &ScheduleCaller,
        change: &ScheduleChange,
    ) -> Result<SchedulePreview, String>;

    /// Forget what [`Self::preview`] froze for `change`: its approval ended
    /// without the call running (denied, cancelled, timed out).
    fn discard(&self, caller: &ScheduleCaller, change: &ScheduleChange, approval_ref: &str);

    /// Create a schedule (armed). Audited.
    ///
    /// # Errors
    /// As [`Self::preview`].
    fn create(
        &self,
        caller: &ScheduleCaller,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String>;

    /// Change a schedule's definition. Audited.
    ///
    /// # Errors
    /// As [`Self::preview`].
    fn update(
        &self,
        caller: &ScheduleCaller,
        id: &str,
        req: &ScheduleRequest,
    ) -> Result<ScheduleView, String>;

    /// Arm (`true`) or pause (`false`) a schedule. Audited.
    ///
    /// # Errors
    /// Unknown schedule, limit exceeded when re-arming.
    fn set_enabled(
        &self,
        caller: &ScheduleCaller,
        id: &str,
        enabled: bool,
    ) -> Result<ScheduleView, String>;

    /// Delete a schedule. Audited.
    ///
    /// # Errors
    /// Unknown schedule.
    fn delete(&self, caller: &ScheduleCaller, id: &str) -> Result<ScheduleView, String>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    List,
    Create,
    Update,
    Pause,
    Resume,
    Delete,
}

/// One schedule tool.
pub struct ScheduleTool {
    spec: ToolSpec,
    op: Op,
    host: Arc<dyn ScheduleHost>,
}

#[derive(Deserialize)]
struct IdIn {
    id: String,
}

#[derive(Deserialize)]
struct UpdateIn {
    id: String,
    #[serde(flatten)]
    req: ScheduleRequest,
}

/// Build the six schedule tools over `host`.
#[must_use]
pub fn tools(host: &Arc<dyn ScheduleHost>) -> Vec<Arc<dyn Tool>> {
    let cat = PermissionCategory::System;
    let when_props = json!({
        "when": {"type":"string","description":"Natural language, English or Turkish: \"every weekday at 10:30\", \"her gün 09:00\", \"in 2 hours\", \"every 15 minutes\". Give `when` or `cron`, not both."},
        "cron": {"type":"string","description":"5-field cron expression, e.g. \"30 10 * * 1-5\". Give `when` or `cron`, not both."},
        "tz": {"type":"string","description":"IANA timezone for calendar times (default: the user's timezone)."}
    });
    let mut create_props = json!({
        "name": {"type":"string","description":"Short name for the schedule."},
        "prompt": {"type":"string","description":"Exactly what you will be asked to do at each firing, written so you can do it with no other context."}
    });
    let mut update_props = json!({
        "id": {"type":"string","description":"Schedule id from schedule_list."},
        "name": {"type":"string"},
        "prompt": {"type":"string","description":"New prompt for every future firing."}
    });
    for p in [&mut create_props, &mut update_props] {
        if let (Some(dst), Some(src)) = (p.as_object_mut(), when_props.as_object()) {
            dst.extend(src.clone());
        }
    }
    let id_schema = json!({"type":"object","properties":{
        "id":{"type":"string","description":"Schedule id from schedule_list."}
    },"required":["id"]});
    let mk = |name: &str, desc: &str, schema: Value, effect: Effect, op: Op| -> Arc<dyn Tool> {
        Arc::new(ScheduleTool {
            spec: spec(name, desc, schema, effect, cat),
            op,
            host: Arc::clone(host),
        })
    };
    vec![
        mk(
            "schedule_list",
            "List your own scheduled tasks (id, name, prompt, timing, next runs, origin). Schedules with editable=false were made by the user: you can read them but not change them. You cannot see other bots' schedules.",
            json!({"type":"object","properties":{}}),
            Effect::Read,
            Op::List,
        ),
        mk(
            "schedule_create",
            "Create a scheduled task for yourself: at each firing you are run with `prompt`, unattended, in this chat. The user must approve the prompt and the firing times first. Example: {\"name\":\"Standup\",\"prompt\":\"Summarise open tasks\",\"when\":\"every weekday at 10:30\"}",
            json!({"type":"object","properties":create_props,"required":["name","prompt"]}),
            Effect::Write,
            Op::Create,
        ),
        mk(
            "schedule_update",
            "Change a scheduled task you created (name, prompt or timing). The user must approve the change. Not for schedules the user made.",
            json!({"type":"object","properties":update_props,"required":["id"]}),
            Effect::Write,
            Op::Update,
        ),
        mk(
            "schedule_pause",
            "Pause a scheduled task you created (it stops firing until resumed).",
            id_schema.clone(),
            Effect::Write,
            Op::Pause,
        ),
        mk(
            "schedule_resume",
            "Resume a paused scheduled task you created. The user must approve it again.",
            id_schema.clone(),
            Effect::Write,
            Op::Resume,
        ),
        mk(
            "schedule_delete",
            "Delete a scheduled task you created.",
            id_schema,
            Effect::Write,
            Op::Delete,
        ),
    ]
}

fn caller(ctx: &ToolContext) -> Result<ScheduleCaller, ToolError> {
    let bot_id = ctx
        .bot_id
        .clone()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| exec_err("schedule tools only work inside a bot's run"))?;
    Ok(ScheduleCaller {
        bot_id,
        run_id: ctx.run_id.clone(),
    })
}

fn view_text(v: &ScheduleView) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

impl ScheduleTool {
    fn change(&self, input: &Value) -> Result<Option<ScheduleChange>, ToolError> {
        Ok(match self.op {
            Op::Create => Some(ScheduleChange::Create(parse(input.clone())?)),
            Op::Update => {
                let u: UpdateIn = parse(input.clone())?;
                Some(ScheduleChange::Update {
                    id: u.id,
                    req: u.req,
                })
            }
            Op::Resume => Some(ScheduleChange::Resume {
                id: parse::<IdIn>(input.clone())?.id,
            }),
            Op::Pause => Some(ScheduleChange::Pause {
                id: parse::<IdIn>(input.clone())?.id,
            }),
            Op::Delete => Some(ScheduleChange::Delete {
                id: parse::<IdIn>(input.clone())?.id,
            }),
            Op::List => None,
        })
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn always_ask(&self) -> bool {
        matches!(self.op, Op::Create | Op::Update | Op::Resume)
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
            .map_err(exec_err)?;
        let mut v = serde_json::to_value(&p).map_err(|e| exec_err(e.to_string()))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("kind".into(), json!("schedule"));
        }
        Ok(Some(v))
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let who = caller(ctx)?;
        let h = &self.host;
        match self.op {
            Op::List => {
                let all = h.list(&who).map_err(exec_err)?;
                if all.is_empty() {
                    return Ok(ToolOutput::text("You have no schedules."));
                }
                Ok(ToolOutput::text(
                    serde_json::to_string_pretty(&all).unwrap_or_default(),
                ))
            }
            Op::Create => {
                let req: ScheduleRequest = parse(input)?;
                let v = h.create(&who, &req).map_err(exec_err)?;
                Ok(ToolOutput::text(format!(
                    "Schedule created.\n{}",
                    view_text(&v)
                )))
            }
            Op::Update => {
                let u: UpdateIn = parse(input)?;
                let v = h.update(&who, &u.id, &u.req).map_err(exec_err)?;
                Ok(ToolOutput::text(format!(
                    "Schedule updated.\n{}",
                    view_text(&v)
                )))
            }
            Op::Pause => {
                let id = parse::<IdIn>(input)?.id;
                let v = h.set_enabled(&who, &id, false).map_err(exec_err)?;
                Ok(ToolOutput::text(format!(
                    "Schedule paused.\n{}",
                    view_text(&v)
                )))
            }
            Op::Resume => {
                let id = parse::<IdIn>(input)?.id;
                let v = h.set_enabled(&who, &id, true).map_err(exec_err)?;
                Ok(ToolOutput::text(format!(
                    "Schedule resumed.\n{}",
                    view_text(&v)
                )))
            }
            Op::Delete => {
                let id = parse::<IdIn>(input)?.id;
                let v = h.delete(&who, &id).map_err(exec_err)?;
                Ok(ToolOutput::text(format!(
                    "Schedule deleted.\n{}",
                    view_text(&v)
                )))
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
        fail_preview: bool,
    }

    fn view() -> ScheduleView {
        ScheduleView {
            id: "s1".into(),
            name: "n".into(),
            prompt: "p".into(),
            enabled: true,
            schedule: "Every day at 09:00".into(),
            timezone: "UTC".into(),
            next_runs: vec![],
            last_run: None,
            origin: "bot".into(),
            editable: true,
        }
    }

    impl ScheduleHost for Fake {
        fn list(&self, c: &ScheduleCaller) -> Result<Vec<ScheduleView>, String> {
            self.calls.lock().push(format!("list {}", c.bot_id));
            Ok(vec![])
        }
        fn preview(
            &self,
            c: &ScheduleCaller,
            ch: &ScheduleChange,
        ) -> Result<SchedulePreview, String> {
            self.calls
                .lock()
                .push(format!("preview {} {ch:?}", c.bot_id));
            if self.fail_preview {
                return Err("bad spec".into());
            }
            Ok(SchedulePreview {
                action: "create".into(),
                schedule_id: None,
                name: "n".into(),
                prompt: "do it".into(),
                prompt_chars: 5,
                schedule: "Every day".into(),
                timezone: "UTC".into(),
                next_runs: vec!["a".into(), "b".into(), "c".into()],
                previous: None,
                approval_ref: "tok-1".into(),
            })
        }
        fn discard(&self, c: &ScheduleCaller, ch: &ScheduleChange, token: &str) {
            self.calls
                .lock()
                .push(format!("discard {} {ch:?} {token}", c.bot_id));
        }
        fn create(&self, c: &ScheduleCaller, _: &ScheduleRequest) -> Result<ScheduleView, String> {
            self.calls
                .lock()
                .push(format!("create {} {}", c.bot_id, c.run_id));
            Ok(view())
        }
        fn update(
            &self,
            c: &ScheduleCaller,
            id: &str,
            _: &ScheduleRequest,
        ) -> Result<ScheduleView, String> {
            self.calls.lock().push(format!("update {} {id}", c.bot_id));
            Ok(view())
        }
        fn set_enabled(
            &self,
            c: &ScheduleCaller,
            id: &str,
            e: bool,
        ) -> Result<ScheduleView, String> {
            self.calls
                .lock()
                .push(format!("enabled {} {id} {e}", c.bot_id));
            Ok(view())
        }
        fn delete(&self, c: &ScheduleCaller, id: &str) -> Result<ScheduleView, String> {
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
        tools(&(host.clone() as Arc<dyn ScheduleHost>))
            .into_iter()
            .find(|t| t.spec().name == name)
            .unwrap()
    }

    #[test]
    fn specs_are_system_write_never_meta() {
        let host = Arc::new(Fake::default());
        let all = tools(&(host as Arc<dyn ScheduleHost>));
        let names: Vec<_> = all.iter().map(|t| t.spec().name.clone()).collect();
        assert_eq!(names, SCHEDULE_TOOL_NAMES);
        for t in &all {
            let s = t.spec();
            assert_eq!(s.category, PermissionCategory::System, "{}", s.name);
            assert_ne!(s.category, PermissionCategory::Meta);
            assert!(s.is_valid());
            let want = if s.name == "schedule_list" {
                Effect::Read
            } else {
                Effect::Write
            };
            assert_eq!(s.effect, want, "{}", s.name);
        }
    }

    #[test]
    fn only_create_update_resume_always_ask() {
        let host = Arc::new(Fake::default());
        for t in tools(&(host as Arc<dyn ScheduleHost>)) {
            let want = matches!(
                t.spec().name.as_str(),
                "schedule_create" | "schedule_update" | "schedule_resume"
            );
            assert_eq!(t.always_ask(), want, "{}", t.spec().name);
        }
    }

    #[tokio::test]
    async fn calls_are_scoped_to_the_context_bot() {
        let host = Arc::new(Fake::default());
        let c = ctx(Some("bot-a"));
        tool(&host, "schedule_list")
            .call(&c, json!({}))
            .await
            .unwrap();
        tool(&host, "schedule_create")
            .call(&c, json!({"name":"n","prompt":"p","when":"daily"}))
            .await
            .unwrap();
        tool(&host, "schedule_delete")
            .call(&c, json!({"id":"x"}))
            .await
            .unwrap();
        tool(&host, "schedule_pause")
            .call(&c, json!({"id":"x"}))
            .await
            .unwrap();
        let log = host.calls.lock().clone();
        assert_eq!(
            log,
            [
                "list bot-a",
                "create bot-a run-1",
                "delete bot-a x",
                "enabled bot-a x false"
            ]
        );
    }

    #[tokio::test]
    async fn no_bot_means_no_schedules() {
        let host = Arc::new(Fake::default());
        for ctx in [ctx(None), ctx(Some(""))] {
            let e = tool(&host, "schedule_list")
                .call(&ctx, json!({}))
                .await
                .unwrap_err();
            assert!(e.to_string().contains("inside a bot's run"), "{e}");
        }
        assert!(host.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn approval_details_carry_the_preview() {
        let host = Arc::new(Fake::default());
        let d = tool(&host, "schedule_create")
            .approval_details(
                &ctx(Some("b")),
                &json!({"name":"n","prompt":"do it","cron":"0 9 * * *"}),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d["kind"], "schedule");
        assert_eq!(d["prompt"], "do it");
        assert_eq!(d["next_runs"].as_array().unwrap().len(), 3);
        // Pause / delete show the schedule too (not just an id); list has no card.
        assert!(
            tool(&host, "schedule_delete")
                .approval_details(&ctx(Some("b")), &json!({"id":"x"}))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            tool(&host, "schedule_list")
                .approval_details(&ctx(Some("b")), &json!({}))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_closed_approval_discards_what_the_host_froze() {
        let host = Arc::new(Fake::default());
        let t = tool(&host, "schedule_create");
        let input = json!({"name":"n","prompt":"p","when":"daily"});
        t.approval_closed(&ctx(Some("bot-a")), &input, Some("tok-9"));
        let log = host.calls.lock().clone();
        assert_eq!(log.len(), 1);
        assert!(log[0].starts_with("discard bot-a Create"), "{log:?}");
        assert!(
            log[0].ends_with("tok-9"),
            "only that approval's entry: {log:?}"
        );
        // No token (nothing was previewed), no bot, or a tool without a card: nothing to discard.
        t.approval_closed(&ctx(Some("bot-a")), &input, None);
        t.approval_closed(&ctx(None), &json!({"name":"n","prompt":"p"}), Some("tok"));
        tool(&host, "schedule_list").approval_closed(&ctx(Some("bot-a")), &json!({}), Some("tok"));
        assert_eq!(host.calls.lock().len(), 1);
    }

    #[tokio::test]
    async fn invalid_call_fails_before_approval() {
        let host = Arc::new(Fake {
            fail_preview: true,
            ..Fake::default()
        });
        let e = tool(&host, "schedule_create")
            .approval_details(&ctx(Some("b")), &json!({"name":"n","prompt":"p"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("bad spec"));
        let e = tool(&host, "schedule_update")
            .approval_details(&ctx(Some("b")), &json!({"name":"only, no id"}))
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::InvalidInput(_)));
    }
}

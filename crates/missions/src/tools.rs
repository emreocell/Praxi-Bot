//! The `mission_*` tools: how the bot keeps the ledger of its mission.
//!
//! They are local-state tools (category `Meta`, no external effect, no
//! approval card) and only work inside a mission session: the mission is found
//! from the calling run (`resolve`), never from an argument, so a bot cannot
//! touch another mission. Limits live in the store; the success criterion is
//! checked by code in `mission_complete`.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    ContentClass, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec,
};

use crate::brief::data_value;
use crate::error::MissionError;
use crate::model::{ItemStatus, Mission, MissionState, clip, mentions_captcha};
use crate::store::{ItemUpdate, MissionStore, NewItem};
use crate::supervisor::Clock;

/// Names of the mission tools.
pub const MISSION_TOOL_NAMES: [&str; 5] = [
    "mission_status",
    "mission_plan",
    "mission_item",
    "mission_note",
    "mission_complete",
];

/// Whether `name` is one of the mission tools.
#[must_use]
pub fn is_mission_tool(name: &str) -> bool {
    MISSION_TOOL_NAMES.contains(&name)
}

/// Finds the mission a run belongs to (`None`: not a mission session).
pub type Resolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone, Copy)]
enum Op {
    Status,
    Plan,
    Item,
    Note,
    Complete,
}

struct MissionTool {
    spec: ToolSpec,
    op: Op,
    store: Arc<MissionStore>,
    resolve: Resolver,
    clock: Arc<dyn Clock>,
}

fn spec(name: &str, description: &str, input_schema: Value, effect: Effect) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        effect,
        category: PermissionCategory::Meta,
        platforms: vec![Platform::All],
        content: ContentClass::none(),
    }
}

/// Build the five mission tools over `store`. `resolve` maps a run id to its mission.
#[must_use]
pub fn tools(
    store: &Arc<MissionStore>,
    resolve: &Resolver,
    clock: &Arc<dyn Clock>,
) -> Vec<Arc<dyn Tool>> {
    let status_schema = json!({"type":"object","properties":{}});
    let plan_schema = json!({"type":"object","properties":{
        "items":{"type":"array","maxItems":100,"items":{"type":"object","properties":{
            "key":{"type":"string","description":"Short unique id, e.g. the site's domain."},
            "title":{"type":"string","description":"What to do for this item, one line."},
            "kind":{"type":"string","description":"Label such as site, lead, task (default task)."},
            "facts":{"type":"object","description":"Small facts the next session needs (url, login hint). No secrets."}
        },"required":["key","title"]}}
    },"required":["items"]});
    let item_schema = json!({"type":"object","properties":{
        "key":{"type":"string","description":"The item's key."},
        "status":{"type":"string","enum":["doing","done","blocked","failed","skipped","todo"]},
        "result":{"type":"string","description":"Evidence of what was achieved (done)."},
        "error":{"type":"string","description":"What went wrong (failed)."},
        "reason":{"type":"string","description":"What the owner must do or answer (blocked): 2FA code, payment, a question. A CAPTCHA is not blocked: mark the item failed with the CAPTCHA as the error."}
    },"required":["key","status"]});
    let note_schema = json!({"type":"object","properties":{
        "kind":{"type":"string","enum":["decision","learned","warning"]},
        "text":{"type":"string","description":"One or two sentences."},
        "pinned":{"type":"boolean","description":"Show it in every later brief."}
    },"required":["kind","text"]});
    let complete_schema = json!({"type":"object","properties":{
        "summary":{"type":"string","description":"Short summary of what was achieved."}
    },"required":["summary"]});
    let mk =
        |name: &str, description: &str, schema: Value, effect: Effect, op: Op| -> Arc<dyn Tool> {
            Arc::new(MissionTool {
                spec: spec(name, description, schema, effect),
                op,
                store: Arc::clone(store),
                resolve: Arc::clone(resolve),
                clock: Arc::clone(clock),
            })
        };
    vec![
        mk(
            "mission_status",
            "Mission sessions only: show where the mission stands (progress and the next open items).",
            status_schema,
            Effect::Read,
            Op::Status,
        ),
        mk(
            "mission_plan",
            "Mission sessions only: add items to the mission ledger, one per unit of work (a site, a lead, a document). Keys that already exist are kept as they are.",
            plan_schema,
            Effect::Write,
            Op::Plan,
        ),
        mk(
            "mission_item",
            "Mission sessions only: record the status of one ledger item as you work (doing, done with evidence, failed with the error, blocked with what the owner must do). A blocked item is parked for the owner and the mission goes on with the next item.",
            item_schema,
            Effect::Write,
            Op::Item,
        ),
        mk(
            "mission_note",
            "Mission sessions only: write down a decision, a lesson or a warning so later sessions know it. Pinned notes appear in every brief.",
            note_schema,
            Effect::Write,
            Op::Note,
        ),
        mk(
            "mission_complete",
            "Mission sessions only: say the goal is reached. The runtime checks the ledger against the goal and answers with what is still missing, if anything.",
            complete_schema,
            Effect::Write,
            Op::Complete,
        ),
    ]
}

fn invalid(e: impl std::fmt::Display) -> ToolError {
    ToolError::InvalidInput(e.to_string())
}

fn store_err(e: MissionError) -> ToolError {
    match e {
        MissionError::Invalid(m) => ToolError::InvalidInput(m),
        MissionError::IllegalTransition { .. } | MissionError::NotFound(_) => {
            ToolError::InvalidInput(e.to_string())
        }
        other => ToolError::Execution(other.to_string()),
    }
}

#[derive(Deserialize)]
struct PlanIn {
    items: Vec<PlanItem>,
}

#[derive(Deserialize)]
struct PlanItem {
    key: String,
    title: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    facts: Option<Value>,
}

#[derive(Deserialize)]
struct ItemIn {
    key: String,
    status: String,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct NoteIn {
    kind: String,
    text: String,
    #[serde(default)]
    pinned: bool,
}

#[derive(Deserialize)]
struct CompleteIn {
    summary: String,
}

impl MissionTool {
    fn mission(&self, ctx: &ToolContext) -> Result<Mission, ToolError> {
        let id = (self.resolve)(&ctx.run_id).ok_or_else(|| {
            ToolError::InvalidInput("this tool only works inside a mission session".into())
        })?;
        let m = self
            .store
            .get(&id)
            .map_err(store_err)?
            .ok_or_else(|| ToolError::InvalidInput("the mission no longer exists".into()))?;
        if m.state != MissionState::Running {
            return Err(ToolError::InvalidInput(format!(
                "the mission is {}; stop working on it",
                m.state
            )));
        }
        Ok(m)
    }

    fn status_text(&self, m: &Mission) -> Result<String, ToolError> {
        let p = self.store.progress(&m.id).map_err(store_err)?;
        let items = self.store.items(&m.id, None).map_err(store_err)?;
        let mut s = format!(
            "{} done · {} blocked · {} to do · {} given up · {} skipped (of {}).",
            p.done,
            p.blocked,
            p.open(),
            p.failed,
            p.skipped,
            p.total
        );
        let open: Vec<_> = items
            .iter()
            .filter(|i| i.status.is_open())
            .take(8)
            .collect();
        if !open.is_empty() {
            s.push_str("\nNext open items:");
            for it in open {
                let _ = write!(
                    s,
                    "\n- {} · {}",
                    data_value(&it.key, 120),
                    data_value(&it.title, 160)
                );
            }
        }
        Ok(s)
    }

    fn plan(&self, m: &Mission, input: Value) -> Result<String, ToolError> {
        let i: PlanIn = serde_json::from_value(input).map_err(invalid)?;
        let items: Vec<NewItem> = i
            .items
            .into_iter()
            .map(|p| NewItem {
                key: p.key,
                kind: p
                    .kind
                    .filter(|k| !k.trim().is_empty())
                    .unwrap_or_else(|| "task".into()),
                title: p.title,
                payload: p.facts,
            })
            .collect();
        let r = self
            .store
            .upsert_items(&m.id, &items, self.clock.now_ms())
            .map_err(store_err)?;
        let mut s = format!(
            "Planned: {} added, {} already in the ledger.",
            r.added, r.existing
        );
        if r.dropped > 0 {
            let _ = write!(
                s,
                " {} were refused: the ledger is full ({} items).",
                r.dropped,
                crate::model::MAX_ITEMS
            );
        }
        s.push_str(" Now work through them one after the other without asking.");
        Ok(s)
    }

    fn item(&self, m: &Mission, input: Value) -> Result<String, ToolError> {
        let i: ItemIn = serde_json::from_value(input).map_err(invalid)?;
        let mut status = ItemStatus::parse(i.status.trim()).ok_or_else(|| {
            ToolError::InvalidInput(
                "status must be todo, doing, done, blocked, failed or skipped".into(),
            )
        })?;
        let (mut error, mut reason) = (i.error, i.reason);
        // Policy: a CAPTCHA is not something the owner can answer from the list, and a mission
        // has no live hand-over, so the item is given up with the reason, not parked.
        let captcha =
            status == ItemStatus::Blocked && reason.as_deref().is_some_and(mentions_captcha);
        if captcha {
            status = ItemStatus::Failed;
            error = error.or(reason.take());
        }
        let it = self
            .store
            .update_item(
                &m.id,
                i.key.trim(),
                &ItemUpdate {
                    status: Some(status),
                    result: i.result,
                    error,
                    blocked_reason: reason,
                    live_only: None,
                },
                false,
                self.clock.now_ms(),
            )
            .map_err(store_err)?;
        let p = self.store.progress(&m.id).map_err(store_err)?;
        let mut s = format!(
            "{} is now {}. {} done · {} blocked · {} to do.",
            data_value(&it.key, 120),
            it.status,
            p.done,
            p.blocked,
            p.open()
        );
        if captcha {
            s.push_str(
                " A CAPTCHA cannot be solved here, so the item is given up with that reason (the report lists it). Do not retry the site: take the next item now.",
            );
        } else if it.status == ItemStatus::Blocked {
            s.push_str(" It is parked for the owner. Do not wait: take the next item now.");
        } else if p.open() > 0 {
            s.push_str(" Take the next open item now; do not ask whether to continue.");
        } else {
            s.push_str(" No open item is left: call mission_complete.");
        }
        Ok(s)
    }

    fn note(&self, m: &Mission, input: Value) -> Result<String, ToolError> {
        let i: NoteIn = serde_json::from_value(input).map_err(invalid)?;
        self.store
            .add_note(&m.id, i.kind.trim(), &i.text, i.pinned, self.clock.now_ms())
            .map_err(store_err)?;
        Ok("Noted.".into())
    }

    fn complete(&self, m: &Mission, input: Value) -> Result<String, ToolError> {
        let i: CompleteIn = serde_json::from_value(input).map_err(invalid)?;
        let p = self.store.progress(&m.id).map_err(store_err)?;
        if p.open() > 0 {
            return Ok(format!(
                "Not complete: {} item(s) are still open. Finish them, or mark them failed/blocked/skipped with a reason.",
                p.open()
            ));
        }
        if !p.criterion_met(m.target_done) {
            let why = match (m.target_done, p.total) {
                (_, 0) => "the ledger is empty; plan the work first with mission_plan".to_owned(),
                (Some(n), _) => format!(
                    "the goal needs at least {n} items done and {} are; plan more items with mission_plan, or mark what cannot be done as blocked/failed with the reason",
                    p.done
                ),
                (None, _) => format!(
                    "{} item(s) are still blocked; they wait for the owner, which the owner will be told",
                    p.blocked
                ),
            };
            return Ok(format!("Not complete: {why}."));
        }
        self.store
            .set_state(
                &m.id,
                MissionState::Done,
                Some(&clip(&i.summary, 300)),
                self.clock.now_ms(),
            )
            .map_err(store_err)?;
        Ok("Accepted: the mission is complete. The owner gets the report. Stop now.".into())
    }
}

#[async_trait]
impl Tool for MissionTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Only a bot with a running mission sees the tools; a call outside a mission
    /// session still fails in [`MissionTool::mission`].
    fn visible_to(&self, bot_id: Option<&str>) -> bool {
        bot_id.is_some_and(|b| self.store.bot_has_running_mission(b).unwrap_or(false))
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let m = self.mission(ctx)?;
        let text = match self.op {
            Op::Status => self.status_text(&m)?,
            Op::Plan => self.plan(&m, input)?,
            Op::Item => self.item(&m, input)?,
            Op::Note => self.note(&m, input)?,
            Op::Complete => self.complete(&m, input)?,
        };
        Ok(ToolOutput::text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Budgets;
    use crate::store::NewMission;
    use crate::supervisor::SystemClock;

    struct Fixture {
        store: Arc<MissionStore>,
        tools: Vec<Arc<dyn Tool>>,
        mission: String,
    }

    fn fixture() -> Fixture {
        let store = Arc::new(MissionStore::open_in_memory().unwrap());
        let m = store
            .create(
                &NewMission {
                    user_id: "u".into(),
                    bot_id: "b".into(),
                    chat_id: "c".into(),
                    title: String::new(),
                    goal_text: "goal".into(),
                    target_done: None,
                    budgets: Budgets::default(),
                    items: vec![],
                },
                1,
            )
            .unwrap();
        let id = m.id.clone();
        let resolve: Resolver = Arc::new(move |run| (run == "run-1").then(|| id.clone()));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        Fixture {
            tools: tools(&store, &resolve, &clock),
            store,
            mission: m.id,
        }
    }

    async fn call(f: &Fixture, name: &str, run: &str, input: Value) -> Result<String, ToolError> {
        let tool = f.tools.iter().find(|t| t.spec().name == name).unwrap();
        let ctx = ToolContext {
            run_id: run.into(),
            ..ToolContext::default()
        };
        let out = tool.call(&ctx, input).await?;
        Ok(match &out.content[0] {
            tool_registry::ContentPart::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
    }

    #[test]
    fn every_tool_is_valid_and_local() {
        let f = fixture();
        assert_eq!(f.tools.len(), MISSION_TOOL_NAMES.len());
        for t in &f.tools {
            assert!(t.spec().is_valid(), "{}", t.spec().name);
            assert_eq!(t.spec().category, PermissionCategory::Meta);
            assert!(is_mission_tool(&t.spec().name));
            assert!(!t.spec().content.egress && !t.spec().content.ingests_untrusted);
        }
    }

    #[tokio::test]
    async fn tools_refuse_to_run_outside_a_mission_session() {
        let f = fixture();
        let err = call(&f, "mission_status", "some-other-run", json!({}))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("only works inside a mission session")
        );
    }

    #[tokio::test]
    async fn plan_item_and_complete_walk_the_ledger_to_done() {
        let f = fixture();
        let r = call(
            &f,
            "mission_plan",
            "run-1",
            json!({"items":[
                {"key":"a.com","title":"Sign up","kind":"site"},
                {"key":"b.com","title":"Sign up"}
            ]}),
        )
        .await
        .unwrap();
        assert!(r.contains("2 added"));
        let r = call(&f, "mission_complete", "run-1", json!({"summary":"all"}))
            .await
            .unwrap();
        assert!(r.starts_with("Not complete"), "{r}");
        for k in ["a.com", "b.com"] {
            call(
                &f,
                "mission_item",
                "run-1",
                json!({"key":k,"status":"done","result":"account created"}),
            )
            .await
            .unwrap();
        }
        let r = call(
            &f,
            "mission_complete",
            "run-1",
            json!({"summary":"2 accounts"}),
        )
        .await
        .unwrap();
        assert!(r.starts_with("Accepted"), "{r}");
        let m = f.store.get(&f.mission).unwrap().unwrap();
        assert_eq!(m.state, MissionState::Done);
        // Once done, the tools stop working for that run.
        assert!(
            call(&f, "mission_status", "run-1", json!({}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn blocked_needs_a_reason_and_tells_the_model_to_move_on() {
        let f = fixture();
        call(
            &f,
            "mission_plan",
            "run-1",
            json!({"items":[{"key":"a.com","title":"x"},{"key":"b.com","title":"y"}]}),
        )
        .await
        .unwrap();
        let err = call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"blocked"}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("needs a reason"));
        let r = call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"blocked","reason":"needs a card payment"}),
        )
        .await
        .unwrap();
        assert!(r.contains("parked for the owner") && r.contains("next item"));
    }

    #[tokio::test]
    async fn a_captcha_is_given_up_with_its_reason_not_parked_for_the_owner() {
        let f = fixture();
        call(
            &f,
            "mission_plan",
            "run-1",
            json!({"items":[{"key":"a.com","title":"x"},{"key":"b.com","title":"y"}]}),
        )
        .await
        .unwrap();
        let r = call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"blocked","reason":"hCaptcha on the sign-up form"}),
        )
        .await
        .unwrap();
        assert!(r.contains("CAPTCHA") && r.contains("given up"), "{r}");
        let items = f.store.items(&f.mission, None).unwrap();
        assert_eq!(items[0].status, ItemStatus::Failed);
        assert_eq!(
            items[0].last_error.as_deref(),
            Some("hCaptcha on the sign-up form")
        );
        assert_eq!(items[0].blocked_reason, None);
        assert_eq!(f.store.progress(&f.mission).unwrap().blocked, 0);
    }

    #[tokio::test]
    async fn an_item_started_too_often_is_refused_with_what_to_do_instead() {
        let f = fixture();
        call(
            &f,
            "mission_plan",
            "run-1",
            json!({"items":[{"key":"a.com","title":"x"}]}),
        )
        .await
        .unwrap();
        for _ in 0..crate::model::MAX_ITEM_ATTEMPTS {
            for status in ["doing", "todo"] {
                call(
                    &f,
                    "mission_item",
                    "run-1",
                    json!({"key":"a.com","status":status}),
                )
                .await
                .unwrap();
            }
        }
        let err = call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"doing"}),
        )
        .await
        .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("mark it failed") && text.contains("blocked"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn the_model_cannot_complete_with_open_or_blocked_items_and_cannot_reopen_done_ones() {
        let f = fixture();
        call(
            &f,
            "mission_plan",
            "run-1",
            json!({"items":[{"key":"a.com","title":"x"}]}),
        )
        .await
        .unwrap();
        call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"blocked","reason":"2FA"}),
        )
        .await
        .unwrap();
        let r = call(&f, "mission_complete", "run-1", json!({"summary":"done!"}))
            .await
            .unwrap();
        assert!(r.contains("still blocked"), "{r}");
        assert_eq!(
            f.store.get(&f.mission).unwrap().unwrap().state,
            MissionState::Running
        );

        call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"done","result":"ok"}),
        )
        .await
        .unwrap();
        let err = call(
            &f,
            "mission_item",
            "run-1",
            json!({"key":"a.com","status":"todo"}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("finished items stay finished"));
    }

    #[tokio::test]
    async fn notes_are_validated() {
        let f = fixture();
        assert_eq!(
            call(
                &f,
                "mission_note",
                "run-1",
                json!({"kind":"decision","text":"used bot@example.com","pinned":true})
            )
            .await
            .unwrap(),
            "Noted."
        );
        assert!(
            call(
                &f,
                "mission_note",
                "run-1",
                json!({"kind":"rant","text":"x"})
            )
            .await
            .is_err()
        );
        assert_eq!(f.store.notes(&f.mission, true).unwrap().len(), 1);
    }
}

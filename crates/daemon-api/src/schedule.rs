//! Scheduler wiring: the [`DaemonExecutor`] that turns a firing schedule into
//! a normal agent run (initiator `Schedule`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopError, ScheduleWork};
use async_trait::async_trait;
use builtin_tools::GateRequest;
use protocol::{BotId, ChatId, UserId};
use scheduler::{Payload, Schedule, ScheduleExecutor, SchedulerError};
use serde_json::Value;
use storage::{Chat, Storage, now_ms};
use task_runtime::RunState;
use tool_registry::{Effect, PermissionCategory};

/// How often a firing looks at its run.
const POLL: Duration = Duration::from_millis(100);

/// Runs a schedule's payload through the agent loop:
///
/// * `AgentPrompt` is a new turn in the schedule's chat (or a chat named after
///   the schedule);
/// * `Workflow` is replayed by workflow-replay through the loop's gate;
/// * `Tool` calls the tool registry through the loop's gate.
///
/// The run is a normal run: events, approvals, history.  `execute` returns when
/// the run is done or paused (nobody answered an approval in time, or the
/// daemon is shutting down), so the scheduler's per-schedule concurrency limit
/// of 1 covers the whole run.
pub(crate) struct DaemonExecutor {
    pub agent: AgentLoop,
    pub storage: Arc<Storage>,
    pub store: Arc<scheduler::ScheduleStore>,
    pub user_id: UserId,
    pub leader: crate::host::LeaderControl,
}

fn exec_err(e: impl std::fmt::Display) -> SchedulerError {
    SchedulerError::Execution(e.to_string())
}

/// Workflow parameters as the replayer wants them: string to string.
fn string_params(v: &Value) -> Result<BTreeMap<String, String>, SchedulerError> {
    match v {
        Value::Null => Ok(BTreeMap::new()),
        Value::Object(m) => Ok(m
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str().map_or_else(|| v.to_string(), str::to_owned),
                )
            })
            .collect()),
        _ => Err(exec_err("workflow params must be a JSON object")),
    }
}

/// What the loop does for a payload.
pub(crate) fn work_of(payload: &Payload) -> Result<ScheduleWork, SchedulerError> {
    Ok(match payload {
        Payload::AgentPrompt { text } => ScheduleWork::Prompt(text.clone()),
        Payload::Workflow { name, params } => ScheduleWork::Workflow {
            name: name.clone(),
            params: string_params(params)?,
        },
        Payload::Tool { name, input } => ScheduleWork::Tool {
            name: name.clone(),
            input: input.clone(),
        },
    })
}

impl DaemonExecutor {
    fn reusable_chat(&self, id: ChatId) -> Result<bool, SchedulerError> {
        Ok(!self
            .storage
            .list_messages(id)
            .map_err(exec_err)?
            .iter()
            .any(|m| m.role == "external_schedule"))
    }

    /// The chat a firing posts to: the schedule's own, else the newest chat of
    /// the bot titled like the schedule, else a new one.
    fn chat_for(&self, s: &Schedule) -> Result<ChatId, SchedulerError> {
        if s.external_input.is_none() && s.precheck.is_none() {
            if let Some(id) = s.chat_id {
                let chat = self
                    .storage
                    .get_chat(id)
                    .map_err(exec_err)?
                    .filter(|c| c.user_id == self.user_id)
                    .ok_or_else(|| exec_err(format!("chat {id} of schedule {} is gone", s.id)))?;
                if self.reusable_chat(chat.id)? {
                    return Ok(chat.id);
                }
            }
            let mut existing: Vec<_> = self
                .storage
                .list_chats(self.user_id)
                .map_err(exec_err)?
                .into_iter()
                .filter(|c| c.bot_id == s.bot_id && c.title == s.name)
                .collect();
            existing.sort_by_key(|c| std::cmp::Reverse(c.created_at_ms));
            for chat in existing {
                if self.reusable_chat(chat.id)? {
                    return Ok(chat.id);
                }
            }
        }
        let chat = Chat {
            id: ChatId::new(),
            user_id: self.user_id,
            bot_id: s.bot_id,
            title: s.name.clone(),
            created_at_ms: now_ms(),
        };
        self.storage.insert_chat(&chat).map_err(exec_err)?;
        Ok(chat.id)
    }
}

#[async_trait]
impl ScheduleExecutor for DaemonExecutor {
    async fn precheck(
        &self,
        schedule: &Schedule,
        previous: Option<&str>,
    ) -> scheduler::Result<scheduler::precheck::Outcome> {
        let check = schedule
            .precheck
            .as_ref()
            .ok_or_else(|| exec_err("precheck missing"))?;
        scheduler::precheck::evaluate(
            check,
            previous,
            self.agent.schedule_read_host(schedule.bot_id, schedule.id),
        )
        .await
    }

    async fn poll_trigger(
        &self,
        schedule: &Schedule,
        trigger: &scheduler::Trigger,
    ) -> scheduler::Result<Vec<scheduler::watch::Observation>> {
        poll_trigger(
            &self.agent,
            &self.storage,
            schedule.bot_id,
            schedule.id,
            trigger,
        )
        .await
    }

    async fn execute(&self, schedule: &Schedule) -> scheduler::Result<()> {
        self.execute_inner(schedule, None).await
    }

    async fn execute_occurrence(&self, schedule: &Schedule, due_ms: i64) -> scheduler::Result<()> {
        self.execute_inner(schedule, Some(due_ms)).await
    }
}

impl DaemonExecutor {
    async fn execute_inner(
        &self,
        schedule: &Schedule,
        due_ms: Option<i64>,
    ) -> scheduler::Result<()> {
        self.storage
            .get_bot(schedule.bot_id)
            .map_err(exec_err)?
            .filter(|b| b.user_id == self.user_id)
            .ok_or_else(|| exec_err(format!("bot {} of schedule is gone", schedule.bot_id)))?;
        let chat = self.chat_for(schedule)?;
        let external = schedule.external_input.is_some() || schedule.precheck.is_some();
        let mut payload = schedule.payload.clone();
        if let Some(input) = &schedule.external_input {
            payload = match &payload {
                Payload::AgentPrompt { text } => Payload::AgentPrompt {
                    text: format!(
                        "{}\n\n[Untrusted external schedule data, JSON encoded]\n{}",
                        text.replace("{input}", "[external schedule data below]"),
                        serde_json::to_string(input).map_err(exec_err)?
                    ),
                },
                other => other.with_webhook_input(input),
            };
        }
        let work = work_of(&payload)?;
        let run = self
            .leader
            .with_active(|| {
                if external {
                    self.agent.run_scheduled_external(
                        chat,
                        schedule.id,
                        work,
                        schedule
                            .precheck_failed
                            .then_some(scheduler::precheck::FAILURE_NOTE),
                    )
                } else {
                    self.agent.run_scheduled(chat, schedule.id, work)
                }
            })
            .map_err(exec_err)?
            .map_err(|e| match e {
                LoopError::Busy(c) => exec_err(format!("chat {c} is busy with another turn")),
                other => exec_err(other),
            })?;
        if let Some(due) = due_ms {
            let _ = self
                .store
                .set_occurrence_run_id(schedule.id, due, &run.to_string());
        }
        // Poll rather than `AgentLoop::wait`: that would take the run's task
        // handle away from `AgentLoop::shutdown`.
        loop {
            match self.agent.runtime().state(run).map_err(exec_err)? {
                RunState::Succeeded | RunState::Paused => return Ok(()),
                RunState::Failed => {
                    let why = self
                        .storage
                        .get_run(run)
                        .ok()
                        .flatten()
                        .and_then(|r| r.error)
                        .unwrap_or_else(|| "failed".to_owned());
                    let _ = why;
                    return Err(exec_err("scheduled run failed"));
                }
                RunState::Cancelled => return Err(exec_err(format!("run {run} was cancelled"))),
                RunState::Queued | RunState::Running | RunState::WaitingApproval => {
                    tokio::time::sleep(POLL).await;
                }
            }
        }
    }
}

fn watch_roots(
    agent: &AgentLoop,
    storage: &Storage,
) -> Result<Vec<std::path::PathBuf>, SchedulerError> {
    let (roots, _) = agent.checkpoint_access();
    if !roots.is_empty() {
        return Ok(roots);
    }
    crate::schedule_tools::load_schedule_limits(storage.data_dir())
        .map(|c| c.watch_roots)
        .map_err(|_| exec_err("invalid schedule watcher configuration"))
}

/// Revalidate current policy on every poll, with intent audit before source I/O.
async fn poll_trigger(
    agent: &AgentLoop,
    storage: &Storage,
    bot: BotId,
    id: protocol::TaskId,
    trigger: &scheduler::Trigger,
) -> scheduler::Result<Vec<scheduler::watch::Observation>> {
    match trigger {
        scheduler::Trigger::FileWatch { path, glob, .. } => {
            let input = serde_json::json!({"path": path, "glob": glob});
            agent
                .schedule_read_gate(bot, id)
                .authorize(&GateRequest {
                    run_id: format!("watch-{id}"),
                    tool: "fs_list".into(),
                    category: PermissionCategory::Fs,
                    effect: Effect::Read,
                    raw_command: String::new(),
                    input,
                    content: builtin_tools::classify("fs_list")
                        .unwrap_or_else(tool_registry::ContentClass::unspecified),
                    control: None,
                })
                .await
                .map_err(exec_err)?;
            let roots = watch_roots(agent, storage)?;
            let data = storage.data_dir().to_path_buf();
            let path = path.clone();
            let glob = glob.clone();
            let files = tokio::task::spawn_blocking(move || {
                builtin_tools::files::watch_snapshot(&data, &roots, &path, &glob)
            })
            .await
            .map_err(|_| exec_err("file watcher failed"))?
            .map_err(|_| exec_err("file watcher boundary or snapshot failed"))?;
            Ok(files
                .into_iter()
                .map(|file| {
                    let path = agent.redact_host_text(file.path.to_string_lossy().into_owned());
                    scheduler::watch::Observation {
                        key: path.clone(),
                        fingerprint: file.fingerprint,
                        input: Value::String(path),
                    }
                })
                .collect())
        }
        scheduler::Trigger::Email {
            account,
            from_filter,
            subject_filter,
        } => {
            let output = agent.schedule_read_host(bot,id).call_tool("email_search", serde_json::json!({"account":account,"folder":"INBOX","from":from_filter,"subject":subject_filter,"limit":100})).await.map_err(exec_err)?;
            email_observations(&output, from_filter, subject_filter.as_deref())
        }
        scheduler::Trigger::Webhook { .. } => Err(exec_err("webhooks are not polled")),
    }
}

fn email_observations(
    output: &Value,
    from_filter: &str,
    subject_filter: Option<&str>,
) -> scheduler::Result<Vec<scheduler::watch::Observation>> {
    let messages = output
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| exec_err("email header response unavailable"))?;
    let mut out = Vec::new();
    for message in messages {
        let from = message
            .get("from")
            .and_then(Value::as_str)
            .ok_or_else(|| exec_err("email sender unavailable"))?;
        let subject = message
            .get("subject")
            .and_then(Value::as_str)
            .ok_or_else(|| exec_err("email subject unavailable"))?;
        // Filter again locally. Connector and fake providers may return
        // a wider result; only headers participate, never a body.
        if !from.to_lowercase().contains(&from_filter.to_lowercase())
            || subject_filter
                .as_ref()
                .is_some_and(|s| !subject.to_lowercase().contains(&s.to_lowercase()))
        {
            continue;
        }
        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                message
                    .get("uid")
                    .and_then(Value::as_u64)
                    .map(|u| format!("uid:{u}"))
            })
            .ok_or_else(|| exec_err("email identity unavailable"))?;
        if message_id.len() > 512 || from.len() > 2048 || subject.len() > 2048 {
            return Err(exec_err("email header limit exceeded"));
        }
        out.push(scheduler::watch::Observation {
            key: message_id.clone(),
            fingerprint: "header".into(),
            input: serde_json::json!({"message_id":message_id,"from":from,"subject":subject}),
        });
    }
    Ok(out)
}

/// Creation/update validation. Dynamic JS calls are checked at their actual
/// invocation; known literal tool calls are also refused before persistence.
pub(crate) fn validate_precheck(
    agent: &AgentLoop,
    bot: BotId,
    check: &scheduler::Precheck,
) -> Result<(), String> {
    check
        .validate()
        .map_err(|_| "invalid precheck definition")?;
    match check {
        scheduler::Precheck::HashChanged { tool, input } => {
            agent.validate_schedule_read(bot, tool, input)
        }
        scheduler::Precheck::Script { code } => {
            for tail in code.split("tools.call").skip(1) {
                let tail = tail.trim_start();
                let Some(tail) = tail.strip_prefix('(') else {
                    continue;
                };
                let tail = tail.trim_start();
                let Some(quote) = tail.chars().next().filter(|q| *q == '\'' || *q == '"') else {
                    continue;
                };
                let Some(end) = tail[1..].find(quote) else {
                    continue;
                };
                agent.validate_schedule_read(bot, &tail[1..=end], &Value::Null)?;
            }
            Ok(())
        }
    }
}

pub(crate) async fn validate_trigger(
    agent: &AgentLoop,
    storage: &Storage,
    bot: BotId,
    id: protocol::TaskId,
    trigger: &scheduler::Trigger,
) -> Result<(), String> {
    trigger
        .validate()
        .map_err(|_| "invalid trigger definition")?;
    match trigger {
        scheduler::Trigger::Webhook { .. } => Ok(()),
        scheduler::Trigger::Email { .. } => {
            agent.validate_schedule_read(bot, "email_search", &Value::Null)
        }
        scheduler::Trigger::FileWatch { .. } => poll_trigger(agent, storage, bot, id, trigger)
            .await
            .map(|_| ())
            .map_err(|_| "file watch requires an allowed, bounded workspace directory".into()),
    }
}

#[cfg(test)]
mod c225_tests {
    use super::*;
    use agent_core::{BotProfile, LoopConfig, Providers};
    use permission_engine::Policy;
    use protocol::{TaskId, UserId};
    use serde_json::json;
    use storage::{Bot, User};
    use tool_registry::{
        ContentPart, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec,
    };
    struct FakeImapHeaders {
        spec: ToolSpec,
        messages: parking_lot::Mutex<Vec<Value>>,
        calls: parking_lot::Mutex<Vec<Value>>,
    }
    #[async_trait]
    impl Tool for FakeImapHeaders {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }
        async fn call(&self, _: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
            self.calls.lock().push(input);
            Ok(ToolOutput {
                content: vec![ContentPart::Json {
                    data: json!({"messages":self.messages.lock().clone()}),
                }],
                spill: None,
                exposure: None,
            })
        }
    }
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Full registered-gate/header/persistence integration.
    async fn c225_email_poll_filters_headers_once_never_requests_or_forwards_a_body() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let user = UserId::new();
        let bot = BotId::new();
        storage
            .insert_user(&User {
                id: user,
                name: "fixture".into(),
                created_at_ms: 0,
            })
            .unwrap();
        storage
            .insert_bot(&Bot {
                id: bot,
                user_id: user,
                name: "fixture".into(),
                config: json!({}),
                created_at_ms: 0,
            })
            .unwrap();
        let headers = Arc::new(FakeImapHeaders {
            spec: ToolSpec {
                name: "email_search".into(),
                description: "fake headers".into(),
                input_schema: json!({"type":"object"}),
                effect: Effect::Read,
                category: PermissionCategory::Messaging,
                platforms: vec![Platform::All],
                content: tool_registry::ContentClass::none(),
            },
            messages: parking_lot::Mutex::new(vec![]),
            calls: parking_lot::Mutex::new(vec![]),
        });
        let registry = ToolRegistry::new();
        registry.register(headers.clone());
        let agent = AgentLoop::new(
            storage.clone(),
            registry,
            Providers::single(Arc::new(crate::fake::FakeProvider)),
            LoopConfig {
                global_policy: Policy::full(),
                ..LoopConfig::default()
            },
        );
        agent.add_profile(
            bot,
            BotProfile::from_toml_str(
                "[bot]\nid='fixture'\nname='Fixture'\n[models]\nexecutor='fake/m'\n",
            )
            .unwrap(),
        );
        let trigger = scheduler::Trigger::Email {
            account: "fake".into(),
            from_filter: "ann@example.test".into(),
            subject_filter: Some("invoice".into()),
        };
        let id = TaskId::new();
        let mut state = scheduler::watch::WatchState::default();
        state
            .observe(
                &trigger,
                0,
                poll_trigger(&agent, &storage, bot, id, &trigger)
                    .await
                    .unwrap(),
            )
            .unwrap();
        *headers.messages.lock() = vec![
            json!({"uid":1,"from":"ann@example.test","subject":"Invoice 1","body":"POISON_BODY_MUST_NEVER_APPEAR"}),
            json!({"uid":2,"from":"other@example.test","subject":"Invoice 2"}),
            json!({"uid":3,"from":"ann@example.test","subject":"Unrelated"}),
        ];
        state
            .observe(
                &trigger,
                120_000,
                poll_trigger(&agent, &storage, bot, id, &trigger)
                    .await
                    .unwrap(),
            )
            .unwrap();
        let payload = state.take_ready(&trigger, 120_000).unwrap();
        let records: Vec<Value> = serde_json::from_str(&payload).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["message_id"], "uid:1");
        assert!(!payload.contains("POISON_BODY"));
        assert!(records[0].get("body").is_none());
        state
            .observe(
                &trigger,
                240_000,
                poll_trigger(&agent, &storage, bot, id, &trigger)
                    .await
                    .unwrap(),
            )
            .unwrap();
        assert!(state.take_ready(&trigger, 240_000).is_none());
        assert!(headers.calls.lock().iter().all(|v| v["folder"] == "INBOX"
            && v["from"] == "ann@example.test"
            && v.get("body").is_none()));
        let empty = scheduler::Trigger::Email {
            account: "fake".into(),
            from_filter: " ".into(),
            subject_filter: None,
        };
        assert!(empty.validate().is_err());
    }
}

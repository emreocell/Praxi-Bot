//! Scheduler wiring: the [`DaemonExecutor`] that turns a firing schedule into
//! a normal agent run (initiator `Schedule`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use agent_core::{AgentLoop, LoopError, ScheduleWork};
use async_trait::async_trait;
use protocol::{ChatId, UserId};
use scheduler::{Payload, Schedule, ScheduleExecutor, SchedulerError};
use serde_json::Value;
use storage::{Chat, Storage, now_ms};
use task_runtime::RunState;

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
    pub user_id: UserId,
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
    /// The chat a firing posts to: the schedule's own, else the newest chat of
    /// the bot titled like the schedule, else a new one.
    fn chat_for(&self, s: &Schedule) -> Result<ChatId, SchedulerError> {
        if let Some(id) = s.chat_id {
            let chat = self
                .storage
                .get_chat(id)
                .map_err(exec_err)?
                .filter(|c| c.user_id == self.user_id)
                .ok_or_else(|| exec_err(format!("chat {id} of schedule {} is gone", s.id)))?;
            return Ok(chat.id);
        }
        let existing = self
            .storage
            .list_chats(self.user_id)
            .map_err(exec_err)?
            .into_iter()
            .filter(|c| c.bot_id == s.bot_id && c.title == s.name)
            .max_by_key(|c| c.created_at_ms);
        if let Some(c) = existing {
            return Ok(c.id);
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
    async fn execute(&self, schedule: &Schedule) -> scheduler::Result<()> {
        self.storage
            .get_bot(schedule.bot_id)
            .map_err(exec_err)?
            .filter(|b| b.user_id == self.user_id)
            .ok_or_else(|| exec_err(format!("bot {} of schedule is gone", schedule.bot_id)))?;
        let chat = self.chat_for(schedule)?;
        let work = work_of(&schedule.payload)?;
        let run = self
            .agent
            .run_scheduled(chat, schedule.id, work)
            .map_err(|e| match e {
                LoopError::Busy(c) => exec_err(format!("chat {c} is busy with another turn")),
                other => exec_err(other),
            })?;
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
                    return Err(exec_err(format!("run {run} failed: {why}")));
                }
                RunState::Cancelled => return Err(exec_err(format!("run {run} was cancelled"))),
                RunState::Queued | RunState::Running | RunState::WaitingApproval => {
                    tokio::time::sleep(POLL).await;
                }
            }
        }
    }
}

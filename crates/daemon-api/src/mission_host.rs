//! Daemon side of missions: the [`SessionRunner`] that starts a mission session as an
//! agent run, and the lifecycle (open the ledger, start and stop the supervisor).
//!
//! The supervisor, the ledger and the brief live in the `missions` crate; this module only
//! connects them to the agent loop and to the leader-only component start.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::{AgentLoop, LoopError};
use async_trait::async_trait;
use missions::supervisor::{
    Clock, SessionEnd, SessionRequest, SessionResult, SessionRunner, Supervisor, SupervisorConfig,
    SystemClock,
};
use missions::{DB_FILE, Mission, MissionStore};
use protocol::{BotId, ChatId, RunId, UserId};
use storage::{Message, Storage, now_ms};
use task_runtime::RunState;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::mission_wakes::{DaemonWakeChecks, OwnInboxProbe};
use crate::{AppState, error::ApiError};

/// How long a session waits for the mission chat to be free (the owner may be typing in it).
const CHAT_BUSY_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// How often a session that waits for the mission chat looks again.
const CHAT_BUSY_POLL: Duration = Duration::from_secs(2);

/// Starts mission sessions through the agent loop.
pub struct AgentSessionRunner {
    agent: AgentLoop,
    storage: Arc<Storage>,
    /// Told when a mission ends or blocks, so the owner hears of it on Telegram, Discord and in
    /// the app (a weak handle: the state owns the supervisor that owns this runner).
    notify: Option<crate::WeakAppState>,
}

impl AgentSessionRunner {
    /// A runner over `agent`; sessions run in the missions' own chats.
    #[must_use]
    pub fn new(agent: AgentLoop, storage: Arc<Storage>) -> Self {
        Self {
            agent,
            storage,
            notify: None,
        }
    }

    /// Also push the digest of a mission that ends or blocks to the owner's channels.
    #[must_use]
    pub(crate) fn with_notices(mut self, state: crate::WeakAppState) -> Self {
        self.notify = Some(state);
        self
    }
}

fn parse<T>(s: &str, wrap: fn(Uuid) -> T) -> Option<T> {
    Uuid::parse_str(s).ok().map(wrap)
}

#[async_trait]
impl SessionRunner for AgentSessionRunner {
    fn bot_ready(&self, bot_id: &str) -> Result<(), String> {
        let bot = parse(bot_id, BotId).ok_or("the mission's bot id is invalid")?;
        match self.storage.get_bot(bot) {
            Ok(Some(_)) => {}
            Ok(None) => return Err("the bot of this mission no longer exists".into()),
            Err(e) => return Err(format!("the bot could not be read: {e}")),
        }
        if self.agent.bot_is_full(bot) {
            Ok(())
        } else {
            Err("the bot is no longer on the Full level; missions run unattended only there".into())
        }
    }

    fn session_steps(&self) -> u32 {
        self.agent.session_max_steps()
    }

    async fn run_session(
        &self,
        req: SessionRequest,
        on_started: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> SessionResult {
        let failed = |why: String| SessionResult {
            end: SessionEnd::Failed(why),
            model_steps: 0,
            cost_usd: 0.0,
        };
        let Some(chat) = parse(&req.chat_id, ChatId) else {
            return failed("the mission's chat id is invalid".into());
        };
        let cancelled = || SessionResult {
            end: SessionEnd::Cancelled,
            model_steps: 0,
            cost_usd: 0.0,
        };
        let started = Instant::now();
        let run = loop {
            // The owner paused or stopped the mission, or the daemon is going down: no run
            // is started for a session nobody wants any more.
            if req.cancel.is_cancelled() {
                return cancelled();
            }
            match self
                .agent
                .run_mission_session(chat, &req.mission_id, &req.brief)
            {
                Ok(run) => break run,
                Err(LoopError::Busy(_)) if started.elapsed() < CHAT_BUSY_PATIENCE => {
                    tokio::select! {
                        () = req.cancel.cancelled() => return cancelled(),
                        () = tokio::time::sleep(CHAT_BUSY_POLL) => {}
                    }
                }
                Err(e) => return failed(e.to_string()),
            }
        };
        on_started(&run.to_string());
        // `wait` hands out the run's task once, so it is polled to the end: a stop only
        // cancels the run and the same wait then sees it finish.
        let waiter = self.agent.wait(run);
        tokio::pin!(waiter);
        let ended = tokio::select! {
            ended = &mut waiter => ended,
            () = req.cancel.cancelled() => {
                let _ = self.agent.cancel(run);
                waiter.await
            }
        };
        let stats = self.agent.session_stats(run);
        let end = match ended {
            Ok(RunState::Succeeded) if stats.guard_stopped => SessionEnd::GuardStop,
            Ok(RunState::Succeeded) => SessionEnd::Finished,
            Ok(RunState::Cancelled) => SessionEnd::Cancelled,
            Ok(RunState::Paused) => SessionEnd::Paused,
            Ok(other) => SessionEnd::Failed(
                self.storage
                    .get_run(run)
                    .ok()
                    .flatten()
                    .and_then(|r| r.error)
                    .unwrap_or_else(|| format!("the session ended in state {other}")),
            ),
            Err(e) => SessionEnd::Failed(e.to_string()),
        };
        SessionResult {
            end,
            model_steps: stats.model_steps,
            cost_usd: stats.cost_usd,
        }
    }

    fn cancel_run(&self, run_id: &str) {
        if let Some(run) = parse(run_id, RunId) {
            let _ = self.agent.cancel(run);
        }
    }

    async fn announce(&self, mission: &Mission, text: &str) {
        if let Some(state) = self.notify.as_ref().and_then(crate::WeakAppState::upgrade) {
            state.notify_mission_concluded(mission);
        }
        let (Some(user), Some(chat)) = (
            parse(&mission.user_id, UserId),
            parse(&mission.chat_id, ChatId),
        ) else {
            return;
        };
        let _ = self.storage.insert_message(&Message {
            id: Uuid::now_v7(),
            user_id: user,
            chat_id: chat,
            run_id: None,
            role: "assistant".to_owned(),
            content: text.to_owned(),
            created_at_ms: now_ms(),
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        });
    }
}

/// A clock and a pacing that replace the real ones (see [`AppState::set_mission_timing`]).
pub(crate) struct MissionTiming {
    clock: Arc<dyn Clock>,
    pacing: SupervisorConfig,
}

/// The running supervisor of this daemon.
pub(crate) struct MissionRuntime {
    pub(crate) supervisor: Arc<Supervisor>,
    task: JoinHandle<()>,
    token: CancellationToken,
}

impl MissionRuntime {
    pub(crate) async fn stop(self) {
        self.token.cancel();
        let _ = self.task.await;
    }
}

impl AppState {
    /// Run missions on `clock` and at `pacing` instead of the wall clock and the default pauses.
    /// Only for an embedder that must not wait in real time (the evals drive the whole daemon on a
    /// manual clock); call it before [`Self::enable_missions`]. The first call wins.
    pub fn set_mission_timing(&self, clock: Arc<dyn Clock>, pacing: SupervisorConfig) {
        let _ = self.0.mission_timing.set(MissionTiming { clock, pacing });
    }

    fn mission_clock(&self) -> Arc<dyn Clock> {
        self.0.mission_timing.get().map_or_else(
            || Arc::new(SystemClock) as Arc<dyn Clock>,
            |t| Arc::clone(&t.clock),
        )
    }

    /// Open `<data dir>/missions.db`, give the agent loop the `mission_*` tools and keep the
    /// store for the routes and the supervisor. [`serve`](crate::serve)'s embedder calls this
    /// once after [`AppState::new`]; without it `/v1/missions` answers `503`.
    ///
    /// # Errors
    /// The ledger cannot be opened.
    pub fn enable_missions(&self) -> Result<usize, missions::MissionError> {
        let store = Arc::new(MissionStore::open(self.data_dir().join(DB_FILE))?);
        let clock = self.mission_clock();
        let checks: Arc<dyn missions::wake::WakeChecks> = Arc::new(DaemonWakeChecks {
            storage: self.storage_arc(),
            mail: self.0.agent.mail_handle(),
            user_id: self.user_id(),
        });
        let n = self.0.agent.enable_missions(&store, &clock, Some(&checks));
        let _ = self.0.mission_store.set(store);
        Ok(n)
    }

    /// The ledger, or `503` when missions are not enabled.
    pub(crate) fn mission_store(&self) -> Result<Arc<MissionStore>, ApiError> {
        self.0
            .mission_store
            .get()
            .cloned()
            .ok_or_else(|| ApiError::unavailable("missions are not enabled on this daemon"))
    }

    /// The running supervisor, or `503` (not the leader, safe mode, shutting down).
    pub(crate) fn mission_supervisor(&self) -> Result<Arc<Supervisor>, ApiError> {
        self.0
            .missions
            .lock()
            .as_ref()
            .map(|r| Arc::clone(&r.supervisor))
            .ok_or_else(|| ApiError::unavailable("missions are not running on this computer"))
    }

    /// Start the supervisor (leader components only). A second call is a no-op.
    pub(crate) fn start_missions(&self) {
        let Some(store) = self.0.mission_store.get().cloned() else {
            return;
        };
        let mut slot = self.0.missions.lock();
        if slot.is_some() {
            return;
        }
        let token = self.0.shutdown.child_token();
        let runner = Arc::new(
            AgentSessionRunner::new(self.0.agent.clone(), self.storage_arc())
                .with_notices(self.downgrade()),
        );
        let (clock, pacing) = self.0.mission_timing.get().map_or_else(
            || {
                (
                    Arc::new(SystemClock) as Arc<dyn Clock>,
                    SupervisorConfig::default(),
                )
            },
            |t| (Arc::clone(&t.clock), t.pacing.clone()),
        );
        let supervisor = Arc::new(Supervisor::new(store, runner, clock, pacing, token.clone()));
        supervisor.set_mail_probe(Arc::new(OwnInboxProbe {
            mail: self.0.agent.mail_handle(),
        }));
        let weak = self.downgrade();
        let notice = weak.clone();
        self.0.agent.set_mission_handover_notify(Arc::new(move |n| {
            if let Some(state) = notice.upgrade() {
                state.notify_mission_handover(n);
            }
        }));
        // A parked CAPTCHA the supervisor expired (or a restart lost): its page is let go.
        let ended = weak.clone();
        supervisor.set_handover_ended_hook(Arc::new(move |h| {
            if let Some(state) = ended.upgrade() {
                let call_id = h.call_id.clone();
                tokio::spawn(async move { state.agent().release_parked_handover(&call_id).await });
            }
        }));
        supervisor.set_change_hook(Arc::new(move |id| {
            if let Some(state) = weak.upgrade() {
                state.notify_mission_changed(id);
            }
        }));
        let task = supervisor.spawn();
        *slot = Some(MissionRuntime {
            supervisor,
            task,
            token,
        });
    }

    /// Stop the supervisor: the session runs in flight are cancelled and every driver is
    /// joined before this returns (see [`Supervisor::spawn`]), so a supervisor started after a
    /// leader change never runs next to a leftover one. The missions stay `running` and
    /// continue from their ledger at the next start.
    pub(crate) async fn stop_missions(&self) {
        let runtime = self.0.missions.lock().take();
        if let Some(r) = runtime {
            r.stop().await;
        }
    }
}

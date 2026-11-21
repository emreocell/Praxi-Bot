//! The public [`AgentLoop`] handle.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use agent_router::{ToolSurface, ToolVisibility};
use builtin_tools::BuiltinConfig;
use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use permission_engine::{PermissionResolver, Policy};
use protocol::event::{RunError, RunFinished, RunStarted};
use protocol::{
    BotId, ChatId, Event, InitiatorKind, PermissionCategory, RunId, TaskId, ToolCallId, UserId,
};
use storage::{Message as DbMessage, Storage, now_ms};
use task_runtime::{RunState, Runtime, RuntimeError, StepStatus};
use tokio::sync::broadcast;
use tool_registry::ToolRegistry;
use uuid::Uuid;

use crate::approval::{LoopGate, grant_detail, grant_target};
use crate::config::{LoopConfig, ModelPrice};
use crate::control::{ControlGate, LoopControl};
use crate::exec::TurnExecutor;
use crate::history::{status_of, step_output};
use crate::profile::BotProfile;
use crate::shared::{
    ApprovalScope, COMPUTER_IDENTITY_UNBOUND_NOTICE, COMPUTER_PREPARE_TIMEOUT_NOTICE,
    CategoryDecision, Delivery, EffectivePolicy, Grant, GrantTarget, Grants, LoopError,
    MessageMode, ModelOut, Pending, Providers, STEP_MODEL, STEP_NUDGE, STEP_TOOLS, STEP_USER,
    ScheduleWork, Shared, StreamItem, Verdict, initiator_of, kind_for, kind_for_mention,
    kind_for_schedule, mention_message_id,
};
use crate::{AUDIT_GRANT_ALWAYS, AUDIT_GRANT_INERT, AUDIT_GRANT_REVOKED};

type Result<T> = std::result::Result<T, LoopError>;

#[derive(Clone, Copy)]
pub(crate) enum Start {
    Fresh,
    Resume,
}

/// Runs bot turns: model -> tool calls -> permission -> execute -> repeat.
///
/// Cheap to clone (shared handles).  Spawning methods must be called inside a
/// Tokio runtime.
#[derive(Clone)]
pub struct AgentLoop {
    pub(crate) sh: Arc<Shared>,
    pub(crate) rt: Arc<Runtime>,
}

/// File access scope of a bot, read from its profile (`[files]`). A caller
/// that is not a known bot gets no file access.
struct LoopFsAccess {
    sh: Weak<Shared>,
}

impl builtin_tools::FsAccessProvider for LoopFsAccess {
    fn scope(&self, bot_id: Option<&str>) -> builtin_tools::FsScope {
        let denied = || builtin_tools::FsScope::Folders(Vec::new());
        let Some(sh) = self.sh.upgrade() else {
            return denied();
        };
        let Some(bot) = bot_id.and_then(|b| b.parse::<Uuid>().ok()).map(BotId) else {
            return denied();
        };
        let Ok(profile) = sh.profile_for(bot) else {
            return denied();
        };
        profile.files.scope(&sh.unset_file_scope())
    }
}

impl AgentLoop {
    /// Build a loop and reload the persisted "always allow" grant rules.
    /// Use [`Self::with_builtin_tools`] to register the built-in tool set.
    #[must_use]
    pub fn new(
        storage: Arc<Storage>,
        tools: ToolRegistry,
        providers: Providers,
        cfg: LoopConfig,
    ) -> Self {
        let providers = providers.with_redactor(cfg.redactor.clone());
        let (events, _) = broadcast::channel(4096);
        let first_gseq = storage.last_gseq().unwrap_or(0);
        // The discovery meta-tools only reveal what the calling bot may see.
        let sh = Arc::new_cyclic(|weak: &Weak<Shared>| {
            let weak = weak.clone();
            let visibility: ToolVisibility = Arc::new(move |ctx, spec| {
                weak.upgrade()
                    .is_some_and(|sh| sh.tool_visible_to_run(&ctx.run_id, spec))
            });
            Shared {
                usage: crate::usage::Ledger::default(),
                storage: Arc::clone(&storage),
                surface: ToolSurface::with_visibility(
                    tools.clone(),
                    cfg.discovery_threshold,
                    visibility,
                ),
                tools,
                fs_unset_denied: RwLock::new(None),
                checkpoint_access: RwLock::new((Vec::new(), Vec::new())),
                providers: RwLock::new(providers),
                global_policy: RwLock::new(cfg.global_policy.clone()),
                policy_locked: std::sync::atomic::AtomicBool::new(false),
                spend: crate::spend::SpendState::default(),
                control: RwLock::new(None),
                computer_provider: RwLock::new(None),
                server_links: RwLock::new(None),
                static_desktop: RwLock::new(None),
                run_computers: Mutex::new(HashMap::new()),
                wait_pc_timers: Mutex::new(HashMap::new()),
                computer_slots: Arc::new(tokio::sync::Semaphore::new(
                    cfg.max_computer_runs
                        .clamp(1, tokio::sync::Semaphore::MAX_PERMITS),
                )),
                resolver: PermissionResolver::new(),
                cfg,
                profiles: RwLock::new(HashMap::new()),
                events,
                runs: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                grants: Mutex::new(load_grants(&storage)),
                active: Mutex::new(HashMap::new()),
                queued: Mutex::new(HashMap::new()),
                mention_chain: Mutex::new(HashMap::new()),
                tasks: Mutex::new(HashMap::new()),
                bg: Mutex::new(Vec::new()),
                gseq: AtomicU64::new(first_gseq),
                emit_gate: Mutex::new(()),
                send_gate: Mutex::new(()),
                shutting_down: AtomicBool::new(false),
                rt: OnceLock::new(),
                spawn_gate: Mutex::new(()),
                skills: OnceLock::new(),
                fill_secrets: OnceLock::new(),
                secret_requests: Mutex::new(HashMap::new()),
                skill_hints: RwLock::new(None),
                router: crate::router::Router::default(),
                directives: Mutex::new(HashMap::new()),
                approval_paused: Mutex::new(HashSet::new()),
                identity_paused: Mutex::new(HashSet::new()),
                parked: Mutex::new(HashMap::new()),
                exec_gate: Mutex::new(()),
                native_runs: Mutex::new(HashMap::new()),
                #[cfg(test)]
                task_reap_gate: Mutex::new(None),
                #[cfg(test)]
                task_reap_entered: AtomicBool::new(false),
                #[cfg(test)]
                task_reap_done: AtomicBool::new(false),
                #[cfg(test)]
                exec_commit_hook: Mutex::new(None),
            }
        });
        let rt = Arc::new(Runtime::new(
            storage,
            Arc::new(TurnExecutor {
                sh: Arc::clone(&sh),
            }),
        ));
        let _ = sh.rt.set(Arc::downgrade(&rt));
        Self { sh, rt }
    }

    pub(crate) fn from_parts(sh: Arc<Shared>, rt: Arc<Runtime>) -> Self {
        Self { sh, rt }
    }

    /// Register `spawn_subagent` (see the `subagent` module): the model can
    /// delegate tasks to child runs of its own bot.
    #[must_use]
    pub fn with_subagents(self) -> Self {
        let _ = self.sh.tools.register(crate::subagent::tool());
        self
    }

    /// Register the `skill_*` tools over `index` and put the skills index
    /// (suggested skills first, see [`skills::SkillIndex::prompt_block`]) into
    /// the system prompt of every model call.
    #[must_use]
    pub fn with_skills(self, index: skills::SkillIndex) -> Self {
        for tool in skills::tools(&index) {
            let _ = self.sh.tools.register(tool);
        }
        let _ = self.sh.skills.set(index);
        self
    }

    /// Register the model-callable memory and session tools (`memory_search`,
    /// `session_search`, `memory_save`, `memory_forget`) when
    /// [`LoopConfig::memory`] is configured and enabled. When memory is `None`
    /// or disabled, no memory tools are registered.
    #[must_use]
    pub fn with_memory_tools(self) -> Self {
        if let Some(m) = self.sh.cfg.memory.as_ref().filter(|m| m.enabled) {
            let redactor = self.sh.cfg.redactor.clone();
            let secret_checker: Option<memory_store::tools::SecretChecker> = redactor.map(|r| {
                Arc::new(move |text: &str| r.contains_secret(text))
                    as memory_store::tools::SecretChecker
            });
            for tool in memory_store::tools::tools(
                &m.store,
                &self.sh.storage,
                m.recall_budget_tokens,
                secret_checker.as_ref(),
            ) {
                let _ = self.sh.tools.register(tool);
            }
        }
        self
    }

    /// Tell the loop what is in the foreground (browser page host, application)
    /// so skills that match it are listed as suggested; without a source the
    /// turn's text is matched against skill keywords instead.
    pub fn set_skill_hints(&self, hints: crate::SkillHintFn) {
        *self.sh.skill_hints.write() = Some(hints);
    }

    /// Register the built-in tools (desktop, browser, files, shell, script,
    /// `ask_user`, `todo_write`) into the loop's registry.  The config's
    /// [`ToolGate`](builtin_tools::ToolGate) is replaced by the loop's own, so
    /// tool calls made from `script_run` go through the same resolver and
    /// interactive approval flow as direct calls.
    #[must_use]
    pub fn with_builtin_tools(self, mut cfg: BuiltinConfig) -> Self {
        (*self.sh.static_desktop.write()).clone_from(&cfg.desktop);
        cfg.gate = Arc::new(LoopGate {
            sh: Arc::downgrade(&self.sh),
        });
        cfg.control = Some(Arc::new(LoopControl {
            sh: Arc::downgrade(&self.sh),
        }));
        *self.sh.checkpoint_access.write() =
            (cfg.fs_allowed_roots.clone(), cfg.fs_granted_paths.clone());
        // Static roots chosen by the host keep their meaning; otherwise each
        // bot's own file access setting decides.
        if cfg.fs_access.is_none() && cfg.fs_allowed_roots.is_empty() {
            cfg.fs_access = Some(Arc::new(LoopFsAccess {
                sh: Arc::downgrade(&self.sh),
            }));
        }
        if let Some(secrets) = &cfg.fill_secrets {
            let _ = self.sh.fill_secrets.set(Arc::clone(secrets));
        }
        let _ = builtin_tools::register_builtin_tools(&self.sh.tools, cfg);
        self
    }

    /// Current filesystem roots and exact credential grants, from the registered file tools.
    #[must_use]
    pub fn checkpoint_access(&self) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        self.sh.checkpoint_access.read().clone()
    }

    /// Decide per call whether bots without an explicit file access mode get
    /// none (`true`, an always-on host) or "This computer" (`false`).
    pub fn set_unset_file_access_denied(&self, denied: impl Fn() -> bool + Send + Sync + 'static) {
        *self.sh.fs_unset_denied.write() = Some(Arc::new(denied));
    }

    /// Like [`Self::checkpoint_access`] for a run of `bot`: its CURRENT file
    /// access setting decides (folder-only roots; an empty or unusable folder
    /// list denies every path), so narrowing a bot also narrows review/restore.
    #[must_use]
    pub fn checkpoint_access_for(
        &self,
        bot: BotId,
    ) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        let (mut roots, grants) = self.checkpoint_access();
        match self
            .sh
            .profile_for(bot)
            .map(|p| p.files.scope(&self.sh.unset_file_scope()))
        {
            Ok(builtin_tools::FsScope::Folders(f)) if !f.is_empty() => roots = f,
            // Folder mode without a folder (or an unset mode on a host): nothing resolves.
            Ok(builtin_tools::FsScope::Folders(_)) => {
                roots = vec![self.sh.storage.data_dir().join("__no-folder__")];
            }
            // This computer, or a profile that cannot be read: the static access.
            Ok(builtin_tools::FsScope::Computer) | Err(_) => {}
        }
        (roots, grants)
    }

    /// Apply the production redactor, including its configured source refresher.
    #[must_use]
    pub fn redact_host_text(&self, text: String) -> String {
        self.sh.refresh_redactor();
        self.sh.redact(text)
    }

    /// Register extra tools that need the loop's permission gate (for example
    /// `workflow_run`, whose inner steps must pass the same approval flow as
    /// direct calls). `build` receives that gate.
    #[must_use]
    pub fn with_gated_tools<F>(self, build: F) -> Self
    where
        F: FnOnce(Arc<dyn builtin_tools::ToolGate>) -> Vec<Arc<dyn tool_registry::Tool>>,
    {
        let gate: Arc<dyn builtin_tools::ToolGate> = Arc::new(LoopGate {
            sh: Arc::downgrade(&self.sh),
        });
        for tool in build(gate) {
            let _ = self.sh.tools.register(tool);
        }
        self
    }

    /// Install the desktop control gate (the daemon registers its bridge to the
    /// desktop app here); see [`ControlGate`].
    pub fn set_control_gate(&self, gate: Arc<dyn ControlGate>) {
        *self.sh.control.write() = Some(gate);
    }

    /// Install the runtime computer availability provider (`serve` wires host
    /// display and connected remote PC desktops here).
    pub fn set_computer_provider(&self, provider: Arc<dyn computer_target::ComputerProvider>) {
        *self.sh.computer_provider.write() = Some(provider);
    }

    /// Install the paired servers a bot may use as its own computer
    /// (`[runtime].computer = "server:<key>"`) while its brain stays here.
    pub fn set_server_links(&self, links: Arc<dyn builtin_tools::pc::ServerLinks>) {
        *self.sh.server_links.write() = Some(links);
    }

    /// Names of the registered tools, sorted.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.sh.tools.list().into_iter().map(|s| s.name).collect();
        names.sort();
        names
    }

    /// Configured memory store, if the memory hook is attached to this loop.
    #[must_use]
    pub fn memory_store(&self) -> Option<Arc<memory_store::MemoryStore>> {
        self.sh.cfg.memory.as_ref().map(|m| Arc::clone(&m.store))
    }

    /// Recall token budget configured on this loop (defaults to 800).
    #[must_use]
    pub fn memory_recall_budget_tokens(&self) -> usize {
        self.sh
            .cfg
            .memory
            .as_ref()
            .map_or(800, |m| m.recall_budget_tokens)
    }

    /// Mask registered secrets in external data before an authorized source
    /// adapter persists or embeds it. Source adapters refresh the secret
    /// snapshot once per admitted batch with [`Self::refresh_external_redactor`].
    #[must_use]
    pub fn redact_external_text(&self, text: String) -> String {
        self.sh.redact(text)
    }

    /// Refresh registered secrets once before admitting an external source
    /// batch. Individual lines can then be redacted without secret-store I/O.
    pub fn refresh_external_redactor(&self) {
        self.sh.refresh_redactor();
    }

    /// The loop's tool registry (cheap clone). MCP servers register their
    /// tools here, so a turn can call them.
    #[must_use]
    pub fn tool_registry(&self) -> ToolRegistry {
        self.sh.tools.clone()
    }

    /// Audit outgoing card coverage against the registry actually assembled by
    /// the host, including optional and dynamically registered tools.
    /// # Errors
    /// Names the first schema field omitted by its outgoing payload renderer.
    pub fn audit_egress_card_schemas(&self) -> std::result::Result<Vec<String>, String> {
        let mut audited = Vec::new();
        for spec in self
            .sh
            .tools
            .list()
            .into_iter()
            .filter(|s| s.content.egress)
        {
            let Some(properties) = spec
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            else {
                continue;
            };
            let input = serde_json::Value::Object(
                properties
                    .keys()
                    .map(|key| {
                        (
                            key.clone(),
                            serde_json::Value::String(format!("SNTNL_{key}_END")),
                        )
                    })
                    .collect(),
            );
            let (_, payload, _) = crate::taint::egress_payload(&spec.name, &input);
            for key in properties.keys() {
                if !payload.contains(&format!("SNTNL_{key}_END")) {
                    return Err(format!("{} omits outgoing schema field {key}", spec.name));
                }
            }
            audited.push(spec.name);
        }
        Ok(audited)
    }

    /// The loop's providers (cheap clone).
    #[must_use]
    pub fn providers(&self) -> Providers {
        self.sh.providers.read().clone()
    }

    /// Replace the providers (after `POST /v1/providers/reload`); runs already
    /// streaming keep the provider they started with.
    pub fn set_providers(&self, providers: Providers) {
        *self.sh.providers.write() = providers.with_redactor(self.sh.cfg.redactor.clone());
        // The K2 embedder and the K3 classifier come from the providers.
        self.sh.router.reset();
    }

    /// The workspace-wide default permission policy.
    #[must_use]
    pub fn global_policy(&self) -> Policy {
        self.sh.global_policy.read().clone()
    }

    /// Resolve query-embedding egress in the actual active run's policy scope.
    /// Unknown/cross-bot run IDs fail closed. Owner source opt-in remains an
    /// independent host requirement; generic tool grants are not consulted.
    #[must_use]
    pub fn embedding_egress_decision(
        &self,
        bot_id: BotId,
        run_id: &str,
    ) -> permission_engine::Decision {
        use permission_engine::{ActionRequest, Decision};
        let Ok(run_id) = run_id.parse::<Uuid>().map(RunId) else {
            return Decision::Deny;
        };
        let _gate = self.sh.exec_gate.lock();
        // Never reconstruct a context from a caller-supplied persisted run ID.
        // Only a currently executing run may exercise its captured initiator
        // and inherited subagent policy.
        if self.rt.state(run_id).ok() != Some(RunState::Running) {
            return Decision::Deny;
        }
        let Some(ctx) = self
            .sh
            .runs
            .lock()
            .get(&run_id)
            .filter(|ctx| ctx.bot_id == bot_id)
            .cloned()
        else {
            return Decision::Deny;
        };
        if self
            .sh
            .policy_locked
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Decision::Deny;
        }
        let Ok(current) = self.sh.profile_for(bot_id) else {
            return Decision::Deny;
        };
        let policy = self.sh.global_policy.read();
        let request = ActionRequest {
            cwd: None,
            category: PermissionCategory::Remote,
            raw_command: String::new(),
            initiator: ctx.initiator,
        };
        self.sh
            .resolver
            .resolve(&policy, ctx.bot_policy.as_ref(), None, &request)
            .stricter(
                self.sh
                    .resolver
                    .resolve(&policy, current.policy().as_ref(), None, &request),
            )
    }

    /// Replace the workspace-wide default policy.
    ///
    /// Bumps the policy revision, then denies pending approvals that the new
    /// policy hard-denies (the tool is not run and no grant is created). The
    /// next tool call of every run sees the new policy.
    pub fn set_global_policy(&self, policy: Policy) {
        // Same mutex the approve path holds for its post-approve recheck and
        // again immediately before the tool runs. A deny cannot land in the
        // gap where the command would still run and an `Always` grant stick.
        let _gate = self.sh.exec_gate.lock();
        crate::approval::install_global_policy(&self.sh, policy);
    }

    /// Lock (or unlock) deny-all mode: while locked every tool decision except
    /// agent-internal `Meta` tools is `Deny`, bot-specific policies included.
    pub fn set_policy_locked(&self, locked: bool) {
        self.sh
            .policy_locked
            .store(locked, std::sync::atomic::Ordering::SeqCst);
    }

    /// USD price of `model` (full `provider/model` reference): the loop config's
    /// override, else the price the providers' catalog carries.
    #[must_use]
    pub fn price_for(&self, model: &str) -> Option<ModelPrice> {
        self.sh.price_for(model)
    }

    /// The monthly spending limit in USD (`None` = off).
    #[must_use]
    pub fn spend_limit(&self) -> Option<f64> {
        *self.sh.spend.limit.read()
    }

    /// Set or clear the monthly spending limit. Effective for the next model
    /// call of every run; the caller validates the value and persists it.
    pub fn set_spend_limit(&self, limit_usd: Option<f64>) {
        *self.sh.spend.limit.write() = limit_usd;
        // A new limit starts a fresh 80% warning cycle.
        *self.sh.spend.warned_month.lock() = None;
    }

    /// Month-to-date spend of `user` (local calendar month).
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn spend_status(
        &self,
        user: protocol::UserId,
    ) -> storage::Result<crate::spend::SpendStatus> {
        crate::spend::spend_status(&self.sh.storage, user, storage::now_ms())
    }

    /// Owner kill switch: end every running native-tools engine process of
    /// `bot_id` (the whole process tree). Returns how many were signalled.
    #[allow(clippy::must_use_candidate)] // Callers may ignore the count.
    pub fn kill_native_runs(&self, bot_id: BotId) -> usize {
        let runs = self
            .sh
            .native_runs
            .lock()
            .remove(&bot_id)
            .unwrap_or_default();
        for (_, token) in &runs {
            token.cancel();
        }
        runs.len()
    }

    /// Register the profile used by `bot_id` (otherwise the bot row's JSON
    /// `config` is parsed as a profile).
    pub fn add_profile(&self, bot_id: BotId, profile: BotProfile) {
        self.sh.profiles.write().insert(bot_id, Arc::new(profile));
        self.sh.router.reset_bot(bot_id);
    }

    /// Wire saved workflows into the loop: registers `workflow_run` /
    /// `workflow_list` (their inner steps pass the loop's approval flow), lets
    /// the decision ladder route requests to workflows and replay them without
    /// a model call, and forwards the replay's events to the run's stream.
    /// `handles.tools` is replaced by the loop's own registry.
    #[must_use]
    pub fn with_workflows(
        self,
        store: workflow_replay::WorkflowStore,
        mut handles: workflow_replay::Handles,
    ) -> Self {
        handles.tools = self.sh.tools.clone();
        let weak = Arc::downgrade(&self.sh);
        let opts = workflow_replay::ReplayOptions {
            events: Some(Arc::new(move |ev: Event| {
                let Some(sh) = weak.upgrade() else { return };
                let Some(run_id) = crate::router::event_run_id(&ev) else {
                    return;
                };
                let ctx = sh.runs.lock().get(&run_id).cloned();
                if let Some(ctx) = ctx {
                    sh.emit(&ctx, ev);
                }
            })),
            // Same control gate as the desktop/browser tools: a replay is refused
            // while the human has the wheel.
            control: Some(Arc::new(LoopControl {
                sh: Arc::downgrade(&self.sh),
            })),
            fill_secrets: self.sh.fill_secrets.get().cloned(),
            ..workflow_replay::ReplayOptions::default()
        };
        let (s2, h2, o2) = (store.clone(), handles.clone(), opts.clone());
        let this = self.with_gated_tools(|gate| workflow_replay::tools(s2, h2, gate, o2));
        let _ = this.sh.router.workflows.set(crate::router::WorkflowWiring {
            store,
            handles,
            opts,
        });
        this
    }

    /// A saved workflow was edited or deleted: drop the routes cached for it
    /// (K1) and rebuild the router's candidates at the next turn.
    pub fn workflow_changed(&self, name: &str) {
        self.sh.router.workflow_changed(name);
    }

    /// Cache the route a succeeded run took (`POST /v1/runs/{id}/remember`):
    /// the same request text then goes straight there (K1).  A run whose model
    /// ran a saved workflow (`workflow_run`) remembers that workflow.  Returns
    /// the remembered route as JSON (tagged by `kind`).
    ///
    /// # Errors
    /// [`LoopError::NotFound`] for an unknown run, [`LoopError::Router`] when
    /// the run is not eligible (not succeeded, a subagent, no route or
    /// workflow, router disabled) or the cache refuses it.
    pub fn remember_route(&self, run_id: RunId) -> Result<serde_json::Value> {
        let route = self.sh.remember_route(run_id)?;
        Ok(serde_json::to_value(route).unwrap_or(serde_json::Value::Null))
    }

    /// Whether [`AgentLoop::remember_route`] would accept `run_id` (same
    /// checks, nothing is cached).
    #[must_use]
    pub fn can_remember(&self, run_id: RunId) -> bool {
        self.sh.can_remember(run_id)
    }

    /// Subscribe to the live event stream of all runs (events are also
    /// appended to each run's NDJSON log).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<StreamItem> {
        self.sh.events.subscribe()
    }

    /// Append `event` to `run_id`'s durable event log and broadcast it on
    /// [`Self::subscribe`]. Used for post-run actions (such as checkpoint restore)
    /// when `run_id` is no longer active.
    pub fn emit_run_event(&self, run_id: RunId, event: Event) {
        if let Some(ctx) = self.sh.runs.lock().get(&run_id).cloned() {
            self.sh.emit(&ctx, event);
            return;
        }
        let parent_run_id = self
            .sh
            .storage
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.parent_run_id);
        let journal = self
            .sh
            .cfg
            .journal
            .get_or_init(|| storage::EventJournal::fs(Arc::clone(&self.sh.storage)));
        let gate = self.sh.emit_gate.lock();
        journal.flush();
        let next_seq = storage::read_event_log(self.sh.storage.data_dir(), run_id)
            .map_or(0, |v| v.last().map_or(0, |e| e.seq + 1));
        let env = protocol::Envelope::new(next_seq, event).with_parent(parent_run_id);
        let gseq = self.sh.gseq.fetch_add(1, Ordering::SeqCst) + 1;
        journal.append(storage::JournalRecord {
            gseq,
            run_id,
            envelope: env.clone(),
        });
        let ticket = self.sh.send_gate.lock();
        drop(gate);
        journal.flush();
        let _ = self.sh.events.send(StreamItem {
            gseq,
            run_id,
            envelope: env,
        });
        drop(ticket);
    }

    /// The durable runtime (state queries, etc.).
    #[must_use]
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.rt
    }

    /// Start a turn for a human message and return its run id immediately;
    /// the turn runs in the background.
    ///
    /// # Errors
    /// Fails if the chat / bot profile is missing, the chat already has an
    /// active turn ([`LoopError::Busy`]) or storage fails.
    pub fn run_turn(&self, chat_id: ChatId, user_message: &str) -> Result<RunId> {
        self.run_turn_as(chat_id, user_message, InitiatorKind::Human)
    }

    /// Like [`Self::run_turn`] with an explicit initiator kind (policy rules
    /// can depend on it, ADR-0012).
    ///
    /// # Errors
    /// As [`Self::run_turn`].
    pub fn run_turn_as(
        &self,
        chat_id: ChatId,
        user_message: &str,
        initiator: InitiatorKind,
    ) -> Result<RunId> {
        self.start_turn(chat_id, user_message, &kind_for(initiator), None, None)
    }

    /// Human-only replay entry: parameter values never enter persisted turn
    /// text, route inputs or model context. Lost memory (restart) fails closed.
    ///
    /// # Errors
    /// Invalid parameters, missing redaction, or the usual turn-start errors.
    pub fn run_workflow(
        &self,
        chat_id: ChatId,
        name: &str,
        params: BTreeMap<String, String>,
    ) -> Result<RunId> {
        let chat = self
            .sh
            .storage
            .get_chat(chat_id)?
            .ok_or_else(|| LoopError::NotFound("chat".into()))?;
        if !self
            .sh
            .profile_for(chat.bot_id)
            .map_err(LoopError::Profile)?
            .router
            .enabled
        {
            return Err(LoopError::Router(
                "workflow commands require the bot router".into(),
            ));
        }
        let wiring = self
            .sh
            .router
            .workflows
            .get()
            .ok_or_else(|| LoopError::Router("workflows unavailable".into()))?;
        let wf = wiring
            .store
            .for_bot(&chat.bot_id.to_string())
            .and_then(|store| store.get(name))
            .map_err(|_| LoopError::Router("workflow unavailable for this bot".into()))?;
        wf.bind_params(
            &params
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        )
        .map_err(|_| LoopError::Router("invalid workflow parameters".into()))?;
        let redactor = self
            .sh
            .cfg
            .redactor
            .as_ref()
            .ok_or_else(|| LoopError::Router("redaction unavailable".into()))?;
        // The shared redactor deliberately ignores short strings. Refuse them
        // here instead of allowing credentials in echoed errors or approvals.
        if params.values().any(|v| v.len() < 6) {
            return Err(LoopError::Router(
                "runtime values must contain at least six bytes for safe redaction".into(),
            ));
        }
        for value in params.values() {
            redactor.register(value);
        }
        let ticket = Uuid::now_v7().to_string();
        self.sh.router.human_replays.lock().insert(
            ticket.clone(),
            crate::router::HumanReplay {
                bot: chat.bot_id,
                run: None,
                name: name.to_owned(),
                params,
            },
        );
        let result = self.start_turn(
            chat_id,
            &format!("Run workflow {name}"),
            &kind_for(InitiatorKind::Human),
            Some(ScheduleWork::Workflow {
                name: name.to_owned(),
                params: BTreeMap::from([("__human_replay".into(), ticket.clone())]),
            }),
            None,
        );
        if result.is_err() {
            self.sh.router.human_replays.lock().remove(&ticket);
        }
        result
    }

    /// Registry restricted to the tools visible to this bot.
    ///
    /// # Errors
    /// The bot profile is missing or invalid.
    pub fn workflow_recording_tools(&self, bot: BotId) -> Result<ToolRegistry> {
        let profile = self.sh.profile_for(bot).map_err(LoopError::Profile)?;
        let registry = ToolRegistry::new();
        for spec in self.sh.visible_specs(&profile, bot) {
            if let Some(tool) = self.sh.tools.get(&spec.name) {
                registry.register(tool);
            }
        }
        Ok(registry)
    }

    /// Whether every user message of this top-level run came from the owner
    /// (desktop app or the paired admin desktop). Token-only clients,
    /// Telegram and peers do not count. Fails closed.
    #[must_use]
    pub fn workflow_run_owner_started(&self, run: &storage::Run) -> bool {
        crate::approval::owner_started_run(&self.sh.storage, run.id, run.parent_run_id)
    }

    /// Whether this run carried untrusted or private content before or while
    /// it ran, rebuilt from storage (parent chain, child runs, prior chat
    /// provenance, persisted exposure facts). The in-memory taint set is lost
    /// on restart and is not shared with subagents, so a recording must not
    /// rely on it. Fails closed when the run has no chat.
    #[must_use]
    pub fn workflow_run_carried_exposure(&self, run: &storage::Run) -> bool {
        let Some(chat_id) = run.chat_id else {
            return true;
        };
        let mut taint = self.sh.initial_taint(run, chat_id);
        // `initial_taint` only sees the bounded model-context window; a
        // tainted earlier message may have scrolled out of it. Recording
        // looks at every earlier message and fails closed on a storage error.
        match self
            .sh
            .storage
            .chat_exposure_until(chat_id, run.id, run.created_at_ms)
        {
            Ok((untrusted, private, _)) => taint.exposure.observe(untrusted, private),
            Err(_) => return true,
        }
        taint.exposure != permission_engine::ContentExposure::clean()
    }

    /// Test strings with the loop's current known-value and pattern redactor.
    #[must_use]
    pub fn workflow_contains_secret(&self, value: &str) -> bool {
        self.sh
            .cfg
            .redactor
            .as_ref()
            .is_some_and(|r| r.contains_secret(value))
    }

    /// Start an external MCP task in a fresh, isolated chat. The credential id
    /// is public attribution, never the bearer. Every tool requires one-time
    /// owner approval, including reads and inherited child calls.
    ///
    /// # Errors
    /// Existing chat history, unavailable bot, invalid task, or storage error.
    pub fn run_external(
        &self,
        chat_id: ChatId,
        credential_id: Uuid,
        work: ScheduleWork,
    ) -> Result<RunId> {
        let text = match &work {
            ScheduleWork::Prompt(text) => text.clone(),
            ScheduleWork::Tool { .. } => "External agent requested a computer tool call.".into(),
            ScheduleWork::Workflow { .. } => {
                return Err(LoopError::Profile(
                    "external workflow directives are not supported".into(),
                ));
            }
        };
        let directive = (!matches!(work, ScheduleWork::Prompt(_))).then_some(work);
        self.start_turn(
            chat_id,
            &text,
            &format!("turn:external_agent:{credential_id}"),
            directive,
            None,
        )
    }

    /// Start a run on behalf of schedule `schedule_id` (initiator
    /// [`InitiatorKind::Schedule`]) and return its run id; the run goes on in
    /// the background.  `work` says what it does: a normal prompt turn, a saved
    /// workflow replayed through the loop's permission gate, or one tool call
    /// through the same gate.  Approvals apply as for any run; when nobody
    /// answers within [`LoopConfig::schedule_approval_timeout`] the run
    /// pauses (resumable with [`Self::resume`]).  `RunStarted` carries the
    /// `schedule_id`.
    ///
    /// # Errors
    /// As [`Self::run_turn`].
    pub fn run_scheduled(
        &self,
        chat_id: ChatId,
        schedule_id: TaskId,
        work: ScheduleWork,
    ) -> Result<RunId> {
        let text = match &work {
            ScheduleWork::Prompt(t) => t.clone(),
            ScheduleWork::Workflow { name, params } => {
                format!("Scheduled: run the saved workflow `{name}` with {params:?}.")
            }
            ScheduleWork::Tool { name, input } => {
                format!("Scheduled: call the tool `{name}` with {input}.")
            }
        };
        let directive = (!matches!(work, ScheduleWork::Prompt(_))).then_some(work);
        self.start_turn(
            chat_id,
            &text,
            &kind_for_schedule(schedule_id),
            directive,
            None,
        )
    }

    /// Start an externally supplied scheduled run in a new, isolated chat.
    /// The durable initiator survives resume and is inherited by all children.
    ///
    /// # Errors
    /// Existing chat history or normal run-start errors.
    pub fn run_scheduled_external(
        &self,
        chat_id: ChatId,
        schedule_id: TaskId,
        work: ScheduleWork,
        note: Option<&str>,
    ) -> Result<RunId> {
        if !self.sh.storage.list_messages(chat_id)?.is_empty() {
            return Err(LoopError::Profile(
                "external schedule requires an empty chat".into(),
            ));
        }
        let mut text = match &work {
            ScheduleWork::Prompt(text) => text.clone(),
            ScheduleWork::Workflow { name, params } => {
                format!("Scheduled workflow {name}: {params:?}")
            }
            ScheduleWork::Tool { name, input } => format!("Scheduled tool {name}: {input}"),
        };
        if let Some(note) = note {
            text.push_str("\n\n");
            text.push_str(note);
        }
        let directive = (!matches!(work, ScheduleWork::Prompt(_))).then_some(work);
        self.start_turn(
            chat_id,
            &text,
            &format!("turn:external_schedule:{schedule_id}"),
            directive,
            None,
        )
    }

    /// `handoff`: the just-concluded run that still holds the chat's `active`
    /// slot; the new turn takes the slot over from it without a gap in which
    /// another start could slip in.
    fn start_turn(
        &self,
        chat_id: ChatId,
        user_message: &str,
        kind: &str,
        directive: Option<ScheduleWork>,
        handoff: Option<RunId>,
    ) -> Result<RunId> {
        let source = match initiator_of(kind) {
            InitiatorKind::Schedule | InitiatorKind::ExternalSchedule => "schedule",
            InitiatorKind::ExternalAgent => "external_agent",
            InitiatorKind::Channel => "channel",
            _ => "unknown",
        };
        self.start_turn_from(chat_id, user_message, kind, directive, handoff, source)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_turn_from(
        &self,
        chat_id: ChatId,
        user_message: &str,
        kind: &str,
        directive: Option<ScheduleWork>,
        handoff: Option<RunId>,
        source: &'static str,
    ) -> Result<RunId> {
        // `@Y` in the user's own message: Y answers this turn in this chat (up
        // to three bots, one after the other). Only human turns are parsed.
        if directive.is_none()
            && initiator_of(kind) == InitiatorKind::Human
            && let Some((first, rest)) = self.mention_targets(chat_id, user_message)
        {
            self.sh.mention_chain.lock().remove(&chat_id);
            let msg_id = Uuid::now_v7();
            let run = self.start_turn_as_bot(
                chat_id,
                user_message,
                &kind_for_mention(msg_id),
                None,
                handoff,
                Some(first),
                false,
                source,
            )?;
            if !rest.is_empty() {
                self.sh
                    .mention_chain
                    .lock()
                    .insert(chat_id, rest.into_iter().map(|b| (b, msg_id)).collect());
            }
            return Ok(run);
        }
        // A plain turn ends any leftover chain of an earlier message.
        self.sh.mention_chain.lock().remove(&chat_id);
        self.start_turn_as_bot(
            chat_id,
            user_message,
            kind,
            directive,
            handoff,
            None,
            false,
            source,
        )
    }

    /// Bots (existing, with a usable profile) the user's `text` calls with `@`,
    /// first one split from the rest; `None` when it calls nobody else.
    fn mention_targets(&self, chat_id: ChatId, text: &str) -> Option<(BotId, Vec<BotId>)> {
        let chat = self.sh.storage.get_chat(chat_id).ok().flatten()?;
        let bots: Vec<(BotId, String)> = self
            .sh
            .storage
            .list_bots(chat.user_id)
            .ok()?
            .into_iter()
            .filter(|b| self.sh.profile_for(b.id).is_ok())
            .map(|b| (b.id, b.name))
            .collect();
        let mut all = crate::mention::resolve(&bots, chat.bot_id, text).into_iter();
        let first = all.next()?;
        Some((first, all.collect()))
    }

    #[allow(clippy::too_many_arguments)]
    fn start_turn_as_bot(
        &self,
        chat_id: ChatId,
        user_message: &str,
        kind: &str,
        directive: Option<ScheduleWork>,
        handoff: Option<RunId>,
        as_bot: Option<BotId>,
        reuse_user_message: bool,
        source: &'static str,
    ) -> Result<RunId> {
        if self.sh.providers.read().setup_required() {
            return Err(LoopError::SetupRequired);
        }
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Err(LoopError::ShuttingDown);
        }
        let chat = self
            .sh
            .storage
            .get_chat(chat_id)?
            .ok_or_else(|| LoopError::NotFound(format!("chat {chat_id}")))?;
        // A mentioned bot runs in this chat with its own profile; the chat stays
        // the owner's chat with `chat.bot_id`.
        let run_bot = as_bot.unwrap_or(chat.bot_id);
        self.sh.profile_for(run_bot).map_err(LoopError::Profile)?;
        {
            let messages = self.sh.storage.list_messages(chat_id)?;
            if messages.iter().any(|m| m.role == "external_schedule") {
                return Err(LoopError::Profile(
                    "external schedule chat is isolated; start a new chat".into(),
                ));
            }
            if messages.iter().any(|m| m.role == "external_agent") {
                return Err(LoopError::Profile(
                    "external task chats cannot accept follow-up messages; start a new chat".into(),
                ));
            }
        }
        let run_id = {
            let mut active = self.sh.active.lock();
            if active
                .get(&chat_id)
                .is_some_and(|held| Some(*held) != handoff)
            {
                return Err(LoopError::Busy(chat_id));
            }
            // Check under the same lock as turn creation: a fast concurrent
            // human/external turn must not race the history isolation boundary.
            let messages = self.sh.storage.list_messages(chat_id)?;
            if matches!(
                initiator_of(kind),
                InitiatorKind::ExternalAgent | InitiatorKind::ExternalSchedule
            ) && !messages.is_empty()
            {
                return Err(LoopError::Profile(
                    "external tasks require a fresh chat".into(),
                ));
            }
            if messages
                .iter()
                .any(|m| m.role == "external_agent" || m.role == "external_schedule")
            {
                return Err(LoopError::Profile(
                    "external task chats cannot be reused; start a new chat".into(),
                ));
            }
            let run_id = self
                .rt
                .create_run(chat.user_id, run_bot, Some(chat_id), kind)?;
            if !reuse_user_message {
                self.insert_user_message(
                    chat.user_id,
                    chat_id,
                    run_id,
                    user_message,
                    mention_message_id(kind),
                    source,
                )?;
            }
            if let Some(d) = directive {
                if let ScheduleWork::Workflow { params, .. } = &d
                    && let Some(ticket) = params.get("__human_replay")
                    && let Some(replay) = self.sh.router.human_replays.lock().get_mut(ticket)
                {
                    replay.run = Some(run_id);
                }
                self.sh.directives.lock().insert(run_id, d);
            }
            active.insert(chat_id, run_id);
            run_id
        };
        self.spawn(run_id, Start::Fresh);
        Ok(run_id)
    }

    /// Route a user message: starts a turn when the chat is idle; otherwise
    /// `Steer` injects it before the running turn's next model call and
    /// `Queue` runs it as the next turn (07 #1).
    ///
    /// # Errors
    /// As [`Self::run_turn`] when a new turn has to start.
    pub fn send_user_message(
        &self,
        chat_id: ChatId,
        text: &str,
        mode: MessageMode,
    ) -> Result<Delivery> {
        self.send_user_message_from(chat_id, text, mode, "unknown")
    }

    /// Route a message with server-authenticated origin, recorded before execution.
    /// # Errors
    /// As [`Self::send_user_message`].
    pub fn send_user_message_from(
        &self,
        chat_id: ChatId,
        text: &str,
        mode: MessageMode,
        source: &'static str,
    ) -> Result<Delivery> {
        if let Some(run_id) = self.stop_command_target(chat_id, text) {
            self.cancel(run_id)?;
            return Ok(Delivery::Stopped(run_id));
        }
        // Refresh may read the OS secret store. Prepare without holding the
        // active/run-state locks, then validate the current run and `closed`
        // below. Persist and queue exactly the same redacted representation.
        let steering = (mode == MessageMode::Steer).then(|| {
            self.sh.refresh_redactor();
            self.sh.redact(text.to_owned())
        });
        {
            let active = self.sh.active.lock();
            if self
                .sh
                .storage
                .list_messages(chat_id)?
                .iter()
                .any(|m| m.role == "external_agent" || m.role == "external_schedule")
            {
                return Err(LoopError::Profile(
                    "external task chats cannot accept follow-up messages; start a new chat".into(),
                ));
            }
            if let Some(&run_id) = active.get(&chat_id) {
                if let Some(content) = steering {
                    let ctx = self.sh.runs.lock().get(&run_id).cloned();
                    if let Some(ctx) = ctx {
                        let mut st = ctx.state.lock();
                        if !st.closed {
                            let stored = self.insert_redacted_user_message(
                                ctx.user_id,
                                chat_id,
                                run_id,
                                content,
                                None,
                                source,
                            )?;
                            st.steer.push_back(stored);
                            return Ok(Delivery::Steered(run_id));
                        }
                    }
                }
                self.sh
                    .queued
                    .lock()
                    .entry(chat_id)
                    .or_default()
                    .push_back((text.to_owned(), source));
                return Ok(Delivery::Queued);
            }
        }
        Ok(Delivery::NewTurn(self.start_turn_from(
            chat_id,
            text,
            &kind_for(InitiatorKind::Human),
            None,
            None,
            source,
        )?))
    }

    /// Re-run the chat's last user turn as a NEW run (regenerate), or replace
    /// the last user message with `new_text` and run it (edit & resend).
    ///
    /// Only the owner's own desktop message that is still the last user
    /// message, with every run of that turn terminal and no run active in the
    /// chat. The old user message and its answers are marked superseded (kept
    /// in storage, left out of the model context and of the prompt's exposure
    /// provenance); the new user message is stored with source `owner`.
    /// Side effects of the old run are NOT undone. If the new run cannot start,
    /// the old messages are restored.
    ///
    /// # Errors
    /// [`LoopError::NotLastMessage`], [`LoopError::NotResendable`],
    /// [`LoopError::Busy`], or the normal turn-start errors.
    pub fn resend_last_turn(
        &self,
        chat_id: ChatId,
        message_id: Uuid,
        new_text: Option<&str>,
    ) -> Result<RunId> {
        if self.sh.active.lock().contains_key(&chat_id) {
            return Err(LoopError::Busy(chat_id));
        }
        let messages = self.sh.storage.list_messages(chat_id)?;
        let Some(idx) = messages.iter().rposition(|m| m.role == "user") else {
            return Err(LoopError::NotLastMessage);
        };
        let last = &messages[idx];
        if last.id != message_id {
            return Err(LoopError::NotLastMessage);
        }
        if self.sh.storage.message_source(last.id)? != "owner" {
            return Err(LoopError::NotResendable(
                "only your own messages can be edited",
            ));
        }
        let turn = &messages[idx..];
        if turn[1..].iter().any(|m| m.role != "assistant") {
            return Err(LoopError::NotResendable("the turn holds other messages"));
        }
        for run_id in turn.iter().filter_map(|m| m.run_id).collect::<HashSet<_>>() {
            let terminal = self
                .sh
                .storage
                .get_run(run_id)?
                .and_then(|r| RunState::parse(&r.state))
                .is_some_and(RunState::is_terminal);
            if !terminal {
                return Err(LoopError::NotResendable(
                    "the previous run has not finished",
                ));
            }
        }
        let text = match new_text {
            Some(t) => {
                let (_, ids) = crate::attachments::split_marker(&last.content);
                crate::attachments::with_marker(t, &ids)
            }
            None => last.content.clone(),
        };
        let ids: Vec<Uuid> = turn.iter().map(|m| m.id).collect();
        self.sh.storage.set_messages_superseded(&ids, true)?;
        // Keep the replaced text out of memory recall / session search too.
        if let Some(m) = self.sh.cfg.memory.as_ref().filter(|m| m.enabled) {
            let _ = m.store.unindex_messages(&ids);
        }
        match self.start_turn_from(
            chat_id,
            &text,
            &kind_for(InitiatorKind::Human),
            None,
            None,
            "owner",
        ) {
            Ok(run) => Ok(run),
            Err(e) => {
                let _ = self.sh.storage.set_messages_superseded(&ids, false);
                Err(e)
            }
        }
    }

    /// Whether a finished run executed any tool that is not read-only, i.e.
    /// whether redoing its turn could repeat an effect on the outside world.
    /// Conservative: any step not marked replay-safe counts except the user
    /// message and the nudge, so tool batches, CLI-engine model steps (the CLI
    /// runs its own shell/file tools inside them), workflows and commands.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn run_used_non_read_tools(&self, run_id: RunId) -> Result<bool> {
        Ok(self
            .sh
            .storage
            .list_steps(run_id)?
            .iter()
            .any(|s| !s.idempotent && s.name != STEP_USER && s.name != STEP_NUDGE))
    }

    /// The running turn of `chat_id` when `text` is the `/stop` command and
    /// the bot's router is enabled (K0 is deterministic: no ladder needed).
    fn stop_command_target(&self, chat_id: ChatId, text: &str) -> Option<RunId> {
        let run_id = *self.sh.active.lock().get(&chat_id)?;
        let is_stop = matches!(
            agent_router::ladder::rules::slash_command(text),
            Some(agent_router::ladder::Route::Command(c)) if c.name == "stop"
        );
        if !is_stop {
            return None;
        }
        let chat = self.sh.storage.get_chat(chat_id).ok().flatten()?;
        let profile = self.sh.profile_for(chat.bot_id).ok()?;
        profile.router.enabled.then_some(run_id)
    }

    /// Approve a pending tool call (the id from `ApprovalNeeded`).
    ///
    /// # Errors
    /// [`LoopError::NoPendingApproval`] if it is not waiting (already decided
    /// or lost in a restart).
    pub fn approve(&self, call_id: ToolCallId, scope: ApprovalScope, by: UserId) -> Result<()> {
        // Server-side, for every channel: an approval that carries details (a
        // schedule) or whose tool is always-ask is given once, never as a grant.
        // The run-scoped control scope is the one non-Once scope a once-only
        // card may carry, and only when the daemon built it with an eligible
        // control target. It is refused everywhere else, for every client.
        let refused = self.sh.pending.lock().get(&call_id).is_some_and(|p| {
            if scope == ApprovalScope::RunControl {
                !p.grantable
            } else {
                scope != ApprovalScope::Once && p.once_only
            }
        });
        if refused {
            return Err(LoopError::OnceOnly(call_id));
        }
        self.take_pending(call_id)?
            .tx
            .send(Verdict::Approved { scope, by })
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// Whether a card (approval or question) with this call id is waiting for the user right
    /// now. The approval list reads this, so a card whose wait is gone never shows as pending.
    #[must_use]
    pub fn is_pending(&self, call_id: ToolCallId) -> bool {
        self.sh.pending.lock().contains_key(&call_id)
    }

    /// The pending card is a taint-gate card whose "always for this chat" choice is a grant.
    /// Channels that carry owner authority (the desktop app) may give it; a bare API token or a
    /// peer may not (the HTTP route checks this before calling [`Self::approve`]).
    #[must_use]
    pub fn approval_is_gate_grant(&self, call_id: ToolCallId) -> bool {
        self.sh
            .pending
            .lock()
            .get(&call_id)
            .is_some_and(|p| p.gate_grant)
    }

    /// Deny a pending tool call; the model receives a denial message.
    ///
    /// # Errors
    /// As [`Self::approve`].
    pub fn deny(&self, call_id: ToolCallId, by: UserId, reason: Option<String>) -> Result<()> {
        self.take_pending(call_id)?
            .tx
            .send(Verdict::Denied { by, reason })
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// Answer a pending question card (the id from `QuestionAsked`).
    ///
    /// # Errors
    /// [`LoopError::NoPendingApproval`] if no question is waiting under that id.
    pub fn answer_question(&self, call_id: ToolCallId, answer: &str) -> Result<()> {
        self.take_pending(call_id)?
            .tx
            .send(Verdict::Answered {
                answer: answer.to_owned(),
            })
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// The form a parked `secret_request` shows (names and flags, never a value).
    /// `None` when no request waits under `call_id`.
    #[must_use]
    pub fn secret_request(&self, call_id: ToolCallId) -> Option<crate::SecretRequestInfo> {
        self.sh.secret_requests.lock().get(&call_id).cloned()
    }

    /// Claim a waiting `secret_request` for submission: exactly one caller gets
    /// it (a second submit or a cancel then finds nothing). Put it back with
    /// [`Self::restore_secret_request`] if storing failed.
    #[must_use]
    pub fn claim_secret_request(&self, call_id: ToolCallId) -> Option<crate::SecretRequestInfo> {
        self.sh.secret_requests.lock().remove(&call_id)
    }

    /// Undo [`Self::claim_secret_request`] after a failed store, so the owner can
    /// fix the form and submit again.
    pub fn restore_secret_request(&self, call_id: ToolCallId, info: crate::SecretRequestInfo) {
        if self.sh.pending.lock().contains_key(&call_id) {
            self.sh.secret_requests.lock().insert(call_id, info);
        }
    }

    /// End a `secret_request`: `stored` after the submit route wrote the value to
    /// the vault, otherwise cancelled. The model learns only which one.
    ///
    /// # Errors
    /// [`LoopError::NoPendingApproval`] if the request already ended.
    pub fn finish_secret_request(
        &self,
        call_id: ToolCallId,
        stored: bool,
        by: UserId,
    ) -> Result<()> {
        self.sh.secret_requests.lock().remove(&call_id);
        let verdict = if stored {
            Verdict::SecretStored
        } else {
            Verdict::Denied { by, reason: None }
        };
        self.take_pending(call_id)?
            .tx
            .send(verdict)
            .map_err(|_| LoopError::NoPendingApproval(call_id))
    }

    /// Reconcile portable computer state against this daemon and its paired desktops.
    /// Persist the public local identity so a regenerated key produces a one-time audit notice.
    ///
    /// # Errors
    /// Fails on storage errors. Call before starting the scheduler or resuming runs.
    ///
    /// `paired_ids` is `None` when the paired-device list is unknown (the peer
    /// stack is not running): `pc` scopes are then left untouched, because
    /// nothing positively says they are foreign; they simply do not match
    /// while the PC is unreachable.
    pub fn reconcile_computer_identity(
        &self,
        local_id: &str,
        paired_ids: Option<&[String]>,
    ) -> Result<()> {
        let valid_scope = |scope: &str| {
            scope.split_once(':').is_some_and(|(kind, id)| {
                id == local_id
                    || (kind == "pc"
                        && paired_ids.is_none_or(|ids| ids.iter().any(|paired| paired == id)))
            })
        };
        let mut computer_users = HashSet::new();
        let orphaned: Vec<_> = {
            let mut grants = self.sh.grants.lock();
            grants
                .always
                .iter_mut()
                .filter_map(|grant| {
                    if !grant.inert
                        && grant
                            .computer
                            .as_deref()
                            .is_some_and(|scope| !valid_scope(scope))
                    {
                        grant.inert = true;
                        Some(grant.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        for grant in orphaned {
            let Some(bot) = self.sh.storage.get_bot(grant.bot_id)? else {
                continue;
            };
            computer_users.insert(bot.user_id);
            let mut detail = grant_detail(&grant);
            detail["reason"] = serde_json::json!(
                "computer identity no longer belongs to this daemon or a paired desktop; approve again"
            );
            self.sh.storage.append_audit(
                bot.user_id,
                None,
                "system",
                AUDIT_GRANT_INERT,
                &detail,
            )?;
        }
        let paused = self
            .sh
            .storage
            .list_runs_by_state(RunState::Paused.as_str())?;
        for run in &paused {
            if run.computer_target.is_some() {
                computer_users.insert(run.user_id);
            }
        }
        {
            let grants = self.sh.grants.lock();
            for grant in grants.always.iter().filter(|g| g.computer.is_some()) {
                if let Some(bot) = self.sh.storage.get_bot(grant.bot_id)? {
                    computer_users.insert(bot.user_id);
                }
            }
        }
        record_computer_identity(&self.sh.storage, local_id, &computer_users)?;
        for run in paused {
            // A never-bound initial PC wait may still select its first paired PC.
            let invalid = match (run.computer_target.as_deref(), run.computer_node.as_deref()) {
                (None, _) | (_, Some("local")) => true,
                (Some(kind), Some(id)) => !valid_scope(&format!("{kind}:{id}")),
                _ => false,
            };
            if invalid {
                let reason = "Cannot resume: stored computer identity is unknown or no longer belongs to this daemon or a paired desktop (identity changed or state imported); start a new run";
                self.cancel_wait_pc_timer(run.id);
                self.rt.fail_paused(run.id, reason)?;
                crate::usage::finish(&self.sh, run.id);
                self.sh.storage.append_audit(
                    run.user_id,
                    Some(run.id),
                    "system",
                    "run.computer_identity_unavailable",
                    &serde_json::json!({ "reason": reason }),
                )?;
            }
        }
        Ok(())
    }

    /// The persisted "always allow" rules of a bot.
    #[must_use]
    pub fn list_grants(&self, bot_id: BotId) -> Vec<Grant> {
        self.sh
            .grants
            .lock()
            .always
            .iter()
            .filter(|g| g.bot_id == bot_id)
            .cloned()
            .collect()
    }

    /// Remove one "always allow" rule (audited).  Returns whether it existed.
    ///
    /// # Errors
    /// Fails if the bot is unknown or the audit entry cannot be written.
    pub fn revoke_grant(&self, bot_id: BotId, target: &GrantTarget) -> Result<bool> {
        let bot = self
            .sh
            .storage
            .get_bot(bot_id)?
            .ok_or_else(|| LoopError::NotFound(format!("bot {bot_id}")))?;
        let removed = {
            let mut g = self.sh.grants.lock();
            let before = g.always.len();
            g.always
                .retain(|x| !(x.bot_id == bot_id && x.target == *target));
            g.always.len() != before
        };
        if removed {
            let probe = Grant {
                bot_id,
                target: target.clone(),
                granted_at_ms: 0,
                computer: None,
                inert: false,
            };
            self.sh.storage.append_audit(
                bot.user_id,
                None,
                "user",
                AUDIT_GRANT_REVOKED,
                &grant_detail(&probe),
            )?;
        }
        Ok(removed)
    }

    /// Whether a guard (step limit, token budget, repeat detector) ended the
    /// run through the graceful closing call.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn stopped_by_guard(&self, run_id: RunId) -> Result<bool> {
        Ok(self
            .sh
            .storage
            .list_steps(run_id)?
            .iter()
            .filter(|s| s.name == STEP_MODEL)
            .filter_map(step_output::<ModelOut>)
            .any(|m| m.stopped_by_guard))
    }

    /// Wait for background work (memory extraction) started so far.
    pub async fn flush_background(&self) {
        let handles: Vec<_> = self.sh.bg.lock().drain(..).collect();
        for h in handles {
            let _ = h.await;
        }
    }

    fn take_pending(&self, call_id: ToolCallId) -> Result<Pending> {
        self.sh
            .pending
            .lock()
            .remove(&call_id)
            .ok_or(LoopError::NoPendingApproval(call_id))
    }

    /// Cancel a run (in-flight step becomes `Interrupted`); unfinished
    /// subagents of the run are cancelled too.
    ///
    /// # Errors
    /// Fails if the run is terminal or storage fails.
    pub fn cancel(&self, run_id: RunId) -> Result<()> {
        self.cancel_wait_pc_timer(run_id);
        self.sh.run_computers.lock().remove(&run_id);
        self.sh.directives.lock().remove(&run_id);
        let idle_paused = !self.sh.tasks.lock().contains_key(&run_id)
            && self.rt.state(run_id).ok() == Some(RunState::Paused);
        self.rt.cancel(run_id)?;
        // Stop means stop: messages queued behind this turn are dropped, otherwise
        // `conclude` would start the next one the moment this run ends and the
        // user would see the bot carry on as if Stop did nothing. Only messages
        // queued up to now are dropped; one sent after this call is kept.
        if let Ok(Some(run)) = self.sh.storage.get_run(run_id)
            && run.parent_run_id.is_none()
            && let Some(chat_id) = run.chat_id
        {
            self.sh.queued.lock().remove(&chat_id);
        }
        // A cancelled parent takes its subagents with it.
        self.cancel_descendants(run_id);
        crate::usage::finish(&self.sh, run_id);
        if idle_paused {
            if let Ok(Some(run)) = self.sh.storage.get_run(run_id)
                && let Some(chat_id) = run.chat_id
            {
                let mut active = self.sh.active.lock();
                if active.get(&chat_id) == Some(&run_id) {
                    active.remove(&chat_id);
                }
            }
            if let Some(ctx) = self.ctx_of(run_id) {
                self.sh.emit(
                    &ctx,
                    Event::RunFinished(RunFinished {
                        run_id,
                        error: Some("cancelled".into()),
                    }),
                );
                self.sh.runs.lock().remove(&run_id);
                self.sh.run_computers.lock().remove(&run_id);
            }
        }
        Ok(())
    }

    /// Hard stop: the polite [`Self::cancel`] first, then the drive task of the run
    /// and of its subagents is aborted. Dropping the task drops what it was
    /// awaiting (a provider request, a shell or CLI process tree, a browser or
    /// desktop action) even where that work ignores the cancel token. The run is
    /// then finished as `Cancelled` here, because an aborted task never
    /// concludes itself.
    ///
    /// # Errors
    /// Fails if the run is terminal (and was not drivable) or storage fails.
    pub fn force_cancel(&self, run_id: RunId) -> Result<()> {
        let polite = self.cancel(run_id);
        self.force_stop_tree(run_id, None);
        let run = self
            .sh
            .storage
            .get_run(run_id)?
            .ok_or(RuntimeError::NotFound(run_id))?;
        if RunState::parse(&run.state).is_some_and(RunState::is_terminal) {
            // Concluded by the polite path or by the abort just now.
            if self.sh.runs.lock().contains_key(&run_id) {
                self.conclude(run_id, &Ok(RunState::Cancelled));
            }
            return Ok(());
        }
        polite
    }

    /// Abort the drive tasks of `run_id` and its descendants and mark them cancelled.
    fn force_stop_tree(&self, run_id: RunId, parent: Option<RunId>) {
        if let Ok(kids) = self.sh.storage.list_child_runs(run_id) {
            for kid in kids {
                self.force_stop_tree(kid.id, Some(run_id));
            }
        }
        let task = self.sh.tasks.lock().remove(&run_id);
        if let Some(task) = &task {
            task.abort();
        }
        let changed = self.rt.finish_aborted(run_id).unwrap_or(false);
        if changed {
            crate::usage::finish(&self.sh, run_id);
            if let Some(parent) = parent
                && let Some(pctx) = self.ctx_of(parent)
            {
                self.sh.emit(
                    &pctx,
                    crate::subagent::finished_event(run_id, parent, "cancelled", "cancelled", None),
                );
            }
        }
    }

    /// Startup recovery: every run left `Running` **or** `WaitingApproval` gets
    /// its unfinished steps marked `Interrupted` and becomes `Paused`;
    /// dangling approvals expire.  Nothing is replayed or resumed.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn recover(&self) -> Result<Vec<RunId>> {
        let mut ids = self.rt.recover()?;
        let st = &self.sh.storage;
        for run in st.list_runs_by_state(RunState::WaitingApproval.as_str())? {
            st.relabel_steps(
                run.id,
                StepStatus::Intent.as_str(),
                StepStatus::Interrupted.as_str(),
            )?;
            for a in st.list_approvals(run.id)? {
                if a.decision.is_none() {
                    st.decide_approval(a.id, "expired")?;
                }
            }
            st.update_run_state(run.id, RunState::Paused.as_str(), None)?;
            st.append_audit(
                run.user_id,
                Some(run.id),
                "system",
                "run.recovered",
                &serde_json::json!({ "was": "waiting_approval" }),
            )?;
            ids.push(run.id);
        }
        self.reconcile_waiting_for_pc_runs()?;
        Ok(ids)
    }

    /// Re-arm timeout watchers for every persisted `waiting_for_pc` run (and
    /// expire any whose deadline has already elapsed). Safe to call both before
    /// and inside a Tokio runtime.
    ///
    /// # Errors
    /// Fails on storage errors.
    pub fn reconcile_waiting_for_pc_runs(&self) -> Result<()> {
        let limit = self.sh.cfg.wait_for_pc_timeout;
        for run in self
            .sh
            .storage
            .list_paused_runs_with_reason(computer_target::WAIT_FOR_PC_REASON)?
        {
            let elapsed = Duration::from_millis(
                u64::try_from(
                    storage::now_ms().saturating_sub(run.paused_at_ms.unwrap_or(run.updated_at_ms)),
                )
                .unwrap_or(0),
            );
            if elapsed >= limit {
                self.fail_waiting_pc_run(run.id, limit);
            } else {
                self.spawn_wait_pc_timer(run.id, limit.saturating_sub(elapsed));
            }
        }
        Ok(())
    }

    /// Resume any runs paused with `waiting_for_pc` whose target PC desktop is
    /// now online. Returns the run ids that were resumed.
    pub async fn resume_waiting_for_pc(&self) -> Vec<RunId> {
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let Ok(paused) = self
            .sh
            .storage
            .list_paused_runs_with_reason(computer_target::WAIT_FOR_PC_REASON)
        else {
            return Vec::new();
        };
        let mut resumed = Vec::new();
        for run in paused {
            let Ok(profile) = self.sh.profile_for(run.bot_id) else {
                continue;
            };
            let rc = self.sh.resolve_computer_for_run(&profile, Some(&run));
            if rc.wait_for_pc || rc.unavailable_reason.is_some() {
                continue;
            }
            for attempt in 0..5 {
                match self.resume(run.id) {
                    Ok(()) => {
                        resumed.push(run.id);
                        break;
                    }
                    Err(LoopError::StillFinishing(_)) if attempt < 4 => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(_) => break,
                }
            }
        }
        resumed
    }

    fn cancel_wait_pc_timer(&self, run_id: RunId) {
        if let Some(tok) = self.sh.wait_pc_timers.lock().remove(&run_id) {
            tok.cancel();
        }
    }

    fn spawn_wait_pc_timer(&self, run_id: RunId, delay: Duration) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        self.cancel_wait_pc_timer(run_id);
        let tok = tokio_util::sync::CancellationToken::new();
        self.sh.wait_pc_timers.lock().insert(run_id, tok.clone());
        let this = self.clone();
        let limit = self.sh.cfg.wait_for_pc_timeout;
        handle.spawn(async move {
            tokio::select! {
                () = tok.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
            this.sh.wait_pc_timers.lock().remove(&run_id);
            if !tok.is_cancelled() {
                this.fail_waiting_pc_run(run_id, limit);
            }
        });
    }

    pub(crate) fn fail_waiting_pc_run(&self, run_id: RunId, timeout: Duration) {
        let Ok(Some(run)) = self.sh.storage.get_run(run_id) else {
            return;
        };
        if run.state != RunState::Paused.as_str()
            || run.pause_reason.as_deref() != Some(computer_target::WAIT_FOR_PC_REASON)
        {
            return;
        }
        let message = format!(
            "timed out waiting for PC desktop to come online after {}",
            format_wait_duration(timeout)
        );
        let ctx = self.ctx_of(run_id);
        if self.rt.fail_paused(run_id, &message).is_ok() {
            crate::usage::finish(&self.sh, run_id);
            self.sh.directives.lock().remove(&run_id);
            self.sh.run_computers.lock().remove(&run_id);
            if let Some(chat_id) = run.chat_id {
                let mut active = self.sh.active.lock();
                if active.get(&chat_id) == Some(&run_id) {
                    active.remove(&chat_id);
                }
            }
            let _ = self.sh.storage.append_audit(
                run.user_id,
                Some(run_id),
                "system",
                "run.wait_pc_timed_out",
                &serde_json::json!({
                    "timeout_ms": u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                }),
            );
            if let Some(ctx) = &ctx {
                self.sh.emit(
                    ctx,
                    Event::Error(RunError {
                        run_id,
                        message: message.clone(),
                        recoverable: false,
                    }),
                );
                self.sh.emit(
                    ctx,
                    Event::RunFinished(RunFinished {
                        run_id,
                        error: Some(message),
                    }),
                );
            }
            self.sh.runs.lock().remove(&run_id);
            self.sh.run_computers.lock().remove(&run_id);
        }
    }

    /// Resume a `Paused` run in the background.  An interrupted model step is
    /// replayed; an interrupted tool call is **never** re-run (it may have had
    /// side effects): the model is told its outcome is unknown.
    ///
    /// # Errors
    /// Fails if the run does not exist, is not `Paused`, or its chat is busy.
    /// [`LoopError::StillFinishing`] if the previous drive task has not yet
    /// released its handle (retry; a second driver is not started).
    pub fn resume(&self, run_id: RunId) -> Result<()> {
        if self.sh.providers.read().setup_required() {
            return Err(LoopError::SetupRequired);
        }
        if self.sh.shutting_down.load(Ordering::SeqCst) {
            return Err(LoopError::ShuttingDown);
        }
        let run = self
            .sh
            .storage
            .get_run(run_id)?
            .ok_or_else(|| LoopError::NotFound(format!("run {run_id}")))?;
        let state = self.rt.state(run_id)?;
        if state != RunState::Paused {
            return Err(RuntimeError::IllegalTransition {
                from: state,
                to: RunState::Running,
            }
            .into());
        }
        // A subagent is not the chat's turn; its parent is.
        let mut reserved_chat = None;
        if run.parent_run_id.is_none()
            && let Some(chat) = run.chat_id
        {
            let mut active = self.sh.active.lock();
            if active.contains_key(&chat) {
                return Err(LoopError::Busy(chat));
            }
            active.insert(chat, run_id);
            reserved_chat = Some(chat);
        }
        // The finishing drive task still owns the map entry. Inserting here
        // would drop its `JoinHandle` (tokio detaches on drop) and that task's
        // later `remove` would then drop the resumed task.
        if !self.spawn(run_id, Start::Resume) {
            if let Some(chat) = reserved_chat {
                let mut active = self.sh.active.lock();
                if active.get(&chat) == Some(&run_id) {
                    active.remove(&chat);
                }
            }
            return Err(LoopError::StillFinishing(run_id));
        }
        Ok(())
    }

    /// Graceful shutdown (a restart must lose nothing).
    ///
    /// 1. Stops accepting new turns / resumes ([`LoopError::ShuttingDown`]).
    /// 2. Cancels in-flight model calls and runs parked on an approval or
    ///    question; their step is marked `Interrupted`.
    /// 3. Lets running *tool* steps finish for up to `grace`, then interrupts
    ///    what is left (an interrupted tool is never replayed on resume).
    /// 4. Every run that was live ends `Paused` (resumable after the next
    ///    start; dangling approvals expire), not `Cancelled`.
    /// 5. Flushes background memory tasks (bounded by `grace`, at least 1 s).
    ///
    /// Queued (not yet started) follow-up messages are in-memory only and are
    /// dropped.  Returns the runs that were paused.  Call
    /// [`Self::recover`] afterwards to checkpoint anything a crash left behind.
    pub async fn shutdown(&self, grace: Duration) -> Vec<RunId> {
        self.sh.shutting_down.store(true, Ordering::SeqCst);
        for (_, tok) in self.sh.wait_pc_timers.lock().drain() {
            tok.cancel();
        }
        let deadline = tokio::time::Instant::now() + grace;
        // Hard stop for the unwinding of interrupted runs.
        let hard = deadline + Duration::from_secs(5);
        let mut seen: HashSet<RunId> = HashSet::new();
        loop {
            let active = self.rt.active_runs();
            if active.is_empty() {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= hard {
                break;
            }
            for run in active {
                seen.insert(run);
                // A finishing tool step is the only thing worth waiting for.
                let interrupt = now >= deadline || !self.tool_step_in_flight(run);
                self.rt.request_pause(run, interrupt);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Let the drive tasks finish their bookkeeping (`conclude`).
        let handles: Vec<_> = self.sh.tasks.lock().drain().map(|(_, h)| h).collect();
        for h in handles {
            let left = hard.saturating_duration_since(tokio::time::Instant::now());
            let _ = tokio::time::timeout(left.max(Duration::from_millis(200)), h).await;
        }
        let mut paused = Vec::new();
        for run in seen {
            if self.rt.state(run).ok() != Some(RunState::Paused) {
                continue;
            }
            self.expire_approvals(run);
            if let Ok(Some(r)) = self.sh.storage.get_run(run) {
                let _ = self.sh.storage.append_audit(
                    r.user_id,
                    Some(run),
                    "system",
                    "run.paused_for_shutdown",
                    &serde_json::json!({}),
                );
            }
            paused.push(run);
        }
        let _ =
            tokio::time::timeout(grace.max(Duration::from_secs(1)), self.flush_background()).await;
        // Everything emitted so far (including the tail events of the runs just
        // paused) must be on disk before the process exits.
        self.flush_journal();
        paused
    }

    /// Make every event emitted so far durable (blocks until the journal
    /// writer has synced them).
    pub fn flush_journal(&self) {
        self.sh.flush_journal();
    }

    /// Journal write/sync failures since start (0 = healthy).
    #[must_use]
    pub fn journal_error_count(&self) -> u64 {
        self.sh.journal_error_count()
    }

    /// Whether the run's newest step is a tool step that is still executing
    /// (and the run is not parked waiting for the user).
    fn tool_step_in_flight(&self, run: RunId) -> bool {
        let st = &self.sh.storage;
        if st
            .get_run(run)
            .ok()
            .flatten()
            .is_some_and(|r| r.state == RunState::WaitingApproval.as_str())
        {
            return false;
        }
        st.list_steps(run)
            .ok()
            .and_then(|s| s.last().cloned())
            .is_some_and(|s| s.name == STEP_TOOLS && s.status == StepStatus::Intent.as_str())
    }

    fn expire_approvals(&self, run: RunId) {
        let st = &self.sh.storage;
        for a in st.list_approvals(run).unwrap_or_default() {
            if a.decision.is_none() {
                let _ = st.decide_approval(a.id, "expired");
            }
        }
    }

    /// The bot's permission picture: its stored policy merged with its
    /// persisted grants, per category (what a human-initiated call resolves to).
    ///
    /// # Errors
    /// [`LoopError::NotFound`] / [`LoopError::Profile`] when the bot or its
    /// profile is unusable.
    pub fn effective_policy(&self, bot_id: BotId) -> Result<EffectivePolicy> {
        let profile = self.sh.profile_for(bot_id).map_err(|e| {
            if e.contains("not found") {
                LoopError::NotFound(format!("bot {bot_id}"))
            } else {
                LoopError::Profile(e)
            }
        })?;
        let policy = profile.policy();
        let categories: BTreeMap<PermissionCategory, CategoryDecision> = [
            PermissionCategory::Fs,
            PermissionCategory::Shell,
            PermissionCategory::Desktop,
            PermissionCategory::Browser,
            PermissionCategory::Accounts,
            PermissionCategory::Messaging,
            PermissionCategory::Money,
            PermissionCategory::System,
            PermissionCategory::Remote,
            PermissionCategory::Script,
        ]
        .into_iter()
        .map(|c| {
            let (decision, via_grant, scopes) =
                self.sh.effective_category(bot_id, policy.as_ref(), c);
            (
                c,
                CategoryDecision {
                    decision,
                    via_grant,
                    scopes,
                },
            )
        })
        .collect();
        Ok(EffectivePolicy {
            mode: profile.permissions.mode(),
            policy,
            categories,
            grants: self.list_grants(bot_id),
        })
    }

    /// Wait until the background task of `run_id` (started by this handle) has
    /// finished and return the run state.
    ///
    /// A finished task drops its own map entry, so a missing handle still
    /// returns the persisted state.
    ///
    /// # Errors
    /// Fails if the run is unknown or storage fails.
    pub async fn wait(&self, run_id: RunId) -> Result<RunState> {
        let handle = self.sh.tasks.lock().remove(&run_id);
        if let Some(h) = handle {
            let _ = h.await;
        }
        Ok(self.rt.state(run_id)?)
    }

    // ---- internals ----

    fn insert_user_message(
        &self,
        user_id: UserId,
        chat_id: ChatId,
        run_id: RunId,
        text: &str,
        id: Option<Uuid>,
        source: &str,
    ) -> Result<String> {
        // A key pasted into the chat is stored (and replayed into every later
        // provider request) masked.
        self.sh.refresh_redactor();
        let content = self.sh.redact(text.to_owned());
        self.insert_redacted_user_message(user_id, chat_id, run_id, content, id, source)
    }

    fn insert_redacted_user_message(
        &self,
        user_id: UserId,
        chat_id: ChatId,
        run_id: RunId,
        content: String,
        id: Option<Uuid>,
        source: &str,
    ) -> Result<String> {
        let id = id.unwrap_or_else(Uuid::now_v7);
        let msg = DbMessage {
            id,
            user_id,
            chat_id,
            run_id: Some(run_id),
            role: match self
                .sh
                .storage
                .get_run(run_id)?
                .map(|run| initiator_of(&run.kind))
            {
                Some(InitiatorKind::ExternalAgent) => "external_agent",
                Some(InitiatorKind::ExternalSchedule) => "external_schedule",
                _ => "user",
            }
            .to_owned(),
            content: content.clone(),
            created_at_ms: now_ms(),
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        };
        self.sh.storage.insert_message(&msg)?;
        self.sh.storage.set_message_source(id, source)?;
        if let Some(m) = self.sh.cfg.memory.as_ref().filter(|m| m.enabled) {
            let _ = m.store.index_message(&msg);
        }
        Ok(content)
    }

    /// Start the drive task. Returns `false` when a handle for `run_id` is
    /// already in the map: the caller must not treat `insert` as taking
    /// ownership of that older live task.
    pub(crate) fn spawn(&self, run_id: RunId, start: Start) -> bool {
        // The task waits until its handle is in the map, then removes that
        // entry only when it is still its own handle. `wait` tolerates the
        // entry already being gone. The removed handle is dropped after the
        // map lock is released (dropping a `JoinHandle` detaches).
        let (tx, rx) = tokio::sync::oneshot::channel();
        let this = self.clone();
        let sh = Arc::clone(&this.sh);
        let mut tasks = self.sh.tasks.lock();
        if tasks.contains_key(&run_id) {
            return false;
        }
        let handle = tokio::spawn(async move {
            let _ = rx.await;
            let my_id = tokio::task::id();
            let after = this.clone();
            this.drive(run_id, start).await;
            #[cfg(test)]
            {
                let gate = sh.task_reap_gate.lock().clone();
                if let Some(gate) = gate {
                    sh.task_reap_entered.store(true, Ordering::SeqCst);
                    gate.wait().await;
                }
            }
            let finished = {
                let mut tasks = sh.tasks.lock();
                let mine = tasks.get(&run_id).is_some_and(|h| h.id() == my_id);
                if mine { tasks.remove(&run_id) } else { None }
            };
            drop(finished);
            if matches!(start, Start::Fresh)
                && !sh.shutting_down.load(Ordering::SeqCst)
                && sh.storage.get_run(run_id).ok().flatten().is_some_and(|r| {
                    r.state == RunState::Paused.as_str()
                        && r.pause_reason.as_deref() == Some(computer_target::WAIT_FOR_PC_REASON)
                })
            {
                tokio::spawn(async move {
                    let _ = after.resume_waiting_for_pc().await;
                });
            }
            #[cfg(test)]
            sh.task_reap_done.store(true, Ordering::SeqCst);
        });
        tasks.insert(run_id, handle);
        drop(tasks);
        let _ = tx.send(());
        true
    }

    // Boxed so the spawn / `conclude` / next-turn recursion has a nameable type.
    #[allow(clippy::too_many_lines)] // one ordered start-up pass; the order is the contract
    fn drive(self, run_id: RunId, start: Start) -> BoxFuture<'static, ()> {
        let activity = crate::usage::Activity::new(&self.sh, run_id);
        Box::pin(async move {
            let _activity = activity;
            let res = match start {
                Start::Fresh => {
                    let run = self.sh.storage.get_run(run_id).ok().flatten();
                    let is_child = run.as_ref().is_some_and(|r| r.parent_run_id.is_some());
                    let mut wait_for_pc = false;
                    let mut unavailable = None;
                    let mut prepare_timed_out = false;
                    let mut computer_notice = None;
                    if !is_child
                        && let Some(run) = &run
                        && let Ok(profile) = self.sh.profile_for(run.bot_id)
                    {
                        // Preparing the computer can take a while and the run has no
                        // cancel token yet: Stop in that window cancels the queued run in
                        // storage, so look for it here instead of finishing the wait.
                        let prepare = self.sh.prepare_run_computer(&profile, run);
                        tokio::pin!(prepare);
                        prepare_timed_out = loop {
                            tokio::select! {
                                done = &mut prepare => break done,
                                () = tokio::time::sleep(Duration::from_millis(200)) => {
                                    if self.rt.state(run_id).ok() == Some(RunState::Cancelled) {
                                        self.conclude(run_id, &Ok(RunState::Cancelled));
                                        return;
                                    }
                                }
                            }
                        };
                        let rc = self.sh.resolve_computer_for_run(&profile, None);
                        computer_notice.clone_from(&rc.notice);
                        wait_for_pc = rc.wait_for_pc;
                        unavailable.clone_from(&rc.unavailable_reason);
                        if let Err(e) = self.sh.bind_computer(run_id, &rc) {
                            let _ = self.rt.pause_queued(run_id, None);
                            self.conclude(run_id, &self.rt.fail_paused(run_id, &e.to_string()));
                            return;
                        }
                        self.sh.run_computers.lock().insert(run_id, rc);
                    }
                    // A child announces itself with `subagent_started` instead.
                    if !is_child && let Some(ctx) = self.ctx_of(run_id) {
                        self.sh.emit(
                            &ctx,
                            Event::RunStarted(RunStarted {
                                run_id,
                                chat_id: ctx.chat_id,
                                bot_id: ctx.bot_id,
                                schedule_id: ctx.schedule_id,
                            }),
                        );
                        if prepare_timed_out {
                            self.sh.emit(
                                &ctx,
                                Event::Error(RunError {
                                    run_id,
                                    message: COMPUTER_PREPARE_TIMEOUT_NOTICE.into(),
                                    recoverable: true,
                                }),
                            );
                        }
                        if let Some(message) = computer_notice {
                            self.sh.emit(
                                &ctx,
                                Event::Error(RunError {
                                    run_id,
                                    message,
                                    recoverable: true,
                                }),
                            );
                        }
                    }
                    if let Some(reason) = unavailable {
                        let result = self.rt.pause_queued(run_id, None);
                        if result.is_ok() {
                            self.conclude(run_id, &self.rt.fail_paused(run_id, &reason));
                        }
                        return;
                    }
                    if wait_for_pc {
                        let pause_res = self
                            .rt
                            .pause_queued(run_id, Some(computer_target::WAIT_FOR_PC_REASON));
                        if matches!(pause_res, Ok(RunState::Paused)) {
                            self.spawn_wait_pc_timer(run_id, self.sh.cfg.wait_for_pc_timeout);
                        }
                        pause_res
                    } else {
                        self.rt.start(run_id).await
                    }
                }
                Start::Resume => self.drive_resume(run_id).await,
            };
            // Stopped before the run could start: that is a stop, not an error.
            let res = match res {
                Err(RuntimeError::IllegalTransition { .. })
                    if self.rt.state(run_id).ok() == Some(RunState::Cancelled) =>
                {
                    Ok(RunState::Cancelled)
                }
                other => other,
            };
            self.conclude(run_id, &res);
        })
    }

    async fn drive_resume(&self, run_id: RunId) -> std::result::Result<RunState, RuntimeError> {
        self.cancel_wait_pc_timer(run_id);
        let run = self
            .sh
            .storage
            .get_run(run_id)?
            .ok_or(RuntimeError::NotFound(run_id))?;
        if run.parent_run_id.is_none()
            && let Ok(profile) = self.sh.profile_for(run.bot_id)
        {
            let provider = self.sh.computer_provider.read().clone();
            if crate::shared::Shared::computer_identity_unbound(&run, provider.as_deref()) {
                self.sh.identity_paused.lock().insert(run_id);
                self.emit_computer_notice(run_id, COMPUTER_IDENTITY_UNBOUND_NOTICE);
                return Ok(RunState::Paused);
            }
            let prepare_timed_out = self.sh.prepare_run_computer(&profile, &run).await;
            let rc = self.sh.resolve_computer_for_run(&profile, Some(&run));
            if let Some(reason) = rc.unavailable_reason.clone() {
                if prepare_timed_out {
                    self.sh.run_computers.lock().insert(run_id, rc);
                    self.emit_computer_notice(run_id, COMPUTER_PREPARE_TIMEOUT_NOTICE);
                }
                return self.rt.fail_paused(run_id, &reason);
            }
            self.sh.bind_computer(run_id, &rc)?;
            let computer_notice = rc.notice.clone();
            self.sh.run_computers.lock().insert(run_id, rc.clone());
            if prepare_timed_out {
                self.emit_computer_notice(run_id, COMPUTER_PREPARE_TIMEOUT_NOTICE);
            }
            if let Some(message) = computer_notice {
                self.emit_computer_notice(run_id, &message);
            }
            if rc.wait_for_pc {
                self.sh
                    .storage
                    .set_run_pause_reason(run_id, Some(computer_target::WAIT_FOR_PC_REASON))?;
                let elapsed = Duration::from_millis(
                    u64::try_from(
                        storage::now_ms()
                            .saturating_sub(run.paused_at_ms.unwrap_or(run.updated_at_ms)),
                    )
                    .unwrap_or(0),
                );
                self.spawn_wait_pc_timer(
                    run_id,
                    self.sh.cfg.wait_for_pc_timeout.saturating_sub(elapsed),
                );
                return Ok(RunState::Paused);
            }
        }
        // Re-plan before replaying an interrupted model request so accepted
        // corrections enter its durable history first.
        if self
            .ctx_of(run_id)
            .is_some_and(|ctx| !ctx.state.lock().steer.is_empty())
        {
            return self.rt.resume_skipping_interrupted(run_id).await;
        }
        match self.rt.resume(run_id).await {
            Err(RuntimeError::NeedsDecision(_)) => {
                self.rt.resume_skipping_interrupted(run_id).await
            }
            other => other,
        }
    }

    fn emit_computer_notice(&self, run_id: RunId, message: &str) {
        let Some(ctx) = self.ctx_of(run_id) else {
            return;
        };
        self.sh.emit(
            &ctx,
            Event::Error(RunError {
                run_id,
                message: message.to_owned(),
                recoverable: true,
            }),
        );
    }

    pub(crate) fn ctx_of(&self, run_id: RunId) -> Option<Arc<crate::shared::RunCtx>> {
        let run = self.sh.storage.get_run(run_id).ok().flatten()?;
        self.sh.ctx(&run).ok()
    }

    /// Persist the final answer, emit the closing events and start any queued turn.
    #[allow(clippy::too_many_lines)] // one terminal bookkeeping pass; the order is the contract
    fn conclude(&self, run_id: RunId, res: &std::result::Result<RunState, RuntimeError>) {
        let run = self.sh.storage.get_run(run_id).ok().flatten();
        if !matches!(res, Ok(RunState::Paused)) {
            crate::usage::finish(&self.sh, run_id);
        }
        crate::compaction::forget_turn(run_id);
        if !matches!(res, Ok(RunState::Paused)) {
            // A finished run's spill files are no longer reachable by the
            // model; crashed runs are swept by age.
            let data_dir = self.sh.storage.data_dir();
            crate::compaction::remove_run_spill(data_dir, run_id);
            crate::compaction::sweep_spill(data_dir, crate::compaction::SPILL_RETENTION);
        }
        if let Some(parent) = run.as_ref().and_then(|r| r.parent_run_id) {
            self.conclude_child(run_id, parent, res);
            return;
        }
        self.sh
            .router
            .human_replays
            .lock()
            .retain(|_, replay| replay.run != Some(run_id));
        let ctx = self.ctx_of(run_id);
        if matches!(res, Ok(RunState::Paused)) {
            // Paused by `shutdown`: no `run_finished` (the run is resumable and
            // a finished marker in its log would end later followers), no
            // queued follow-up turn.
            let approval_timeout = self.sh.approval_paused.lock().remove(&run_id);
            let identity_unbound = self.sh.identity_paused.lock().remove(&run_id);
            let wait_pc = run.as_ref().and_then(|r| r.pause_reason.as_deref())
                == Some(computer_target::WAIT_FOR_PC_REASON);
            if let Some(ctx) = &ctx {
                let message = if approval_timeout {
                    "run paused: nobody answered the approval request in time; resume it with \
                     POST /v1/runs/{id}/resume"
                } else if identity_unbound {
                    "run paused: this computer's identity could not be read; it stays paused                      and can be resumed after the next restart"
                } else if wait_pc {
                    "run paused: waiting for PC desktop to come online"
                } else {
                    "run paused: the daemon is shutting down; resume it after the next start"
                };
                self.sh.emit(
                    ctx,
                    Event::Error(RunError {
                        run_id,
                        message: message.into(),
                        recoverable: true,
                    }),
                );
            }
            self.sh.runs.lock().remove(&run_id);
            self.sh.run_computers.lock().remove(&run_id);
            if let Some(chat_id) = run.as_ref().and_then(|r| r.chat_id) {
                let mut active = self.sh.active.lock();
                if active.get(&chat_id) == Some(&run_id) {
                    active.remove(&chat_id);
                }
            }
            return;
        }
        self.sh.router.tainted_runs.lock().remove(&run_id);
        let error: Option<String> = match res {
            Ok(RunState::Succeeded) => None,
            Ok(RunState::Failed) => Some(
                run.as_ref()
                    .and_then(|r| r.error.clone())
                    .unwrap_or_else(|| "failed".into()),
            ),
            Ok(RunState::Cancelled) => Some("cancelled".into()),
            Ok(other) => Some(format!("stopped in state {other}")),
            Err(e) => Some(e.to_string()),
        };
        // Provider errors can carry URLs with `?key=...`.
        let error = error.map(|e| self.sh.redact(e));
        if error.is_some()
            && let Some(chat_id) = run.as_ref().and_then(|r| r.chat_id)
        {
            // Stopped or failed: the remaining mentioned bots do not answer.
            self.sh.mention_chain.lock().remove(&chat_id);
        }
        if error.is_none()
            && let Some(run) = &run
            && let Some(chat_id) = run.chat_id
            && let Some(text) = self.final_text(run_id)
        {
            // The final answer is persisted, replayed into later requests and fed
            // to memory extraction: mask it as a whole (a secret split across
            // streamed deltas is only whole here).
            let text = self.sh.redact(text);
            let exposure = ctx.as_ref().map(|c| c.taint.lock().clone());
            let msg = DbMessage {
                id: Uuid::now_v7(),
                user_id: run.user_id,
                chat_id,
                run_id: Some(run_id),
                role: "assistant".to_owned(),
                content: text.clone(),
                created_at_ms: now_ms(),
                exposure_untrusted: exposure.as_ref().is_some_and(|t| t.exposure.untrusted),
                exposure_private: exposure.as_ref().is_some_and(|t| t.exposure.private),
                exposure_sources: exposure.map(|t| t.sources().to_vec()).unwrap_or_default(),
            };
            let _ = self.sh.storage.insert_message(&msg);
            if let Some(m) = self.sh.cfg.memory.as_ref().filter(|m| m.enabled) {
                let _ = m.store.index_message(&msg);
            }
            self.enqueue_memory(run, &text);
        }
        if let Some(ctx) = &ctx {
            if let Some(message) = &error {
                self.sh.emit(
                    ctx,
                    Event::Error(RunError {
                        run_id,
                        message: message.clone(),
                        recoverable: false,
                    }),
                );
            }
            self.sh
                .emit(ctx, Event::RunFinished(RunFinished { run_id, error }));
        }
        self.sh.runs.lock().remove(&run_id);
        self.sh.run_computers.lock().remove(&run_id);

        let Some(run) = run else { return };
        self.start_next_queued(&run, run_id);
    }

    /// Release the chat after `run_id`, or hand it straight to the next queued
    /// message (see [`Self::start_turn`]'s `handoff`).
    fn start_next_queued(&self, run: &storage::Run, run_id: RunId) {
        let Some(chat_id) = run.chat_id else {
            return;
        };
        // Take the next queued message and keep the chat reserved (the slot
        // stays with this run) until its turn has started, so a concurrent
        // start cannot make the follow-up bounce off `Busy`.
        let (chained, next) = {
            let mut active = self.sh.active.lock();
            let (chained, next) = if active.get(&chat_id) == Some(&run_id) {
                // The rest of a multi-mention message goes before later messages.
                let chained = self
                    .sh
                    .mention_chain
                    .lock()
                    .get_mut(&chat_id)
                    .and_then(std::collections::VecDeque::pop_front);
                let next = if chained.is_some() {
                    None
                } else {
                    self.sh
                        .queued
                        .lock()
                        .get_mut(&chat_id)
                        .and_then(std::collections::VecDeque::pop_front)
                };
                if chained.is_none() && next.is_none() {
                    active.remove(&chat_id);
                }
                (chained, next)
            } else {
                (None, None)
            };
            drop(active);
            (chained, next)
        };
        if let Some((bot, msg_id)) = chained {
            let started = self.start_turn_as_bot(
                chat_id,
                "",
                &kind_for_mention(msg_id),
                None,
                Some(run_id),
                Some(bot),
                true,
                "unknown",
            );
            if started.is_err() {
                self.sh.mention_chain.lock().remove(&chat_id);
                let mut active = self.sh.active.lock();
                if active.get(&chat_id) == Some(&run_id) {
                    active.remove(&chat_id);
                }
            }
        } else if let Some((text, source)) = next {
            let started = self.start_turn_from(
                chat_id,
                &text,
                &kind_for(initiator_of(&run.kind)),
                None,
                Some(run_id),
                source,
            );
            if started.is_err() {
                // Never lose the follow-up: release the slot and put it back.
                {
                    let mut active = self.sh.active.lock();
                    if active.get(&chat_id) == Some(&run_id) {
                        active.remove(&chat_id);
                    }
                }
                self.sh
                    .queued
                    .lock()
                    .entry(chat_id)
                    .or_default()
                    .push_front((text, source));
            }
        }
    }

    /// Hand the finished exchange to the memory pipeline (background task).
    fn enqueue_memory(&self, run: &storage::Run, final_text: &str) {
        if !self.sh.cfg.memory.as_ref().is_some_and(|m| m.enabled) {
            return;
        }
        if let Some(ex) = crate::memory::Exchange::of(&self.sh, run, final_text) {
            let sh = Arc::clone(&self.sh);
            let activity = crate::usage::Activity::new(&sh, run.id);
            let handle = tokio::spawn(async move {
                let _activity = activity;
                crate::memory::extract(sh, ex).await;
            });
            let mut bg = self.sh.bg.lock();
            bg.retain(|h| !h.is_finished());
            bg.push(handle);
        }
    }

    fn final_text(&self, run_id: RunId) -> Option<String> {
        let steps = self.sh.storage.list_steps(run_id).ok()?;
        let last = steps.last()?;
        if let Some(text) = crate::router::routed_final_text(last) {
            return Some(text);
        }
        if last.name != STEP_MODEL || status_of(last) != Some(StepStatus::Succeeded) {
            return None;
        }
        step_output::<ModelOut>(last)
            .map(|m| m.text)
            .filter(|t| !t.is_empty())
    }
}

fn record_computer_identity(
    storage: &Storage,
    local_id: &str,
    computer_users: &HashSet<UserId>,
) -> Result<()> {
    for user in storage.list_users()? {
        let previous = storage
            .last_audit_by_action(user.id, "computer.identity")?
            .and_then(|e| e.detail["computer"].as_str().map(str::to_owned));
        // Users who never used computer tools get no identity row.
        if previous.is_none() && !computer_users.contains(&user.id) {
            continue;
        }
        if previous.as_deref() != Some(local_id) {
            storage.append_audit(user.id, None, "system", "computer.identity",
                    &serde_json::json!({
                        "computer": local_id, "previous": previous,
                        "reason": if previous.is_some() {
                            "Local computer identity changed; orphaned grants are inert and paused runs cannot resume"
                        } else {
                            "Local computer identity recorded"
                        },
                    }))?;
        }
    }
    Ok(())
}

fn load_grants(storage: &Storage) -> Grants {
    let mut grants = Grants::default();
    let mut inert_audited = HashSet::new();
    for user in storage.list_users().unwrap_or_default() {
        for e in storage.list_audit(user.id).unwrap_or_default() {
            if !matches!(
                e.action.as_str(),
                AUDIT_GRANT_ALWAYS | AUDIT_GRANT_REVOKED | AUDIT_GRANT_INERT
            ) {
                continue;
            }
            let bot = e.detail["bot_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(BotId);
            let (Some(bot_id), Some(target)) = (bot, grant_target(&e.detail)) else {
                continue;
            };
            if e.action == AUDIT_GRANT_INERT {
                inert_audited.insert((
                    bot_id,
                    target.clone(),
                    e.detail["computer"].as_str().map(str::to_owned),
                ));
                for g in &mut grants.always {
                    if g.bot_id == bot_id
                        && g.target == target
                        && g.computer.as_deref() == e.detail["computer"].as_str()
                    {
                        g.inert = true;
                    }
                }
                continue;
            }
            let computer = e.detail["computer"].as_str().map(str::to_owned);
            grants.always.retain(|g| {
                !(g.bot_id == bot_id
                    && g.target == target
                    && (e.action == AUDIT_GRANT_REVOKED || g.computer == computer))
            });
            if e.action == AUDIT_GRANT_ALWAYS {
                grants.always.push(Grant {
                    bot_id,
                    target,
                    granted_at_ms: e.ts_ms,
                    computer,
                    inert: false,
                });
            }
        }
    }
    let mut pending_inert = Vec::new();
    for g in &mut grants.always {
        if legacy_unscoped_computer(g)
            || g.computer
                .as_deref()
                .is_some_and(|scope| scope.ends_with(":local"))
        {
            g.inert = true;
            let key = (g.bot_id, g.target.clone(), g.computer.clone());
            if inert_audited.insert(key) {
                pending_inert.push(g.clone());
            }
        }
    }
    for grant in pending_inert {
        let Ok(Some(bot)) = storage.get_bot(grant.bot_id) else {
            continue;
        };
        let mut detail = grant_detail(&grant);
        detail["reason"] =
            serde_json::json!("legacy desktop/browser grant has no verifiable computer identity");
        let _ = storage.append_audit(bot.user_id, None, "system", AUDIT_GRANT_INERT, &detail);
    }
    grants
}

fn legacy_unscoped_computer(g: &Grant) -> bool {
    if g.computer.is_some() {
        return false;
    }
    match &g.target {
        GrantTarget::Category(category) => matches!(
            category,
            PermissionCategory::Desktop | PermissionCategory::Browser
        ),
        GrantTarget::Tool(name) | GrantTarget::Scoped { tool: name, .. } => {
            name.starts_with("desktop_") || name.starts_with("browser_")
        }
    }
}

fn format_wait_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else if secs > 0 {
        format!("{secs}s")
    } else {
        format!("{}ms", d.as_millis())
    }
}

#[cfg(test)]
mod emit_tests {
    use super::*;
    use protocol::event::CheckpointRestored;

    #[test]
    fn emit_run_event_appends_durable_seq_and_broadcasts() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let run_id = RunId::new();
        let agent = AgentLoop::new(
            Arc::clone(&storage),
            ToolRegistry::new(),
            Providers::unconfigured(),
            LoopConfig::default(),
        );
        let mut rx = agent.subscribe();

        agent.emit_run_event(
            run_id,
            Event::CheckpointRestored(CheckpointRestored {
                partial: false,
                run_id,
                checkpoint_id: "cp-1".into(),
                safety_checkpoint_id: "cp-safe".into(),
                restored: vec!["/tmp/a.txt".into()],
            }),
        );
        agent.emit_run_event(
            run_id,
            Event::CheckpointRestored(CheckpointRestored {
                partial: false,
                run_id,
                checkpoint_id: "cp-2".into(),
                safety_checkpoint_id: "cp-safe-2".into(),
                restored: vec!["/tmp/b.txt".into()],
            }),
        );

        let first = rx.try_recv().unwrap();
        let second = rx.try_recv().unwrap();
        assert_eq!(first.run_id, run_id);
        assert_eq!(first.envelope.seq, 0);
        assert_eq!(second.envelope.seq, 1);
        assert!(second.gseq > first.gseq);

        let log = storage::read_event_log(dir.path(), run_id).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].seq, 0);
        assert_eq!(log[1].seq, 1);
    }
}

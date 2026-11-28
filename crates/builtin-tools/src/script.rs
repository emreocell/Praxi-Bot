//! `script_run` (permission category `Script`) and the permission gate used for
//! tool calls made from inside a script.
//!
//! A script can only reach tools through [`GateHost`], which looks the tool up in
//! the registry and asks a [`ToolGate`] for permission **before every call**, so
//! inner calls are checked exactly like direct ones. The agent loop will
//! implement [`ToolGate`] on top of its approval flow; [`ResolverGate`] is a
//! non-interactive implementation on top of `permission-engine`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use permission_engine::{ActionRequest, Decision, PermissionResolver, Policy};
use protocol::InitiatorKind;
use script_runtime::{Limits, ScriptError, ScriptHost, ScriptRuntime};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    ContentClass, ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError,
    ToolOutput, ToolRegistry, ToolSpec,
};

use crate::util::{exec_err, parse, spec};

/// Name of the script tool; scripts may not call it recursively.
pub const SCRIPT_TOOL_NAME: &str = "script_run";

/// What the gate is asked to authorize.
#[derive(Debug, Clone)]
pub struct GateRequest {
    /// Id of the run the script belongs to (as given in the `ToolContext`).
    pub run_id: String,
    /// Tool name.
    pub tool: String,
    /// Permission category of the tool.
    pub category: PermissionCategory,
    /// Effect level of the tool.
    pub effect: Effect,
    /// Command text for never-list matching (see [`raw_command_for`]).
    pub raw_command: String,
    /// The call's input.
    pub input: Value,
    /// Classification of the tool being called. The loop records taint from
    /// this after a successful authorization.
    pub content: ContentClass,
    /// The program a desktop input call would reach, resolved by the caller
    /// (workflow replay). `None` for script calls: a script never gets a
    /// run-scoped control grant.
    pub control: Option<tool_registry::ControlTarget>,
}

/// Facts the registry tool must enforce when it executes the authorized call.
#[derive(Debug, Default)]
pub struct GateAuthorization {
    /// Origin that received browser authorization.
    pub expected_origin: Option<String>,
    /// This mail may run without consuming an owner's warning override.
    pub unattended_mail: bool,
    /// The owner's own Full-level chat turn changes a bot without a card (see
    /// [`tool_registry::ToolContext::full_waived_preview`]). Only the agent loop's own tool
    /// call sets it; a script's call never does.
    pub full_waived_preview: bool,
    /// Approval for a managed browser payment document, if required.
    pub payment: tool_registry::PaymentApproval,
}

/// Permission check for tool calls issued from scripts.
///
/// The agent loop implements this with its full approval flow (it may suspend
/// to ask the human). `Ok(())` allows the call, `Err(reason)` denies it and the
/// reason is thrown into the script as a JS error.
#[async_trait]
pub trait ToolGate: Send + Sync {
    /// Authorize one tool call.
    ///
    /// # Errors
    /// A human-readable denial reason.
    async fn authorize(&self, req: &GateRequest) -> Result<(), String>;

    /// [`Self::authorize`] for a caller that sends the input itself (workflow
    /// replay). Returns the program the caller must pass to the backend's
    /// guarded path when a run-scoped control grant covers the call, `None`
    /// otherwise. The default has no grants.
    ///
    /// # Errors
    /// A human-readable denial reason.
    async fn authorize_control(
        &self,
        req: &GateRequest,
    ) -> Result<Option<tool_registry::ControlIdentity>, String> {
        self.authorize(req).await.map(|()| None)
    }

    /// Authorize a registry tool call whose caller forwards the origin to `ToolContext`.
    /// # Errors
    /// A human-readable denial reason.
    async fn authorize_tool(&self, req: &GateRequest) -> Result<GateAuthorization, String> {
        self.authorize(req)
            .await
            .map(|()| GateAuthorization::default())
    }

    /// Whether the call would run right now without asking anyone: the run's
    /// profile offers the tool and policy allows it outright. Unlike
    /// [`Self::authorize`] it never opens an approval card and records nothing
    /// the call reads, so the caller must throw the output away (the
    /// `tool_self_check` probes). `Err` is any reason the call may not run
    /// unattended; callers must not show its text to the model. The default
    /// refuses: a gate that cannot answer keeps unattended calls from running.
    ///
    /// # Errors
    /// The call is hidden, denied or needs approval.
    async fn authorize_unattended(&self, _req: &GateRequest) -> Result<(), String> {
        Err("this gate has no unattended mode".into())
    }

    /// Withdraw every task-wide permission of `run_id`. A caller that sent a
    /// guarded call itself (workflow replay) calls this when that send stopped
    /// or failed, as the agent loop does for its own tool calls. The default
    /// has no grants.
    fn revoke_control(&self, _run_id: &str) {}

    /// Record content consumed by a composite tool before its inner calls run.
    /// Only host code supplies this classification; model input cannot clear it.
    fn note_exposure(&self, _run_id: &str, _content: ContentClass, _label: &str) {}

    /// Record trusted host metadata carried by a tool result, including a folder trust offer.
    fn note_output_exposure(&self, run_id: &str, exposure: &tool_registry::OutputExposure) {
        self.note_exposure(
            run_id,
            ContentClass::declared(exposure.ingests_untrusted, exposure.reads_private, false),
            &exposure.label,
        );
    }

    /// Whether `run_id` has already read untrusted or private content.
    ///
    /// Skill writes use this to store the skill as untrusted data. The default
    /// is false: a gate that does not track taint does not mark skills.
    fn content_tainted(&self, _run_id: &str) -> bool {
        false
    }

    /// Whether the run holds UNTRUSTED content: files a tool writes in such a run get an
    /// untrusted-origin mark. Default false.
    fn content_untrusted(&self, _run_id: &str) -> bool {
        false
    }
}

/// Non-interactive [`ToolGate`] backed by [`PermissionResolver`]: `Allow` passes,
/// `Ask` and `Deny` are refused (a script cannot wait for a human).
pub struct ResolverGate {
    resolver: PermissionResolver,
    global: Policy,
    bot: Option<Policy>,
    initiator: InitiatorKind,
}

impl ResolverGate {
    /// Create a gate evaluating `global` (and optionally `bot`) policies.
    #[must_use]
    pub fn new(
        resolver: PermissionResolver,
        global: Policy,
        bot: Option<Policy>,
        initiator: InitiatorKind,
    ) -> Self {
        Self {
            resolver,
            global,
            bot,
            initiator,
        }
    }
}

#[async_trait]
impl ToolGate for ResolverGate {
    async fn authorize(&self, req: &GateRequest) -> Result<(), String> {
        let action = ActionRequest {
            cwd: cwd_for(&req.tool, &req.input),
            category: req.category,
            raw_command: req.raw_command.clone(),
            initiator: self.initiator,
        };
        match self
            .resolver
            .resolve(&self.global, self.bot.as_ref(), None, &action)
        {
            Decision::Allow => Ok(()),
            Decision::Ask { .. } => Err(format!(
                "permission denied: {} needs approval, which is not available inside scripts",
                req.tool
            )),
            Decision::Deny => Err(format!("permission denied: {} is not allowed", req.tool)),
        }
    }

    /// This gate never asks anyone, so unattended is the same as authorized.
    async fn authorize_unattended(&self, req: &GateRequest) -> Result<(), String> {
        self.authorize(req).await
    }
}

/// The text the never-list should see for a call: the shell command, or the
/// executable and arguments of an app launch. Empty for other tools.
#[must_use]
pub fn raw_command_for(tool: &str, input: &Value) -> String {
    match tool {
        "shell_run" => input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        // `start` names the program; `write` types text into one that is already running, so the
        // never-list sees what is typed too (a bare shell cannot be started, see `pty_shell`).
        "shell_interactive" => {
            let field = match input.get("action").and_then(Value::as_str) {
                Some("start") => "command",
                Some("write") => "input",
                _ => return String::new(),
            };
            input
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        // The owner's own computer launches a program the same way: the never-list and the
        // money floor read the full command line there too.
        "desktop_launch" | "pc_desktop_launch" => {
            let exe = input
                .get("executable")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args: Vec<&str> = input
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            format!("{exe} {}", args.join(" ")).trim().to_string()
        }
        _ => String::new(),
    }
}

/// The working directory a `shell_run` call names (absolute only), for path resolution in the
/// permission layer.
pub fn cwd_for(tool: &str, input: &Value) -> Option<String> {
    matches!(tool, "shell_run" | "shell_interactive")
        .then(|| input.get("cwd").and_then(Value::as_str))
        .flatten()
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

/// Convert a tool output into the JSON a script sees.
fn output_to_json(out: &ToolOutput) -> Value {
    let mut parts: Vec<Value> = out
        .content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => Value::String(text.clone()),
            ContentPart::Json { data } => data.clone(),
            ContentPart::Binary { mime_type, base64 } => {
                json!({"mime_type": mime_type, "bytes_base64_len": base64.len()})
            }
        })
        .collect();
    if let Some(s) = &out.spill {
        parts.push(json!({"spill_path": s.path, "size_bytes": s.size_bytes}));
    }
    if parts.len() == 1 {
        parts.remove(0)
    } else {
        Value::Array(parts)
    }
}

/// [`ScriptHost`] that routes script tool calls through the registry with a
/// permission check on every call.
pub struct GateHost {
    action: Option<crate::control::ScriptAction>,
    registry: ToolRegistry,
    gate: Arc<dyn ToolGate>,
    run_id: String,
    max_inline_bytes: usize,
    bot_id: Option<String>,
    lane: Option<String>,
}

impl GateHost {
    /// Create a host for one script run.
    ///
    /// `bot_id` is copied onto every inner tool call so an MCP server's bot
    /// allowlist still applies inside a script.
    #[must_use]
    pub fn new(
        registry: ToolRegistry,
        gate: Arc<dyn ToolGate>,
        run_id: String,
        max_inline_bytes: usize,
        bot_id: Option<String>,
    ) -> Self {
        Self {
            action: None,
            registry,
            gate,
            run_id,
            max_inline_bytes,
            bot_id,
            lane: None,
        }
    }
}

#[async_trait]
impl ScriptHost for GateHost {
    async fn call_tool(&self, name: &str, input: Value) -> Result<Value, String> {
        if name == SCRIPT_TOOL_NAME {
            return Err("scripts cannot call script_run".into());
        }
        // Scripts run on another thread, so a workflow's task-local recursion
        // guard cannot see through them; workflows stay out of scripts.
        if name == "workflow_run" {
            return Err("scripts cannot call workflow_run".into());
        }
        let tool = self
            .registry
            .get(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let spec = tool.spec();
        let req = GateRequest {
            run_id: self.run_id.clone(),
            tool: name.to_string(),
            category: spec.category,
            effect: spec.effect,
            raw_command: raw_command_for(name, &input),
            input: input.clone(),
            content: spec.content,
            control: None,
        };
        let authorization = self.gate.authorize_tool(&req).await?;
        let ctx = ToolContext {
            run_id: self.run_id.clone(),
            max_inline_bytes: self.max_inline_bytes,
            bot_id: self.bot_id.clone(),
            content_tainted: self.gate.content_tainted(&self.run_id),
            exposure_untrusted: self.gate.content_untrusted(&self.run_id),
            expected_origin: authorization.expected_origin,
            unattended_mail: authorization.unattended_mail,
            full_waived_preview: false,
            payment: authorization.payment,
            // The gate does not say which kind it was: any taint counts as private here.
            exposure_private: self.gate.content_tainted(&self.run_id),
            // A script is never covered by a run-scoped control grant.
            control_guard: None,
            lane: self.lane.clone(),
        };
        let out = if let Some(action) = &self.action {
            crate::control::scope_action(action.clone(), tool.call(&ctx, input)).await
        } else {
            tool.call(&ctx, input).await
        };
        // The output's own exposure replaces the static class for the calls
        // that follow, as it does between model turns (a listing that
        // contains an untrusted skill, for example).
        if let Ok(o) = &out
            && let Some(exposure) = &o.exposure
        {
            self.gate.note_output_exposure(&self.run_id, exposure);
        }
        out.map(|o| output_to_json(&o)).map_err(|e| e.to_string())
    }
}

#[derive(Deserialize)]
struct Input {
    code: String,
    timeout: Option<u64>,
}

/// The `script_run` tool.
pub struct ScriptTool {
    spec: ToolSpec,
    registry: ToolRegistry,
    gate: Arc<dyn ToolGate>,
    limits: Limits,
    control: Option<Arc<dyn crate::control::DesktopControlGate>>,
}

/// Build `script_run`. `registry` is the registry scripts call back into (it may
/// be the registry this tool is registered in; clones share state).
#[must_use]
pub fn tool(registry: ToolRegistry, gate: Arc<dyn ToolGate>, limits: Limits) -> Arc<dyn Tool> {
    tool_with_control(registry, gate, limits, None)
}

#[must_use]
pub fn tool_with_control(
    registry: ToolRegistry,
    gate: Arc<dyn ToolGate>,
    limits: Limits,
    control: Option<Arc<dyn crate::control::DesktopControlGate>>,
) -> Arc<dyn Tool> {
    Arc::new(ScriptTool {
        spec: spec(
            SCRIPT_TOOL_NAME,
            "Run a short JavaScript program that calls several tools and returns one compact result. Use await tools.call(name, input) and return a value. Example: {\"code\":\"const r = await tools.call('fs_list',{path:'.'}); return r;\"}",
            json!({"type":"object","properties":{
                "code":{"type":"string","description":"Body of an async function; must return a JSON value."},
                "timeout":{"type":"integer","minimum":1,"description":"Seconds of the script's own computing before it stops (max 10; waiting for tools does not count)."}
            },"required":["code"]}),
            Effect::Write,
            PermissionCategory::Script,
        ),
        registry,
        gate,
        limits,
        control,
    })
}

#[async_trait]
impl Tool for ScriptTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: Input = parse(input)?;
        let mut limits = self.limits.clone();
        if let Some(t) = i.timeout {
            limits.cpu_timeout = Duration::from_secs(t).min(limits.cpu_timeout);
        }
        let mut host = GateHost::new(
            self.registry.clone(),
            self.gate.clone(),
            ctx.run_id.clone(),
            ctx.max_inline_bytes,
            ctx.bot_id.clone(),
        );
        // Admission is lazy: only a computer tool call inside the script takes
        // it (one admission for the whole run), so a script that never touches
        // the computer neither waits for a pause nor holds the computer slot.
        if self.control.is_some() {
            host.action = Some(Arc::default());
        }
        // A subagent's script keeps the subagent's own browser tab.
        host.lane.clone_from(&ctx.lane);
        let host = Arc::new(host);
        // QuickJS is synchronous CPU work: a busy loop would block the async
        // worker (and `tokio::time::timeout` cannot preempt it). Run it on the
        // blocking pool; the engine's interrupt handler enforces the deadline.
        let handle = tokio::runtime::Handle::current();
        let code = i.code;
        let cancel = tokio_util::sync::CancellationToken::new();
        // Dropping a JoinHandle detaches a blocking task. Signal the worker
        // explicitly when the outer tool future is cancelled or times out.
        let _cancel_on_drop = cancel.clone().drop_guard();
        let joined = tokio::task::spawn_blocking(move || {
            handle.block_on(ScriptRuntime::run_cancellable(&code, host, limits, cancel))
        })
        .await;
        let result = match joined {
            Ok(r) => r,
            Err(e) => return Err(exec_err(format!("script worker failed: {e}"))),
        };
        match result {
            Ok(out) => {
                let data = json!({
                    "result": out.result,
                    "logs": out.logs,
                    "tool_calls": out.tool_calls.iter().map(|c| json!({
                        "name": c.name,
                        "ok": c.error.is_none(),
                        "error": c.error,
                    })).collect::<Vec<_>>(),
                    "truncated": out.truncated,
                });
                Ok(ToolOutput::text(data.to_string()))
            }
            Err(ScriptError::Js(e)) => Err(exec_err(format!("script error: {e}"))),
            Err(e) => Err(exec_err(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use permission_engine::{Level, Rule};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    struct WaitingEffect {
        spec: ToolSpec,
        entered: Notify,
        release: Notify,
        dropped: Arc<Notify>,
        effects: AtomicUsize,
    }

    struct CallDrop(Arc<Notify>);

    impl Drop for CallDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    #[async_trait]
    impl Tool for WaitingEffect {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }

        async fn call(&self, _ctx: &ToolContext, _input: Value) -> Result<ToolOutput, ToolError> {
            let _drop = CallDrop(Arc::clone(&self.dropped));
            self.entered.notify_one();
            self.release.notified().await;
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("effect committed"))
        }
    }

    struct Echo {
        spec: ToolSpec,
        calls: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }
        async fn call(&self, _c: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
            self.calls.lock().push(input.clone());
            Ok(ToolOutput::text(format!("echo:{input}")))
        }
    }

    fn echo(name: &str, cat: PermissionCategory, calls: Arc<Mutex<Vec<Value>>>) -> Arc<dyn Tool> {
        let mut spec = spec(name, "d", json!({"type":"object"}), Effect::Read, cat);
        // Test doubles are not built-ins. Declare them as touching nothing so
        // the registry accepts them; the test is about the gate, not taint.
        if !spec.content.is_declared() {
            spec.content = tool_registry::ContentClass::none();
        }
        Arc::new(Echo { spec, calls })
    }

    /// Fake gate recording what it was asked and denying configured tools.
    struct FakeGate {
        deny: Vec<&'static str>,
        seen: Mutex<Vec<GateRequest>>,
    }

    #[async_trait]
    impl ToolGate for FakeGate {
        async fn authorize(&self, req: &GateRequest) -> Result<(), String> {
            self.seen.lock().push(req.clone());
            if self.deny.contains(&req.tool.as_str()) {
                Err(format!("denied {}", req.tool))
            } else {
                Ok(())
            }
        }
    }

    fn setup(deny: Vec<&'static str>) -> (ToolRegistry, Arc<FakeGate>, Arc<Mutex<Vec<Value>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let reg = ToolRegistry::new();
        reg.register(echo("safe", PermissionCategory::Fs, calls.clone()));
        reg.register(echo("shell_run", PermissionCategory::Shell, calls.clone()));
        let gate = Arc::new(FakeGate {
            deny,
            seen: Mutex::new(Vec::new()),
        });
        reg.register(tool(reg.clone(), gate.clone(), Limits::default()));
        (reg, gate, calls)
    }

    async fn run(reg: &ToolRegistry, code: &str) -> Result<ToolOutput, ToolError> {
        reg.get(SCRIPT_TOOL_NAME)
            .unwrap()
            .call(&ToolContext::default(), json!({"code": code}))
            .await
    }

    #[tokio::test]
    async fn dropping_script_call_cancels_in_flight_tool() {
        let (reg, _gate, _calls) = setup(vec![]);
        let effect = Arc::new(WaitingEffect {
            spec: {
                let mut spec = spec(
                    "delayed_effect",
                    "d",
                    json!({"type":"object"}),
                    Effect::Write,
                    PermissionCategory::Fs,
                );
                // A test double: declare it as touching nothing so the registry accepts it.
                spec.content = tool_registry::ContentClass::none();
                spec
            },
            entered: Notify::new(),
            release: Notify::new(),
            dropped: Arc::new(Notify::new()),
            effects: AtomicUsize::new(0),
        });
        reg.register(effect.clone());
        let call = tokio::spawn(async move {
            run(&reg, "return await tools.call('delayed_effect', {});").await
        });
        tokio::time::timeout(Duration::from_secs(5), effect.entered.notified())
            .await
            .expect("script did not enter its inner tool");
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        let dropped = tokio::time::timeout(Duration::from_secs(2), effect.dropped.notified()).await;
        // Always release a detached old worker so a failing test can shut
        // its runtime down without waiting for the full script deadline.
        effect.release.notify_one();
        assert!(
            dropped.is_ok(),
            "inner tool outlived the cancelled script call"
        );
        assert_eq!(effect.effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn busy_loop_does_not_block_the_async_runtime() {
        // Default #[tokio::test] is a current_thread runtime.
        let (reg, _gate, _calls) = setup(vec![]);
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let t2 = ticks.clone();
        let ticker = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                t2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let started = std::time::Instant::now();
        let err = reg
            .get(SCRIPT_TOOL_NAME)
            .unwrap()
            .call(
                &ToolContext::default(),
                json!({"code": "while (true) {}", "timeout": 1}),
            )
            .await
            .unwrap_err();
        ticker.abort();
        assert!(err.to_string().to_lowercase().contains("time"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(8));
        assert!(
            ticks.load(std::sync::atomic::Ordering::SeqCst) >= 5,
            "runtime was starved: {} ticks",
            ticks.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    struct SlowTool {
        spec: ToolSpec,
    }

    #[async_trait]
    impl Tool for SlowTool {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }
        async fn call(&self, _c: &ToolContext, _input: Value) -> Result<ToolOutput, ToolError> {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            Ok(ToolOutput::text("slow done"))
        }
    }

    /// The `timeout` the model passes is a compute budget: a script that spends
    /// longer than that waiting on a tool (a page load, a download) still
    /// finishes, as long as its own work is short.
    #[tokio::test]
    async fn waiting_on_a_slow_tool_does_not_use_up_the_script_timeout() {
        let (reg, _gate, _calls) = setup(vec![]);
        let mut slow = spec(
            "slow",
            "d",
            json!({"type":"object"}),
            Effect::Read,
            PermissionCategory::Fs,
        );
        slow.content = tool_registry::ContentClass::none();
        reg.register(Arc::new(SlowTool { spec: slow }));
        let started = std::time::Instant::now();
        let out = reg
            .get(SCRIPT_TOOL_NAME)
            .unwrap()
            .call(
                &ToolContext::default(),
                json!({"code": "return await tools.call('slow', {});", "timeout": 1}),
            )
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(1500));
        let ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("slow done"), "{text}");
    }

    /// A model cannot ask for more than the configured compute budget.
    #[tokio::test]
    async fn the_script_timeout_argument_can_only_lower_the_compute_budget() {
        let (reg, _gate, _calls) = setup(vec![]);
        let started = std::time::Instant::now();
        let err = reg
            .get(SCRIPT_TOOL_NAME)
            .unwrap()
            .call(
                &ToolContext::default(),
                json!({"code": "while (true) {}", "timeout": 1}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("compute budget"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[tokio::test]
    async fn allowed_inner_calls_run_through_the_gate() {
        let (reg, gate, calls) = setup(vec![]);
        let out = run(
            &reg,
            "const a = await tools.call('safe', {x: 1}); return a;",
        )
        .await
        .unwrap();
        let ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("echo:"), "{text}");
        assert_eq!(calls.lock().len(), 1);
        let seen = gate.seen.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].tool, "safe");
        assert_eq!(seen[0].category, PermissionCategory::Fs);
    }

    #[tokio::test]
    async fn denied_inner_call_never_executes_and_script_sees_the_error() {
        let (reg, _gate, calls) = setup(vec!["shell_run"]);
        let out = run(
            &reg,
            "try { await tools.call('shell_run', {command: 'rm -rf /'}); return 'ran'; } catch (e) { return 'blocked: ' + e.message; }",
        )
        .await
        .unwrap();
        let ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("blocked: denied shell_run"), "{text}");
        assert!(calls.lock().is_empty(), "denied tool must not run");
    }

    #[tokio::test]
    async fn gate_receives_raw_command_for_never_list() {
        let (reg, gate, _calls) = setup(vec![]);
        run(&reg, "await tools.call('shell_run', {command: 'ls -la'});")
            .await
            .unwrap();
        assert_eq!(gate.seen.lock()[0].raw_command, "ls -la");
    }

    #[tokio::test]
    async fn recursion_and_unknown_tools_are_refused() {
        let (reg, gate, _calls) = setup(vec![]);
        let out = run(
            &reg,
            "let r = []; for (const n of ['script_run','workflow_run','nope']) { try { await tools.call(n, {}); } catch (e) { r.push(e.message); } } return r;",
        )
        .await
        .unwrap();
        let ContentPart::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("cannot call script_run"), "{text}");
        assert!(text.contains("cannot call workflow_run"), "{text}");
        assert!(text.contains("unknown tool"), "{text}");
        assert!(gate.seen.lock().is_empty());
    }

    #[tokio::test]
    async fn resolver_gate_applies_policy_and_never_list() {
        let mut global = Policy::full();
        global
            .categories
            .insert(PermissionCategory::Fs, Rule::new(Level::Deny));
        let gate = ResolverGate::new(
            PermissionResolver::new(),
            global,
            None,
            InitiatorKind::Human,
        );
        let req = |tool: &str, category, raw: &str| GateRequest {
            run_id: String::new(),
            tool: tool.into(),
            category,
            effect: Effect::Write,
            raw_command: raw.into(),
            input: Value::Null,
            content: ContentClass::none(),
            control: None,
        };
        assert!(
            gate.authorize(&req("fs_write", PermissionCategory::Fs, ""))
                .await
                .is_err()
        );
        assert!(
            gate.authorize(&req("shell_run", PermissionCategory::Shell, "echo hi"))
                .await
                .is_ok()
        );
        // The never-list forces Ask, which scripts cannot satisfy.
        let err = gate
            .authorize(&req("shell_run", PermissionCategory::Shell, "rm -rf /"))
            .await
            .unwrap_err();
        assert!(err.contains("needs approval"), "{err}");
        // Typing a catastrophic command into an interactive session is judged the same way.
        let typed = raw_command_for(
            "shell_interactive",
            &json!({"action":"write","session":"pty-1","input":"rm -rf /","enter":true}),
        );
        let err = gate
            .authorize(&req("shell_interactive", PermissionCategory::Shell, &typed))
            .await
            .unwrap_err();
        assert!(err.contains("needs approval"), "{err}");
    }

    #[test]
    fn raw_command_extraction() {
        assert_eq!(raw_command_for("shell_run", &json!({"command":"x"})), "x");
        assert_eq!(
            raw_command_for(
                "desktop_launch",
                &json!({"executable":"a.exe","args":["1","2"]})
            ),
            "a.exe 1 2"
        );
        assert_eq!(raw_command_for("fs_read", &json!({"path":"p"})), "");
        // The interactive shell: the program on `start`, the typed text on `write`, nothing else.
        let interactive = |input: Value| raw_command_for("shell_interactive", &input);
        assert_eq!(
            interactive(json!({"action":"start","command":"vibe --setup","input":"x"})),
            "vibe --setup"
        );
        assert_eq!(
            interactive(json!({"action":"write","session":"s","input":"rm -rf /"})),
            "rm -rf /"
        );
        assert_eq!(interactive(json!({"action":"read","session":"s"})), "");
        assert_eq!(
            interactive(json!({"action":"write","session":"s","key":"enter"})),
            ""
        );
    }
}

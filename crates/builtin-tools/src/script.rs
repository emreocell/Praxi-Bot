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
    ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput,
    ToolRegistry, ToolSpec,
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
        "desktop_launch" => {
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
    action_permit: Option<crate::control::ActionPermit>,
    registry: ToolRegistry,
    gate: Arc<dyn ToolGate>,
    run_id: String,
    max_inline_bytes: usize,
    bot_id: Option<String>,
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
            action_permit: None,
            registry,
            gate,
            run_id,
            max_inline_bytes,
            bot_id,
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
        };
        self.gate.authorize(&req).await?;
        let ctx = ToolContext {
            run_id: self.run_id.clone(),
            max_inline_bytes: self.max_inline_bytes,
            bot_id: self.bot_id.clone(),
        };
        let out = if let Some(permit) = &self.action_permit {
            crate::control::scope_action(permit.clone(), tool.call(&ctx, input)).await
        } else {
            tool.call(&ctx, input).await
        };
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
                "timeout":{"type":"integer","minimum":1,"description":"Seconds before the script is stopped."}
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
            limits.timeout = Duration::from_secs(t).min(limits.timeout);
        }
        // Admission precedes QuickJS's wall-clock budget. Already admitted
        // scripts drain with a lifetime permit, including all native tool calls.
        let action_permit = if let Some(control) = &self.control {
            Some(
                control
                    .admit_background_action(&crate::control::ControlRequest {
                        run_id: ctx.run_id.clone(),
                        rect: None,
                        label: "script computer actions".into(),
                        action: "script".into(),
                    })
                    .await
                    .map_err(exec_err)?,
            )
        } else {
            None
        };
        if action_permit
            .as_ref()
            .is_some_and(crate::control::ActionPermit::needs_observation)
        {
            return Ok(ToolOutput::text(
                "Human intervention completed; previous script discarded. Obtain a fresh normal observation and replan.",
            ));
        }
        let mut host = GateHost::new(
            self.registry.clone(),
            self.gate.clone(),
            ctx.run_id.clone(),
            ctx.max_inline_bytes,
            ctx.bot_id.clone(),
        );
        host.action_permit = action_permit;
        let host = Arc::new(host);
        // QuickJS is synchronous CPU work: a busy loop would block the async
        // worker (and `tokio::time::timeout` cannot preempt it). Run it on the
        // blocking pool; the engine's interrupt handler enforces the deadline.
        let handle = tokio::runtime::Handle::current();
        let code = i.code;
        let joined = tokio::task::spawn_blocking(move || {
            handle.block_on(ScriptRuntime::run(&code, host, limits))
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
        Arc::new(Echo {
            spec: spec(name, "d", json!({"type":"object"}), Effect::Read, cat),
            calls,
        })
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
    }
}

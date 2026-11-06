//! Embedded script interpreter sandbox for code-as-action execution.
//!
//! The model writes a short JavaScript program that calls our tools several
//! times, filters the data and returns a compact result. The program runs in an
//! embedded `QuickJS` engine (`rquickjs`) with **no ambient capabilities**: there
//! is no filesystem, network, process or module loader. The only way out of the
//! sandbox is [`ScriptHost::call_tool`], exposed to JS as
//! `await tools.call("name", {...})`. `console.log` is captured into
//! [`ScriptOutput::logs`].
//!
//! Script contract: the code is the *body of an async function*. Use `await`
//! freely and `return` the final (JSON-serialisable) value.
//!
//! ```js
//! const a = await tools.call("search", { q: "rust" });
//! console.log("found", a.length);
//! return a.filter(x => x.score > 0.5).map(x => x.id);
//! ```
//!
//! Hard limits ([`Limits`]): wall-clock timeout, memory, tool-call count and
//! output size. Errors ([`ScriptError`]) carry the JS message, stack and line
//! number so the model can fix its script.
//!
//! Note: `QuickJS` is single threaded and a busy loop occupies the polling
//! thread until the interrupt handler fires (at most `timeout`). Callers that
//! cannot afford this on an async worker should run the future with
//! `spawn_blocking` or on a dedicated runtime.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use rquickjs::context::intrinsic;
use rquickjs::function::Async;
use rquickjs::{
    AsyncContext, AsyncRuntime, Ctx, Error as QjsError, Function, Object, Promise, Value,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

/// Marker appended to anything that was cut because of `max_output_bytes`.
pub const TRUNCATION_MARKER: &str = "…[truncated]";

/// The host side of the sandbox: the only capability a script has.
///
/// The real implementation (permission checks, tool registry) lives in
/// `agent-core`; tests use a fake host.
#[async_trait]
pub trait ScriptHost: Send + Sync {
    /// Execute the tool `name` with `input` and return its JSON result, or an
    /// error message that is thrown into the script as a JS `Error`.
    ///
    /// # Errors
    /// Returns a human-readable message when the tool fails or is denied.
    async fn call_tool(&self, name: &str, input: Json) -> Result<Json, String>;
}

/// Hard resource limits for a single script run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock budget for the whole run (including awaited tool calls).
    pub timeout: Duration,
    /// Maximum JS heap size in bytes.
    pub memory_bytes: usize,
    /// Maximum JS native stack size in bytes (guards against deep recursion).
    pub max_stack_bytes: usize,
    /// Maximum number of `tools.call` invocations per run.
    pub max_tool_calls: usize,
    /// Maximum size in bytes of the serialised result and of the captured
    /// logs. Larger data is truncated with [`TRUNCATION_MARKER`].
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            memory_bytes: 32 * 1024 * 1024,
            max_stack_bytes: 512 * 1024,
            max_tool_calls: 50,
            max_output_bytes: 64 * 1024,
        }
    }
}

/// One tool invocation made by a script, for audit and debugging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Tool name.
    pub name: String,
    /// Input passed by the script.
    pub input: Json,
    /// Tool output on success.
    pub output: Option<Json>,
    /// Error message on failure.
    pub error: Option<String>,
    /// How long the host call took.
    pub duration: Duration,
}

/// Successful result of a script run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptOutput {
    /// The returned value (`null` if the script returns nothing). If the
    /// serialised value exceeded the limit this is a truncated JSON *string*.
    pub result: Json,
    /// Captured `console.*` lines.
    pub logs: Vec<String>,
    /// Every tool call, in order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Total wall-clock duration.
    pub duration: Duration,
    /// True if the result or the logs were truncated.
    pub truncated: bool,
}

/// A JavaScript exception with the position information the model needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsError {
    /// Exception message (e.g. `SyntaxError: unexpected token in expression`).
    pub message: String,
    /// JS stack trace, when available.
    pub stack: Option<String>,
    /// 1-based line number in the submitted code.
    pub line: Option<u32>,
    /// 1-based column number in the submitted code.
    pub column: Option<u32>,
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        match (self.line, self.column) {
            (Some(l), Some(c)) => write!(f, " (line {l}, column {c})")?,
            (Some(l), None) => write!(f, " (line {l})")?,
            _ => {}
        }
        if let Some(stack) = &self.stack
            && !stack.trim().is_empty()
        {
            write!(f, "\n{}", stack.trim_end())?;
        }
        Ok(())
    }
}

/// Why a script run failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// The script threw (including syntax errors).
    #[error("script error: {0}")]
    Js(JsError),
    /// The wall-clock timeout elapsed.
    #[error("script timed out after {0:?}")]
    Timeout(Duration),
    /// The JS heap exceeded the memory limit.
    #[error("script exceeded the memory limit of {limit_bytes} bytes")]
    MemoryLimitExceeded {
        /// Configured limit.
        limit_bytes: usize,
    },
    /// The script tried to call more tools than allowed.
    #[error("script exceeded the tool call limit of {max}")]
    ToolCallLimitExceeded {
        /// Configured limit.
        max: usize,
    },
    /// Engine setup or other internal failure.
    #[error("script runtime internal error: {0}")]
    Internal(String),
}

/// Entry point: runs JavaScript programs in the sandbox.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScriptRuntime;

/// Shared mutable state between the JS closures and the runner.
struct RunState {
    limits: Limits,
    host: Arc<dyn ScriptHost>,
    calls_started: AtomicUsize,
    call_limit_hit: AtomicBool,
    records: Mutex<Vec<ToolCallRecord>>,
    logs: Mutex<LogBuffer>,
}

#[derive(Default)]
struct LogBuffer {
    lines: Vec<String>,
    bytes: usize,
    truncated: bool,
}

impl RunState {
    fn push_log(&self, line: &str) {
        let mut logs = self.logs.lock();
        if logs.truncated {
            return;
        }
        let max = self.limits.max_output_bytes;
        if logs.bytes + line.len() > max {
            let room = max.saturating_sub(logs.bytes);
            let mut cut = truncate_str(line, room).to_owned();
            cut.push_str(TRUNCATION_MARKER);
            logs.lines.push(cut);
            logs.truncated = true;
        } else {
            logs.bytes += line.len();
            logs.lines.push(line.to_owned());
        }
    }

    /// Implements `tools.call`. Always returns JSON text of the shape
    /// `{"ok": <value>}` or `{"err": "<message>"}`; the JS shim turns it into a
    /// resolved value or a thrown `Error`.
    async fn call_tool(&self, name: String, input_json: String) -> String {
        let max = self.limits.max_tool_calls;
        if self.calls_started.fetch_add(1, Ordering::SeqCst) >= max {
            self.call_limit_hit.store(true, Ordering::SeqCst);
            return json!({"err": format!("tool call limit exceeded (max {max} per run)")})
                .to_string();
        }
        let input: Json = match serde_json::from_str(&input_json) {
            Ok(v) => v,
            Err(e) => return json!({"err": format!("invalid tool input: {e}")}).to_string(),
        };
        let started = Instant::now();
        let outcome = self.host.call_tool(&name, input.clone()).await;
        let duration = started.elapsed();
        let (reply, output, error) = match outcome {
            Ok(v) => (json!({"ok": v}), Some(v), None),
            Err(e) => (json!({"err": e}), None, Some(e)),
        };
        self.records.lock().push(ToolCallRecord {
            name,
            input,
            output,
            error,
            duration,
        });
        reply.to_string()
    }
}

/// JS-side glue. Receives the two native functions as arguments so nothing
/// native stays reachable through globals.
const SHIM: &str = r"(function (nativeCall, nativeLog) {
  'use strict';
  const fmt = (v) => {
    if (typeof v === 'string') return v;
    if (v === undefined) return 'undefined';
    try {
      const s = JSON.stringify(v);
      return s === undefined ? String(v) : s;
    } catch (_) {
      return String(v);
    }
  };
  const logger = (prefix) => (...args) => { nativeLog(prefix + args.map(fmt).join(' ')); };
  Object.defineProperty(globalThis, 'console', {
    value: Object.freeze({
      log: logger(''), info: logger(''), debug: logger(''),
      warn: logger('[warn] '), error: logger('[error] '),
    }),
    configurable: true, writable: true,
  });
  Object.defineProperty(globalThis, 'tools', {
    value: Object.freeze({
      call: async (name, input) => {
        const raw = await nativeCall(String(name), JSON.stringify(input === undefined ? null : input));
        const reply = JSON.parse(raw);
        if (reply.err !== undefined) throw new Error(reply.err);
        return reply.ok;
      },
    }),
    configurable: true, writable: true,
  });
})";

impl ScriptRuntime {
    /// Run `code` (the body of an async function) in a fresh sandbox.
    ///
    /// # Errors
    /// Returns a [`ScriptError`] on JS exceptions, syntax errors, timeouts,
    /// memory exhaustion, tool-call cap violations or engine failures.
    pub async fn run(
        code: &str,
        host: Arc<dyn ScriptHost>,
        limits: Limits,
    ) -> Result<ScriptOutput, ScriptError> {
        let started = Instant::now();
        let deadline = started + limits.timeout;
        let timed_out = Arc::new(AtomicBool::new(false));

        let rt = AsyncRuntime::new().map_err(|e| internal(&e))?;
        rt.set_memory_limit(limits.memory_bytes).await;
        rt.set_max_stack_size(limits.max_stack_bytes).await;
        {
            let flag = Arc::clone(&timed_out);
            rt.set_interrupt_handler(Some(Box::new(move || {
                if Instant::now() >= deadline {
                    flag.store(true, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            })))
            .await;
        }
        let ctx = AsyncContext::custom::<(
            intrinsic::Eval,
            intrinsic::Json,
            intrinsic::Promise,
            intrinsic::Date,
            intrinsic::RegExp,
            intrinsic::RegExpCompiler,
            intrinsic::MapSet,
            intrinsic::TypedArrays,
            intrinsic::Proxy,
        )>(&rt)
        .await
        .map_err(|e| internal(&e))?;

        let state = Arc::new(RunState {
            limits: limits.clone(),
            host,
            calls_started: AtomicUsize::new(0),
            call_limit_hit: AtomicBool::new(false),
            records: Mutex::new(Vec::new()),
            logs: Mutex::new(LogBuffer::default()),
        });

        // The wrapper prefix has no newline, so the user's code keeps the
        // same line numbers as the submitted source.
        let source = format!("(async () => {{{code}\n}})()");
        let work = {
            let state = Arc::clone(&state);
            ctx.async_with(async move |ctx| execute(&ctx, &source, &state).await)
        };

        let Ok(outcome) = tokio::time::timeout(limits.timeout, work).await else {
            return Err(ScriptError::Timeout(limits.timeout));
        };

        if state.call_limit_hit.load(Ordering::SeqCst) {
            return Err(ScriptError::ToolCallLimitExceeded {
                max: limits.max_tool_calls,
            });
        }
        if timed_out.load(Ordering::SeqCst) {
            return Err(ScriptError::Timeout(limits.timeout));
        }
        let oom = || ScriptError::MemoryLimitExceeded {
            limit_bytes: limits.memory_bytes,
        };
        let result_json = match outcome {
            Ok(s) => s,
            Err(Failure::Js(e)) if is_oom(&e.message) => return Err(oom()),
            Err(Failure::Js(e)) => return Err(ScriptError::Js(e)),
            Err(Failure::Oom) => return Err(oom()),
            Err(Failure::Internal(m)) => return Err(ScriptError::Internal(m)),
        };

        let (result, result_truncated) = shape_result(&result_json, limits.max_output_bytes)?;
        let (logs, logs_truncated) = {
            let logs = state.logs.lock();
            (logs.lines.clone(), logs.truncated)
        };
        let tool_calls = state.records.lock().clone();
        Ok(ScriptOutput {
            result,
            logs,
            tool_calls,
            duration: started.elapsed(),
            truncated: result_truncated || logs_truncated,
        })
    }
}

enum Failure {
    Js(JsError),
    Oom,
    Internal(String),
}

fn internal(e: &QjsError) -> ScriptError {
    ScriptError::Internal(e.to_string())
}

fn is_oom(message: &str) -> bool {
    message.contains("out of memory")
}

/// Install the shim, evaluate the program and return the JSON text of its
/// result.
async fn execute(ctx: &Ctx<'_>, source: &str, state: &Arc<RunState>) -> Result<String, Failure> {
    let call_fn = {
        let state = Arc::clone(state);
        Function::new(
            ctx.clone(),
            Async(move |name: String, input: String| {
                let state = Arc::clone(&state);
                async move { state.call_tool(name, input).await }
            }),
        )
    };
    let log_fn = {
        let state = Arc::clone(state);
        Function::new(ctx.clone(), move |line: String| state.push_log(&line))
    };
    let (call_fn, log_fn) = match (call_fn, log_fn) {
        (Ok(c), Ok(l)) => (c, l),
        (Err(e), _) | (_, Err(e)) => return Err(classify(ctx, e)),
    };

    let install: Function = ctx.eval(SHIM).map_err(|e| classify(ctx, e))?;
    install
        .call::<_, ()>((call_fn, log_fn))
        .map_err(|e| classify(ctx, e))?;

    let promise: Promise = ctx.eval(source).map_err(|e| classify(ctx, e))?;
    let value: Value = promise
        .into_future::<Value>()
        .await
        .map_err(|e| classify(ctx, e))?;
    match ctx.json_stringify(value) {
        Ok(Some(s)) => s.to_string().map_err(|e| classify(ctx, e)),
        Ok(None) => Ok("null".to_owned()),
        Err(e) => Err(classify(ctx, e)),
    }
}

/// Turn an engine error into a [`Failure`], pulling message/stack/line from the
/// pending JS exception when there is one.
fn classify(ctx: &Ctx<'_>, err: QjsError) -> Failure {
    match err {
        QjsError::Allocation => Failure::Oom,
        QjsError::Exception => Failure::Js(describe_exception(&ctx.catch())),
        other => Failure::Internal(other.to_string()),
    }
}

fn describe_exception(value: &Value<'_>) -> JsError {
    if let Some(obj) = value.as_object() {
        let text = |key: &str| obj.get::<_, Option<String>>(key).ok().flatten();
        let message = text("message").unwrap_or_default();
        let name = text("name").unwrap_or_default();
        let stack = text("stack").filter(|s| !s.trim().is_empty());
        let (mut line, mut column) = stack.as_deref().map_or((None, None), parse_position);
        if line.is_none() {
            line = number_prop(obj, "lineNumber");
            column = number_prop(obj, "columnNumber");
        }
        let message = if name.is_empty() || message.starts_with(&name) {
            message
        } else {
            format!("{name}: {message}")
        };
        return JsError {
            message,
            stack,
            line,
            column,
        };
    }
    let message = value
        .as_string()
        .and_then(|s| s.to_string().ok())
        .unwrap_or_else(|| format!("{value:?}"));
    JsError {
        message: format!("uncaught value thrown: {message}"),
        stack: None,
        line: None,
        column: None,
    }
}

fn number_prop(obj: &Object<'_>, key: &str) -> Option<u32> {
    obj.get::<_, Option<u32>>(key)
        .ok()
        .flatten()
        .filter(|n| *n > 0)
}

/// Find the first `:<line>:<col>` position in a `QuickJS` stack trace.
fn parse_position(stack: &str) -> (Option<u32>, Option<u32>) {
    for frame in stack.lines() {
        let frame = frame.trim().trim_end_matches(')');
        let mut parts = frame.rsplitn(3, ':');
        let (Some(col), Some(line)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let (Ok(l), Ok(c)) = (line.parse::<u32>(), col.parse::<u32>()) {
            return (Some(l), Some(c));
        }
    }
    (None, None)
}

/// Apply the output size limit to the serialised result.
fn shape_result(result_json: &str, max: usize) -> Result<(Json, bool), ScriptError> {
    if result_json.len() <= max {
        let value = serde_json::from_str(result_json)
            .map_err(|e| ScriptError::Internal(format!("result is not valid JSON: {e}")))?;
        return Ok((value, false));
    }
    let mut text = truncate_str(result_json, max).to_owned();
    text.push_str(TRUNCATION_MARKER);
    Ok((Json::String(text), true))
}

/// Longest prefix of `s` that is at most `max` bytes and ends on a char
/// boundary.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests;

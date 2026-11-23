//! Tier-1 scenario format (YAML) and loader.
//!
//! ```yaml
//! name: approval-ask-approve
//! description: ...
//! policy: approve_destructive     # full | approve_destructive | read_only | read_and_draft
//! profile_toml: ""                # extra bot profile TOML ([permissions], [tools] ...)
//! guards: { max_steps: 5 }        # optional overrides
//! subagents: false                # register spawn_subagent
//! workflows: []                   # saved workflows (version, name, steps: [{kind: tool, tool, input}])
//! tools:                          # fake tools
//!   - { name: t_write, effect: write, category: fs, output: "written" }
//! user: "do it"
//! script:                         # model responses, in order
//!   - tool_calls: [{ id: c1, tool: t_write, args: {} }]
//!   - text: "done"
//! reactions:                      # how the simulated user reacts to events
//!   - { on: approval_needed, tool: t_write, do: approve }
//! expect:
//!   state: succeeded
//!   model_calls: 2
//!   tool_calls: [t_write]
//!   permissions: [{ tool: t_write, decision: approved }]
//!   final_text: { contains: ["done"] }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;

/// A tier-1 scenario.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Unique name (also the metrics key).
    pub name: String,
    /// What the scenario protects.
    #[serde(default)]
    pub description: String,
    /// Global policy preset.
    #[serde(default)]
    pub policy: PolicyPreset,
    /// Extra bot-profile TOML appended after the `[models]` table.
    #[serde(default)]
    pub profile_toml: String,
    /// Use the deferred tool surface. Older scripts replay the original definitions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred_tools: bool,
    /// Guard overrides.
    #[serde(default)]
    pub guards: GuardOverrides,
    /// Fake tools registered for the run.
    #[serde(default)]
    pub tools: Vec<FakeToolSpec>,
    /// Register the `spawn_subagent` tool.
    #[serde(default)]
    pub subagents: bool,
    /// Workflows saved before the turn (`/workflow <name>` replays one with no model call).
    #[serde(default)]
    pub workflows: Vec<workflow_replay::Workflow>,
    /// The user message that starts the turn.
    pub user: String,
    /// Scripted model responses, consumed one per model call.
    #[serde(default)]
    pub script: Vec<ScriptStep>,
    /// Recorded provider exchange (mutually exclusive with `script`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<crate::transcript::Transcript>,
    /// Reactions of the simulated user.
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// Assertions.
    #[serde(default)]
    pub expect: Expect,
    /// Wall-clock cap in milliseconds (default 20 000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Sink tools an attack aims at. A call to one that runs with no owner decision
    /// (permission `allowed`) counts as an unsafe action.
    #[serde(default)]
    pub unsafe_tools: Vec<String>,
    /// Strings that must never reach outbound text: the final reply, or the arguments
    /// of a sink call that ran without an owner decision.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Marks a known gap: the scenario is expected to FAIL today. The text must cite
    /// the gap. A failing expected-fail scenario is reported as XFAIL and does not
    /// trip the unsafe-action gate; one that starts passing fails the suite so the
    /// marker gets removed.
    #[serde(default)]
    pub expected_fail: Option<String>,
    /// Who starts the run (default `human`). A `schedule` or `routine` run must not inherit
    /// what the owner granted in a chat.
    #[serde(default)]
    pub initiator: InitiatorSpec,
    /// Run the REAL `fs_*` tools (profile `[files]` scope applies) against a throw-away
    /// workspace. `{ws}` in strings (user, script, profile, expectations, fixtures) is the
    /// workspace directory, spelled with `/`.
    #[serde(default)]
    pub fs: Option<FsFixture>,
    /// Wait this long after the run ended before counting side effects, so work that keeps
    /// going after a cancel is seen.
    #[serde(default)]
    pub settle_ms: u64,
    /// Register `secrets` with the loop's redactor (what the daemon does with vault and
    /// environment values) and scan every surface for them afterwards: the model's requests,
    /// the event stream, the audit log, stored messages and every file of the data dir.
    #[serde(default)]
    pub redact_secrets: bool,
    /// Classification metadata for prompt-injection / red-team corpus reporting.
    #[serde(default)]
    pub attack: Option<AttackMeta>,
}

/// Classification metadata for a prompt-injection or red-team scenario.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttackMeta {
    /// Corpus format version (for example, `1`).
    #[serde(default)]
    pub version: Option<u32>,
    /// Untrusted content entry point (`web_page`, `mail`, `file`, `tool_output`, `skill_text`, `filename`, `memory`, `chat`, `control`).
    pub entry_point: String,
    /// Attack shape (`hidden_text`, `markdown_link_exfil`, `instruction_in_filename`, `multilingual_turkish`, `claims_to_be_user`, ...).
    pub shape: String,
    /// What the injected instruction attempts (`exfil_url`, `exfil_mail`, `run_shell`, `desktop_type`, `benign_control`, ...).
    pub goal: String,
    /// Gate or floor expected to hold (`egress_gate`, `executes_gate`, `forbidden_egress_chars`, `money_floor`, `never_list_floor`, ...).
    pub gate: String,
}

/// Who starts the run.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitiatorSpec {
    /// The owner, in a chat.
    #[default]
    Human,
    /// A schedule.
    Schedule,
    /// A routine.
    Routine,
}

/// Files of the throw-away workspace behind the real file tools.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FsFixture {
    /// Relative path to text content.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Links: `link` is created pointing at `target` (both relative to the workspace).
    /// A directory symlink; on Windows a junction when symlinks are not permitted.
    #[serde(default)]
    pub links: Vec<LinkSpec>,
}

/// One directory link of an [`FsFixture`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    /// Where the link is created.
    pub link: String,
    /// What it points at.
    pub target: String,
}

/// Global policy preset.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyPreset {
    /// Everything allowed.
    Full,
    /// Reads allowed, writes ask.
    #[default]
    ApproveDestructive,
    /// Everything denied.
    ReadOnly,
    /// Reads and drafts allowed, sending asks.
    ReadAndDraft,
}

/// Optional overrides of the loop guards.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GuardOverrides {
    /// Maximum model calls per turn.
    pub max_steps: Option<u32>,
    /// Identical-call warning threshold.
    pub repeat_warn: Option<u32>,
    /// Identical-call stop threshold.
    pub repeat_stop: Option<u32>,
    /// Token budget.
    pub max_tokens_per_turn: Option<u64>,
    /// Spill threshold in bytes.
    pub spill_threshold_bytes: Option<usize>,
    /// Spill preview size in bytes.
    pub spill_preview_bytes: Option<usize>,
    /// Tool timeout in milliseconds.
    pub tool_timeout_ms: Option<u64>,
    /// Model-call timeout (covering provider retries) in milliseconds.
    pub model_timeout_ms: Option<u64>,
}

/// Effect of a fake tool.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectSpec {
    /// Read-only.
    Read,
    /// Reversible write.
    Write,
    /// Destructive.
    Destructive,
}

/// What a fake tool puts into the run's context or can do with it (the engine's
/// [`tool_registry::ContentClass`]). The default, `none`, keeps the taint rules out of
/// scenarios that do not test them.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSpec {
    /// No untrusted ingestion, no private read, no egress.
    #[default]
    None,
    /// Reads private data.
    PrivateRead,
    /// Returns untrusted content (a page, a message).
    Untrusted,
    /// Returns untrusted content and reads private data (mail, files).
    UntrustedPrivate,
    /// Can send data off the machine.
    Egress,
    /// Sends data out and the reply is untrusted.
    UntrustedEgress,
    /// All three.
    All,
}

/// Permission category of a fake tool.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySpec {
    /// Files.
    #[default]
    Fs,
    /// Shell.
    Shell,
    /// Desktop.
    Desktop,
    /// Browser.
    Browser,
    /// Money.
    Money,
    /// Messaging (e-mail, chat).
    Messaging,
    /// Scripts (`script_run`).
    Script,
    /// Accounts.
    Accounts,
    /// System.
    System,
    /// Remote.
    Remote,
    /// Meta.
    Meta,
}

/// A fake tool backend.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FakeToolSpec {
    /// Tool name (a `.` is exposed to the model as `__`).
    pub name: String,
    /// Optional catalog description. MCP descriptions are untrusted metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Effect.
    pub effect: EffectSpec,
    /// Category.
    #[serde(default)]
    pub category: CategorySpec,
    /// Fixed text output.
    #[serde(default)]
    pub output: String,
    /// If set, the output is this many `x` characters instead of `output`.
    #[serde(default)]
    pub output_bytes: Option<usize>,
    /// Artificial latency in milliseconds.
    #[serde(default)]
    pub delay_ms: u64,
    /// If set, the tool fails with this message.
    #[serde(default)]
    pub error: Option<String>,
    /// Content class (taint source / egress sink).
    #[serde(default)]
    pub content: ContentSpec,
    /// The call starts an arbitrary program (a run tainted by untrusted content asks).
    #[serde(default)]
    pub executes: bool,
    /// A tool of an MCP server the owner connected as a browser (name
    /// `mcp:<server>/<tool>`). Content class, effect and always-ask come from
    /// `tool_registry::browser_mcp`, exactly as the MCP bridge registers them;
    /// `content`, `effect` and `executes` are ignored. A tool name the browser
    /// rules do not know keeps the generic MCP class (untrusted, private,
    /// egress, destructive).
    #[serde(default)]
    pub browser_mcp: bool,
}

/// Token usage reported with a scripted reply.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSpec {
    /// Prompt tokens.
    pub prompt: u64,
    /// Completion tokens.
    pub completion: u64,
    /// Cached prompt tokens.
    #[serde(default)]
    pub cached: Option<u64>,
}

/// One tool call inside a scripted reply.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptCall {
    /// Provider call id.
    pub id: String,
    /// Tool name.
    pub tool: String,
    /// Arguments (serialised to JSON text).
    #[serde(default)]
    pub args: Value,
    /// Raw argument text, to script malformed JSON (overrides `args`).
    #[serde(default)]
    pub raw_args: Option<String>,
    /// Expand into this many calls in one reply. `{i}` (0-based) in `id` and in string
    /// arguments becomes the call's index, so every copy is distinct.
    #[serde(default)]
    pub times: Option<u32>,
}

/// Replace `from` by `to` in every string of a JSON value.
fn substitute(v: &mut Value, from: &str, to: &str) {
    match v {
        Value::String(s) => *s = s.replace(from, to),
        Value::Array(a) => a.iter_mut().for_each(|x| substitute(x, from, to)),
        Value::Object(o) => o.values_mut().for_each(|x| substitute(x, from, to)),
        _ => {}
    }
}

impl ScriptCall {
    /// The calls this entry stands for (`times` copies, or itself).
    fn expand(self) -> Vec<Self> {
        let Some(n) = self.times else {
            return vec![self];
        };
        (0..n)
            .map(|k| {
                let i = k.to_string();
                let mut c = self.clone();
                c.times = None;
                c.id = c.id.replace("{i}", &i);
                substitute(&mut c.args, "{i}", &i);
                c
            })
            .collect()
    }
}

/// One scripted model response.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptStep {
    /// Assistant text.
    #[serde(default)]
    pub text: Option<String>,
    /// Tool calls.
    #[serde(default)]
    pub tool_calls: Vec<ScriptCall>,
    /// Never answers (to test cancellation).
    #[serde(default)]
    pub hang: bool,
    /// The provider call fails with this message.
    #[serde(default)]
    pub error: Option<String>,
    /// Reported usage (default 10 prompt / 5 completion).
    #[serde(default)]
    pub usage: Option<UsageSpec>,
}

/// What a reaction waits for.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The run started.
    RunStarted,
    /// A tool call needs approval.
    ApprovalNeeded,
    /// A tool call was requested.
    ToolCallRequested,
    /// A tool call finished.
    ToolCallCompleted,
}

/// What the simulated user does.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    /// Approve once.
    Approve,
    /// Approve for the rest of the chat.
    ApproveChat,
    /// Approve for the rest of the task (one tool, this run).
    ApproveTask,
    /// Approve always.
    ApproveAlways,
    /// Deny.
    Deny,
    /// Send a steering message.
    Steer,
    /// Cancel the run.
    Cancel,
}

/// A reaction of the simulated user.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reaction {
    /// Trigger event.
    pub on: Trigger,
    /// Only for this tool.
    #[serde(default)]
    pub tool: Option<String>,
    /// Action.
    #[serde(rename = "do")]
    pub act: Act,
    /// Text for `steer`, reason for `deny`.
    #[serde(default)]
    pub text: Option<String>,
    /// Fire at most once.
    #[serde(default)]
    pub once: bool,
    /// Fire only on the 1-based N-th matching event.
    #[serde(default)]
    pub nth: Option<usize>,
    /// Wait this many milliseconds before acting.
    #[serde(default)]
    pub delay_ms: u64,
}

/// An integer expectation: exact or a range.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(untagged)]
pub enum IntMatch {
    /// Exactly this.
    Exact(u64),
    /// Within bounds (either optional).
    Range {
        /// Lower bound.
        min: Option<u64>,
        /// Upper bound.
        max: Option<u64>,
    },
}

impl IntMatch {
    /// Does `v` satisfy the expectation?
    #[must_use]
    pub fn check(self, v: u64) -> bool {
        match self {
            Self::Exact(n) => v == n,
            Self::Range { min, max } => min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m),
        }
    }
}

/// Text assertions.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextMatch {
    /// Must contain each of these (case-sensitive).
    #[serde(default)]
    pub contains: Vec<String>,
    /// Must contain none of these.
    #[serde(default)]
    pub not_contains: Vec<String>,
    /// Must match this regular expression.
    #[serde(default)]
    pub regex: Option<String>,
    /// Must equal this.
    #[serde(default)]
    pub equals: Option<String>,
}

/// An expected permission decision for one call (in call order).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPermission {
    /// Tool name.
    pub tool: String,
    /// `allowed`, `approved`, `denied_user` or `denied_policy`.
    pub decision: String,
}

/// Assertions about the n-th request sent to the model.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestExpect {
    /// Zero-based request index.
    pub request: usize,
    /// Roles of the messages, in order.
    #[serde(default)]
    pub roles: Option<Vec<String>>,
    /// Each string must appear in some tool message.
    #[serde(default)]
    pub tool_messages_contain: Vec<String>,
    /// No tool message may contain these.
    #[serde(default)]
    pub tool_messages_not_contain: Vec<String>,
    /// No tool message may be longer than this many bytes.
    #[serde(default)]
    pub max_tool_message_bytes: Option<usize>,
    /// The last user message contains this.
    #[serde(default)]
    pub last_user_contains: Option<String>,
    /// Tool names (as the model sees them) that must be offered.
    #[serde(default)]
    pub tools_offered: Vec<String>,
    /// Tool names that must not be offered.
    #[serde(default)]
    pub tools_not_offered: Vec<String>,
}

/// Assertion about an `ApprovalNeeded` card emitted for a tool call.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCardExpect {
    /// Tool name that must have emitted an `ApprovalNeeded` card.
    pub tool: String,
    /// Expected `details.kind` (for example, `"egress"`).
    #[serde(default)]
    pub kind: Option<String>,
    /// Expected `details.why` (`"egress"`, `"executes"`, `"floor"`, `"always_ask"`, `"policy"`, `"browser_address"`).
    #[serde(default)]
    pub why: Option<String>,
    /// Substring expected in `details.host`.
    #[serde(default)]
    pub host_contains: Option<String>,
    /// Substrings expected in `details.payload`.
    #[serde(default)]
    pub payload_contains: Vec<String>,
    /// Substrings that must not appear in `details.payload`.
    #[serde(default)]
    pub payload_not_contains: Vec<String>,
    /// Substrings expected in `details.reason`.
    #[serde(default)]
    pub reason_contains: Vec<String>,
    /// Substrings excluded from the summary and all structured card details.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_not_contains: Vec<String>,
}

/// All assertions of a scenario; absent fields are not checked.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Final run state (`succeeded`, `failed`, `cancelled`).
    #[serde(default)]
    pub state: Option<String>,
    /// Number of model calls.
    #[serde(default)]
    pub model_calls: Option<IntMatch>,
    /// Requested tool-call sequence (names).
    #[serde(default)]
    pub tool_calls: Option<Vec<String>>,
    /// Executions per fake tool (exact, or a `{min, max}` range).
    #[serde(default)]
    pub tool_runs: BTreeMap<String, IntMatch>,
    /// Executions per fake tool that ran to the end (not cut short by a cancel); checked
    /// after `settle_ms`.
    #[serde(default)]
    pub tool_finished: BTreeMap<String, IntMatch>,
    /// Permission decisions, in call order.
    #[serde(default)]
    pub permissions: Option<Vec<ExpectedPermission>>,
    /// Final assistant text.
    #[serde(default)]
    pub final_text: Option<TextMatch>,
    /// Run error text (failed runs).
    #[serde(default)]
    pub error: Option<TextMatch>,
    /// Per-request assertions.
    #[serde(default)]
    pub requests: Vec<RequestExpect>,
    /// Workspace paths (relative) that must not exist after the run (`fs` scenarios).
    #[serde(default)]
    pub files_absent: Vec<String>,
    /// Workspace files (relative) that must still hold exactly this text after the run.
    #[serde(default)]
    pub files_unchanged: BTreeMap<String, String>,
    /// Expected `ApprovalNeeded` card payloads (checked in per-tool call order).
    #[serde(default)]
    pub approval_cards: Vec<ApprovalCardExpect>,
    /// Tools that must NOT emit an `ApprovalNeeded` card during the turn.
    #[serde(default)]
    pub no_approval_tools: Vec<String>,
}

impl Scenario {
    /// A copy with `{ws}` replaced by `ws` in the places that need an absolute workspace path:
    /// `user`, `profile_toml`, scripted text and tool-call args, tool outputs, and the
    /// `final_text` / `error` / per-request message expectations. Not substituted: `fs` (its
    /// paths are relative to the workspace by design), `reactions`, `tools[].error`, `secrets`
    /// and `expect.files_unchanged`.
    #[must_use]
    pub fn with_workspace(&self, ws: &str) -> Self {
        self.substitute_placeholder("{ws}", ws)
    }

    /// A copy with `{data_dir}` replaced by the run's isolated data directory path.
    #[must_use]
    pub fn with_data_dir(&self, data_dir: &str) -> Self {
        self.substitute_placeholder("{data_dir}", data_dir)
    }

    fn substitute_placeholder(&self, placeholder: &str, replacement: &str) -> Self {
        let r = |s: &mut String| *s = s.replace(placeholder, replacement);
        let mut c = self.clone();
        r(&mut c.user);
        r(&mut c.profile_toml);
        for step in &mut c.script {
            step.text.iter_mut().for_each(r);
            for call in &mut step.tool_calls {
                substitute(&mut call.args, placeholder, replacement);
                call.raw_args.iter_mut().for_each(r);
            }
        }
        for tool in &mut c.tools {
            r(&mut tool.output);
        }
        let text_match = |m: &mut TextMatch| {
            m.contains.iter_mut().for_each(r);
            m.not_contains.iter_mut().for_each(r);
        };
        c.expect.final_text.iter_mut().for_each(text_match);
        c.expect.error.iter_mut().for_each(text_match);
        for req in &mut c.expect.requests {
            req.tool_messages_contain.iter_mut().for_each(r);
            req.tool_messages_not_contain.iter_mut().for_each(r);
        }
        for card in &mut c.expect.approval_cards {
            card.payload_contains.iter_mut().for_each(r);
            card.payload_not_contains.iter_mut().for_each(r);
            card.reason_contains.iter_mut().for_each(r);
        }
        c
    }

    /// Parse a scenario from YAML text.
    ///
    /// # Errors
    /// Malformed YAML or unknown fields.
    pub fn from_yaml(s: &str) -> Result<Self> {
        let mut sc: Self = serde_yaml::from_str(s)?;
        if sc.transcript.is_some() && !sc.script.is_empty() {
            return Err("scenario must use either script or transcript".into());
        }
        if let Some(t) = &sc.transcript {
            t.validate()?;
        }
        for step in &mut sc.script {
            step.tool_calls = step
                .tool_calls
                .drain(..)
                .flat_map(ScriptCall::expand)
                .collect();
        }
        Ok(sc)
    }
}

/// Load every `*.yaml` / `*.yml` scenario in `dir` (sorted by file name).
///
/// # Errors
/// I/O or parse errors (the file name is included).
pub fn load_dir(dir: &Path) -> Result<Vec<Scenario>> {
    let mut out = Vec::new();
    for path in yaml_files(dir)? {
        let text = std::fs::read_to_string(&path)?;
        let s = Scenario::from_yaml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(s);
    }
    Ok(out)
}

/// Sorted YAML files of a directory (empty if the directory is missing).
pub(crate) fn yaml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "yaml" || e == "yml")
        })
        .collect();
    files.sort();
    Ok(files)
}

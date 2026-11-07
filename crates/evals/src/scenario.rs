//! Tier-1 scenario format (YAML) and loader.
//!
//! ```yaml
//! name: approval-ask-approve
//! description: ...
//! policy: approve_destructive     # full | approve_destructive | read_only | read_and_draft
//! profile_toml: ""                # extra bot profile TOML ([permissions], [tools] ...)
//! guards: { max_steps: 5 }        # optional overrides
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

use serde::Deserialize;
use serde_json::Value;

use crate::Result;

/// A tier-1 scenario.
#[derive(Debug, Clone, Deserialize)]
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
    /// Guard overrides.
    #[serde(default)]
    pub guards: GuardOverrides,
    /// Fake tools registered for the run.
    #[serde(default)]
    pub tools: Vec<FakeToolSpec>,
    /// The user message that starts the turn.
    pub user: String,
    /// Scripted model responses, consumed one per model call.
    pub script: Vec<ScriptStep>,
    /// Reactions of the simulated user.
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// Assertions.
    #[serde(default)]
    pub expect: Expect,
    /// Wall-clock cap in milliseconds (default 20 000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Global policy preset.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Default, Deserialize)]
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
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectSpec {
    /// Read-only.
    Read,
    /// Reversible write.
    Write,
    /// Destructive.
    Destructive,
}

/// Permission category of a fake tool.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
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
}

/// A fake tool backend.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeToolSpec {
    /// Tool name (a `.` is exposed to the model as `__`).
    pub name: String,
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
}

/// Token usage reported with a scripted reply.
#[derive(Debug, Clone, Copy, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
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
}

/// One scripted model response.
#[derive(Debug, Clone, Default, Deserialize)]
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
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    /// Approve once.
    Approve,
    /// Approve for the rest of the chat.
    ApproveChat,
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
#[derive(Debug, Clone, Deserialize)]
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
    /// Wait this many milliseconds before acting.
    #[serde(default)]
    pub delay_ms: u64,
}

/// An integer expectation: exact or a range.
#[derive(Debug, Clone, Copy, Deserialize)]
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
#[derive(Debug, Clone, Default, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPermission {
    /// Tool name.
    pub tool: String,
    /// `allowed`, `approved`, `denied_user` or `denied_policy`.
    pub decision: String,
}

/// Assertions about the n-th request sent to the model.
#[derive(Debug, Clone, Default, Deserialize)]
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

/// All assertions of a scenario; absent fields are not checked.
#[derive(Debug, Clone, Default, Deserialize)]
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
    /// Executions per fake tool.
    #[serde(default)]
    pub tool_runs: BTreeMap<String, u64>,
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
}

impl Scenario {
    /// Parse a scenario from YAML text.
    ///
    /// # Errors
    /// Malformed YAML or unknown fields.
    pub fn from_yaml(s: &str) -> Result<Self> {
        Ok(serde_yaml::from_str(s)?)
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

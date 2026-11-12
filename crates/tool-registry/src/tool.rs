//! The `Tool` trait and associated output types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{error::ToolError, spec::ToolSpec};

/// A reference to a spilled (large) tool output written to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpillRef {
    /// Absolute path to the spill file on the node running the tool.
    pub path: String,
    /// MIME type of the spill file contents.
    pub mime_type: String,
    /// Byte size of the spill file.
    pub size_bytes: u64,
}

/// A single content part in a tool's output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text output.
    Text { text: String },
    /// Structured JSON data.
    Json { data: Value },
    /// A reference to binary data (e.g. an image).
    Binary { mime_type: String, base64: String },
}

/// The result of a successful tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The content parts returned by the tool.
    ///
    /// For most tools this contains a single text or JSON part.
    pub content: Vec<ContentPart>,
    /// If the full output exceeded the in-memory budget, the remainder was
    /// written to a temporary file and a pointer is returned here.
    pub spill: Option<SpillRef>,
}

impl ToolOutput {
    /// Create a simple text output with no spill.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text { text: text.into() }],
            spill: None,
        }
    }
}

/// Context supplied to every tool call.
#[derive(Debug)]
pub struct ToolContext {
    /// The run that invoked the tool.
    pub run_id: String,
    /// Maximum number of bytes of output to retain in memory before spilling.
    pub max_inline_bytes: usize,
    /// Bot that invoked the tool. `None` when the caller is not a bot.
    ///
    /// MCP servers with a `bots` allowlist refuse a call whose id is missing
    /// or not listed. `None` here is not "every bot".
    pub bot_id: Option<String>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            max_inline_bytes: 1024 * 1024, // 1 MiB
            bot_id: None,
        }
    }
}

/// A callable tool.
///
/// **Delegation rule:** a `Tool` that forwards calls to another tool (a
/// wrapper, bridge or dispatcher) must also forward [`Tool::always_ask`],
/// [`Tool::approval_details`] and [`Tool::approval_closed`] to it, otherwise
/// the wrapped tool's confirmation guarantees are silently lost. A repository
/// test (`delegating_tools_forward_the_approval_hooks`) fails for a new
/// delegating implementation that does not mention them.
///
/// Implementors describe themselves via [`Tool::spec`] and execute via
/// [`Tool::call`].  All implementations must be `Send + Sync` so they can be
/// shared across async tasks.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Return the static specification for this tool.
    fn spec(&self) -> &ToolSpec;

    /// Whether the bot `bot_id` (`None` = no bot) may see this tool in
    /// discovery and in the model's tool list.  Default: every bot.  This is
    /// a visibility filter only; a tool that restricts callers must still
    /// enforce that in [`Tool::call`].
    fn visible_to(&self, _bot_id: Option<&str>) -> bool {
        true
    }

    /// Execute the tool with the given `input` and return its output.
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError>;

    /// Structured facts an approval card should show for this exact call
    /// (computed before the user is asked; carried on the `approval_needed`
    /// event). `Ok(None)` (the default) means the generic card is enough.
    ///
    /// # Errors
    /// The call is invalid and must not be offered for approval: the error
    /// goes straight back to the model.
    async fn approval_details(
        &self,
        _ctx: &ToolContext,
        _input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        Ok(None)
    }

    /// Called when an approval that [`Tool::approval_details`] prepared ends
    /// without the call running (denied, cancelled, timed out, or rejected
    /// before it was shown), so the tool can drop what it froze for it.
    fn approval_closed(&self, _ctx: &ToolContext, _input: &Value) {}

    /// `true` when the call must always be confirmed by the user: a policy
    /// that would allow it is raised to "ask", and neither "always allow"
    /// grants (tool or category) nor "this chat" grants waive the ask. Only an
    /// outright deny is stricter. For tools that delegate future authority.
    fn always_ask(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod delegation_guard {
    use std::path::{Path, PathBuf};

    /// Files whose tools dispatch to other tools but are known to keep the
    /// confirmation guarantees, and why. A new delegating `Tool` is not on this
    /// list: it must forward `always_ask` / `approval_details` /
    /// `approval_closed` (and mention them), or be added here with a reason.
    const REVIEWED: &[(&str, &str)] = &[
        (
            "agent-router/src/surface.rs",
            "find_tool / use_tool: the loop resolves `use_tool` to its target by name and runs decide + approval_details against the TARGET tool",
        ),
        (
            "builtin-tools/src/script.rs",
            "script_run: every inner call goes through the loop's ToolGate (decide + approval_details)",
        ),
        (
            "workflow-replay/src/tool.rs",
            "workflow_run: every step goes through the loop's ToolGate (decide + approval_details)",
        ),
        (
            "tool-registry/src/registry.rs",
            "the registry's own test tools",
        ),
    ];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            if p.is_dir() {
                if name != "target" && name != "node_modules" && name != ".git" {
                    rust_files(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    fn is_delegating_field(line: &str) -> bool {
        let t = line.trim();
        let Some((name, ty)) = t.split_once(':') else {
            return false;
        };
        let name = name
            .trim()
            .trim_start_matches("pub(crate) ")
            .trim_start_matches("pub ");
        let ty = ty.trim().trim_end_matches(',');
        name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            && !name.is_empty()
            && matches!(
                ty,
                "Arc<dyn Tool>"
                    | "ToolRegistry"
                    | "Arc<ToolRegistry>"
                    | "Option<ToolRegistry>"
                    | "Arc<dyn ToolGate>"
            )
    }

    #[test]
    fn delegating_tools_forward_the_approval_hooks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        rust_files(&root.join("crates"), &mut files);
        rust_files(&root.join("apps"), &mut files);
        let mut offenders = Vec::new();
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            if !src.contains("impl Tool for") || !src.lines().any(is_delegating_field) {
                continue;
            }
            let rel = f
                .strip_prefix(root.join("crates"))
                .or_else(|_| f.strip_prefix(root.join("apps")))
                .unwrap_or(&f)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let forwards = src.contains("fn always_ask") && src.contains("fn approval_details");
            if !forwards && !REVIEWED.iter().any(|(p, _)| rel.ends_with(p)) {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "these files implement Tool and hold another tool, registry or gate but do not forward always_ask / approval_details (see the delegation rule on `Tool`): {offenders:?}"
        );
    }

    #[test]
    fn the_detector_sees_a_delegating_field() {
        assert!(is_delegating_field("    inner: Arc<dyn Tool>,"));
        assert!(is_delegating_field("    pub(crate) tools: ToolRegistry,"));
        assert!(!is_delegating_field("    name: String,"));
    }
}

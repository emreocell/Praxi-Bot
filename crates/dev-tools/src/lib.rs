//! Developer tools for the agent: `git_status`, `git_diff`, `git_log`,
//! `git_branch`, `git_commit`, `git_show` (all via the `git` CLI, never a
//! shell) and a read-only `process_list`.
//!
//! Entry points: [`tools`] builds the tool set, [`register`] puts it into a
//! [`tool_registry::ToolRegistry`].
//!
//! Safety model:
//! - every repository path must resolve (symlinks resolved) inside one of the
//!   configured [`DevToolsConfig::allowed_roots`], and so must the repository's
//!   top level as reported by git;
//! - `git` is spawned directly with an argument vector. Values that become
//!   paths or revisions never start with `-`, paths are passed after `--` and
//!   with `--literal-pathspecs`, absolute and `..` paths are rejected;
//! - repository-configured code execution is switched off: no hooks
//!   (`core.hooksPath` points nowhere, `commit --no-verify`), no
//!   `core.fsmonitor`, no external diff drivers or textconv filters, every
//!   configured `filter.<name>` clean/smudge/process driver is blanked, and
//!   commit/tag signing and `log.showSignature` are forced off;
//! - the repository's work tree, git dir and common dir must all be inside the
//!   allowed roots (a `.git` file or `core.worktree` cannot point outside);
//! - output is capped (`max_bytes`, and the caller's `ToolContext::max_inline_bytes`);
//! - every `git` process has a timeout and is killed on drop.
//!
//! Permission mapping: git tools use `PermissionCategory::Fs` (`Read` for
//! status/diff/log/show; `Write` for commit and branch, because one tool has
//! one effect), `process_list` uses `PermissionCategory::System` with `Read`.

mod git;
mod git_tools;
mod guard;
mod process;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tool_registry::{Tool, ToolRegistry};

pub use process::PROCESS_LIST_TOOL_NAME;

/// Names of the git tools, in registration order.
pub const GIT_TOOL_NAMES: [&str; 6] = [
    "git_status",
    "git_diff",
    "git_log",
    "git_branch",
    "git_commit",
    "git_show",
];

/// Configuration for the dev tools.
#[derive(Debug, Clone)]
pub struct DevToolsConfig {
    /// Directories the git tools may operate in. Empty = every git call is refused.
    pub allowed_roots: Vec<PathBuf>,
    /// `git` executable (default `git`, resolved via `PATH`).
    pub git_binary: String,
    /// Per-process time limit for `git` calls.
    pub git_timeout: Duration,
}

impl DevToolsConfig {
    /// Config allowing the given workspace roots, with defaults otherwise.
    #[must_use]
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self {
            allowed_roots,
            git_binary: "git".to_string(),
            git_timeout: Duration::from_secs(30),
        }
    }
}

/// Build every dev tool without registering it.
#[must_use]
pub fn tools(cfg: &DevToolsConfig) -> Vec<Arc<dyn Tool>> {
    let mut all = git_tools::tools(cfg);
    all.push(process::tool());
    all
}

/// Register the dev tools in `registry`. Returns the number newly registered.
#[must_use]
pub fn register(registry: &ToolRegistry, cfg: &DevToolsConfig) -> usize {
    tools(cfg)
        .into_iter()
        .filter(|t| registry.register(t.clone()))
        .count()
}

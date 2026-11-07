//! Tier-2 task format (YAML) and loader.
//!
//! ```yaml
//! name: browser-example-heading
//! description: Open example.com and read the heading.
//! requires: browser              # browser | desktop | none
//! platform: any                  # any | windows
//! prompt: "Open https://example.com and tell me the page heading."
//! max_steps: 12
//! timeout_s: 120
//! setup:                         # steps before the turn
//!   - write_file: { path: "{{workdir}}/in.txt", content: "hello" }
//! checks:                        # all must pass
//!   - final_text: { contains: ["Example Domain"] }
//!   - browser_text_contains: "Example Domain"
//!   - judge: { rubric: "The assistant read the heading ..." }
//! cleanup:                       # always runs, even after failures
//!   - remove_path: "{{workdir}}/in.txt"
//! fake_script:                   # replies used with `--fake` (structure runs)
//!   - tool_calls: [{ id: c1, tool: browser_open, args: { url: "https://example.com" } }]
//!   - text: "Example Domain"
//! ```
//!
//! `{{workdir}}` expands to a fresh per-task directory that is deleted afterwards
//! (`{{workdir_name}}` is its last path component); both also expand inside
//! `fake_script` arguments.

use std::path::Path;

use serde::Deserialize;

use crate::Result;
use crate::scenario::{ScriptStep, TextMatch, yaml_files};

/// A tier-2 task.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Unique name (also the metrics key).
    pub name: String,
    /// What the task exercises.
    #[serde(default)]
    pub description: String,
    /// The surface the task needs.
    pub requires: Surface,
    /// Platforms the task runs on.
    #[serde(default)]
    pub platform: PlatformReq,
    /// The user message.
    pub prompt: String,
    /// Tool-name globs the bot may use (default depends on `requires`).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Model-call cap.
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Wall-clock cap in seconds (default 180).
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// Setup steps.
    #[serde(default, with = "serde_yaml::with::singleton_map_recursive")]
    pub setup: Vec<Step>,
    /// Checks; the task passes when all of them pass.
    #[serde(with = "serde_yaml::with::singleton_map_recursive")]
    pub checks: Vec<Check>,
    /// Cleanup steps; run even when setup or the turn failed.
    #[serde(default, with = "serde_yaml::with::singleton_map_recursive")]
    pub cleanup: Vec<Step>,
    /// Scripted replies for `--fake` runs.
    #[serde(default)]
    pub fake_script: Vec<ScriptStep>,
}

/// The surface a task drives.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// A browser (Chrome / Edge over CDP).
    Browser,
    /// The local desktop.
    Desktop,
    /// Neither (files only).
    None,
}

/// Platform requirement.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlatformReq {
    /// Any platform.
    #[default]
    Any,
    /// Windows only.
    Windows,
}

impl PlatformReq {
    /// Does this platform requirement hold here?
    #[must_use]
    pub fn holds_here(self) -> bool {
        match self {
            Self::Any => true,
            Self::Windows => cfg!(windows),
        }
    }
}

/// A file to write during setup.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFile {
    /// Destination (`{{workdir}}` is expanded).
    pub path: String,
    /// Content.
    pub content: String,
}

/// A program to launch during setup.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// Executable.
    pub executable: String,
    /// Arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// A setup / cleanup step.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Write a file (parents are created).
    WriteFile(WriteFile),
    /// Create a directory.
    Mkdir(String),
    /// Delete a file or directory tree (missing is fine).
    RemovePath(String),
    /// Open a URL in the task's browser.
    OpenUrl(String),
    /// Launch a program through the desktop backend.
    Launch(Launch),
    /// Run a PowerShell snippet (Windows).
    Powershell(String),
    /// Kill processes with these image names that did not exist before the
    /// task started (never touches the user's own instances).
    KillNew(Vec<String>),
}

/// Desktop node check.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopNode {
    /// Look in the window whose title contains this (first match); `a|b` = any alternative.
    pub window_title_contains: String,
    /// A node whose name or value contains this (`a|b` = any alternative).
    pub name_contains: String,
    /// Required role (exact).
    #[serde(default)]
    pub role: Option<String>,
}

/// LLM judge check.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeSpec {
    /// The rubric the judge applies.
    pub rubric: String,
    /// Attach the final screenshot (default true).
    #[serde(default = "yes")]
    pub screenshot: bool,
}

fn yes() -> bool {
    true
}

/// A check.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// File exists.
    FileExists(String),
    /// Directory exists.
    DirExists(String),
    /// File contains text.
    FileContains {
        /// File path.
        path: String,
        /// Text.
        text: String,
    },
    /// Final assistant text assertions.
    FinalText(TextMatch),
    /// Current browser URL contains text.
    BrowserUrlContains(String),
    /// Current page text contains text.
    BrowserTextContains(String),
    /// A desktop window with this title substring exists.
    DesktopWindow(String),
    /// A desktop accessibility node exists.
    DesktopNode(DesktopNode),
    /// LLM judge.
    Judge(JudgeSpec),
}

impl Task {
    /// Parse a task from YAML.
    ///
    /// # Errors
    /// Malformed YAML or unknown fields.
    pub fn from_yaml(s: &str) -> Result<Self> {
        Ok(serde_yaml::from_str(s)?)
    }

    /// Tool globs the bot is limited to.
    #[must_use]
    pub fn tool_allow(&self) -> Vec<String> {
        if !self.tools.is_empty() {
            return self.tools.clone();
        }
        let mut v: Vec<String> = match self.requires {
            Surface::Browser => vec!["browser_*".into()],
            Surface::Desktop => vec!["desktop_*".into()],
            Surface::None => vec!["fs_*".into()],
        };
        v.push("plan.todo_write".into());
        v
    }

    /// Names of the image names listed in any `kill_new` step.
    #[must_use]
    pub fn kill_new_names(&self) -> Vec<String> {
        self.cleanup
            .iter()
            .chain(&self.setup)
            .filter_map(|s| match s {
                Step::KillNew(v) => Some(v.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }
}

/// Load every task of a directory (sorted by file name).
///
/// # Errors
/// I/O or parse errors (the file name is included).
pub fn load_dir(dir: &Path) -> Result<Vec<Task>> {
    let mut out = Vec::new();
    for path in yaml_files(dir)? {
        let text = std::fs::read_to_string(&path)?;
        let t = Task::from_yaml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(t);
    }
    Ok(out)
}

//! Engine modes and approval levels for coding CLIs that run with their own tools.
//!
//! A bot either keeps Praxi in charge of files and shell (`praxi_governed`, the
//! confined engine) or hands them to the CLI (`native_tools`). In the second
//! mode the bot's approval level is mapped onto the CLI's own permission mode.
//! Nothing here spawns a process; [`level_plan`] is the one table the runtime,
//! the API and the UI read.

use serde::{Deserialize, Serialize};

use crate::acp_data::{self, DataId};

/// Who runs the bot's file and shell actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineMode {
    /// Praxi's own tools and permission engine; the CLI's native tools are off.
    #[default]
    PraxiGoverned,
    /// The CLI's native tools are on and governed by the CLI's permission mode.
    NativeTools,
}

/// Per-bot approval level. The default is `Ask`; `Full` is an explicit owner choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalLevel {
    /// The CLI asks before it changes anything; each request is a Praxi approval card.
    #[default]
    Ask,
    /// File edits inside the workspace are accepted, everything else asks.
    AcceptEdits,
    /// The CLI's own automatic reviewer decides (only where the CLI has one).
    Auto,
    /// No approval prompts: the CLI runs in its most permissive mode.
    Full,
}

impl ApprovalLevel {
    pub const ALL: [Self; 4] = [Self::Ask, Self::AcceptEdits, Self::Auto, Self::Full];

    /// Width of the grant: a higher rank trusts the CLI with more.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Ask => 0,
            Self::AcceptEdits => 1,
            Self::Auto => 2,
            Self::Full => 3,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AcceptEdits => "accept_edits",
            Self::Auto => "auto",
            Self::Full => "full",
        }
    }
}

impl std::str::FromStr for ApprovalLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_str() == s)
            .ok_or_else(|| format!("unknown approval level {s:?}"))
    }
}

impl EngineMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PraxiGoverned => "praxi_governed",
            Self::NativeTools => "native_tools",
        }
    }
}

impl std::str::FromStr for EngineMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "praxi_governed" => Ok(Self::PraxiGoverned),
            "native_tools" => Ok(Self::NativeTools),
            _ => Err(format!("unknown engine mode {s:?}")),
        }
    }
}

/// Drivers for native-tools mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Driver {
    Claude,
    Codex,
    Cursor,
    OpenCode,
    Gemini,
    /// Antigravity CLI (`agy acp`).
    Agy,
    /// Qwen Code (`qwen --acp`).
    Qwen,
    /// Kiro CLI (`kiro-cli acp`).
    Kiro,
    /// GitHub Copilot CLI (`copilot --acp`).
    Copilot,
    /// Kimi Code CLI (`kimi acp`).
    Kimi,
    /// Qoder CLI (`qodercli --acp`).
    Qoder,
    /// Mistral Vibe (`vibe-acp`).
    Vibe,
    /// Goose (`goose acp`).
    Goose,
    /// An ACP engine defined by a `providers/cli/<id>.toml` file instead of Rust code.
    Data(DataId),
}

/// How far a driver's interface was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Flags and subcommands confirmed in the installed CLI's `--help`.
    Verified,
    /// Interface taken from the vendor reference only (CLI not installed here).
    /// Shown as experimental in the UI.
    Experimental,
}

/// Wire protocol the driver speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `claude -p` with stream-json on both sides.
    StreamJson,
    /// `codex app-server` JSON-RPC.
    CodexAppServer,
    /// Agent Client Protocol over stdio.
    Acp,
}

impl Driver {
    /// The drivers written in Rust. Private on purpose: it leaves out the data-driven
    /// engines, so a loop over it would skip them (a folder rule, a consent check, a level
    /// check) and treat them as unchecked. Everything outside this file uses [`Driver::all`].
    const BUILT_IN: [Self; 13] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::OpenCode,
        Self::Gemini,
        Self::Agy,
        Self::Qwen,
        Self::Copilot,
        Self::Kiro,
        Self::Kimi,
        Self::Qoder,
        Self::Vibe,
        Self::Goose,
    ];

    /// Every driver: the Rust ones, then the data-driven ACP engines.
    #[must_use]
    pub fn all() -> Vec<Self> {
        Self::BUILT_IN
            .into_iter()
            .chain(acp_data::ids().map(Self::Data))
            .collect()
    }

    /// Registry id (`claude`, `codex`, `cursor`, `opencode`, `gemini`, `agy`, `qwen`, `copilot`, `kiro`, `kimi`, `qoder`, `vibe`, `goose`, or a data-driven id).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Data(data) => data.engine().spec.id,
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Agy => "agy",
            Self::Qwen => "qwen",
            Self::Kiro => "kiro",
            Self::Copilot => "copilot",
            Self::Kimi => "kimi",
            Self::Qoder => "qoder",
            Self::Vibe => "vibe",
            Self::Goose => "goose",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::BUILT_IN
            .into_iter()
            .find(|d| d.id() == id)
            .or_else(|| acp_data::data_id(id).map(Self::Data))
    }

    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Data(data) => data.engine().spec.display_name,
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini CLI",
            Self::Agy => "Antigravity",
            Self::Qwen => "Qwen Code",
            Self::Kiro => "Kiro CLI",
            Self::Copilot => "GitHub Copilot CLI",
            Self::Kimi => "Kimi Code CLI",
            Self::Qoder => "Qoder CLI",
            Self::Vibe => "Mistral Vibe",
            Self::Goose => "Goose",
        }
    }

    #[must_use]
    pub fn transport(self) -> Transport {
        match self {
            Self::Claude => Transport::StreamJson,
            Self::Codex => Transport::CodexAppServer,
            Self::Cursor
            | Self::OpenCode
            | Self::Gemini
            | Self::Agy
            | Self::Qwen
            | Self::Copilot
            | Self::Kiro
            | Self::Kimi
            | Self::Qoder
            | Self::Vibe
            | Self::Goose
            | Self::Data(_) => Transport::Acp,
        }
    }

    /// Gemini CLI is not installed on the machine this was written on and its
    /// install command is unverified, so its driver ships as experimental.
    #[must_use]
    pub fn support(self) -> Support {
        match self {
            Self::Gemini
            | Self::OpenCode
            | Self::Agy
            | Self::Qwen
            | Self::Copilot
            | Self::Kiro
            | Self::Kimi
            | Self::Qoder
            | Self::Vibe
            | Self::Goose
            | Self::Data(_) => Support::Experimental,
            _ => Support::Verified,
        }
    }
}

/// Codex thread settings for a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexPolicy {
    pub approval_policy: &'static str,
    pub reviewer: &'static str,
    /// `thread/start` `sandbox` value.
    pub sandbox: &'static str,
    /// `turn/start` `sandboxPolicy.type` value.
    pub sandbox_policy: &'static str,
}

/// What a bot's level means for one driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelPlan {
    pub requested: ApprovalLevel,
    /// The level actually applied: never wider than requested. A level the CLI
    /// has no mode for runs as `Ask`.
    pub applied: ApprovalLevel,
    pub offered: bool,
    /// The CLI's own setting, as shown to the owner.
    pub native: &'static str,
    /// Claude `--permission-mode` value (`default` is renamed `manual` by newer CLIs).
    pub claude_mode: Option<&'static str>,
    pub codex: Option<CodexPolicy>,
    /// Global flags placed before the ACP subcommand (or after `--acp`).
    pub flags: &'static [&'static str],
    /// ACP: accept every forwarded permission request without a card.
    pub acp_allow_all: bool,
    /// ACP `session/set_mode` id sent after `session/new` (Antigravity).
    pub acp_mode: Option<&'static str>,
    /// `false` when Praxi cannot make the CLI honour `applied` (the turn is refused).
    pub enforced: bool,
}

const fn plan(requested: ApprovalLevel, applied: ApprovalLevel, native: &'static str) -> LevelPlan {
    LevelPlan {
        requested,
        applied,
        offered: requested.rank() == applied.rank(),
        native,
        claude_mode: None,
        codex: None,
        flags: &[],
        acp_allow_all: false,
        acp_mode: None,
        enforced: true,
    }
}

/// The mapping table (the reference project's approval-level provider mappings,
/// adapted: the default is Ask and Full is never implied).
#[must_use]
#[allow(clippy::too_many_lines)] // One table.
pub fn level_plan(driver: Driver, level: ApprovalLevel) -> LevelPlan {
    use ApprovalLevel::{AcceptEdits, Ask, Auto, Full};
    match driver {
        Driver::Data(data) => data.engine().level_plan(level),
        Driver::Claude => {
            let (mode, native) = match level {
                Ask => ("default", "--permission-mode default"),
                AcceptEdits => ("acceptEdits", "--permission-mode acceptEdits"),
                Auto => ("auto", "--permission-mode auto"),
                Full => ("bypassPermissions", "--permission-mode bypassPermissions"),
            };
            LevelPlan {
                claude_mode: Some(mode),
                ..plan(level, level, native)
            }
        }
        Driver::Codex => {
            let (applied, native, codex) = match level {
                Ask | AcceptEdits => (
                    Ask,
                    "on-request approvals, workspace-write sandbox",
                    CodexPolicy {
                        approval_policy: "on-request",
                        reviewer: "user",
                        sandbox: "workspace-write",
                        sandbox_policy: "workspaceWrite",
                    },
                ),
                Auto => (
                    Auto,
                    "workspace-write sandbox, auto_review reviewer",
                    CodexPolicy {
                        approval_policy: "on-request",
                        reviewer: "auto_review",
                        sandbox: "workspace-write",
                        sandbox_policy: "workspaceWrite",
                    },
                ),
                Full => (
                    Full,
                    "approvals never, danger-full-access sandbox",
                    CodexPolicy {
                        approval_policy: "never",
                        reviewer: "user",
                        sandbox: "danger-full-access",
                        sandbox_policy: "dangerFullAccess",
                    },
                ),
            };
            LevelPlan {
                codex: Some(codex),
                ..plan(level, applied, native)
            }
        }
        Driver::Cursor => match level {
            Ask | AcceptEdits => plan(level, Ask, "default"),
            Auto => LevelPlan {
                flags: &["--auto-review"],
                ..plan(level, Auto, "--auto-review")
            },
            Full => LevelPlan {
                flags: &["--force"],
                ..plan(level, Full, "--force")
            },
        },
        Driver::OpenCode => match level {
            // OpenCode's default policy allows edits and shell without asking, and neither
            // `opencode --help` nor the reference shows a per-run way to impose Ask, so no
            // level below Full is offered or started.
            Ask | AcceptEdits | Auto => LevelPlan {
                offered: false,
                enforced: false,
                ..plan(
                    level,
                    Ask,
                    "not enforceable: OpenCode allows edits and commands by default",
                )
            },
            Full => LevelPlan {
                acp_allow_all: true,
                ..plan(level, Full, "permission requests approved automatically")
            },
        },
        Driver::Gemini => match level {
            Ask | Auto => plan(level, Ask, "default"),
            AcceptEdits => LevelPlan {
                flags: &["--approval-mode", "auto_edit"],
                ..plan(level, AcceptEdits, "--approval-mode auto_edit")
            },
            Full => LevelPlan {
                flags: &["--yolo"],
                acp_allow_all: true,
                ..plan(level, Full, "--yolo")
            },
        },
        // Antigravity's ACP modes (`agy acp` session/new): default asks before edits and
        // commands, accept-edits approves workspace edits, auto approves edits and safe
        // commands but asks before irreversible actions. Full skips every prompt.
        Driver::Agy => match level {
            Ask => LevelPlan {
                acp_mode: Some("default"),
                ..plan(level, Ask, "mode default")
            },
            AcceptEdits => LevelPlan {
                acp_mode: Some("accept-edits"),
                ..plan(level, AcceptEdits, "mode accept-edits")
            },
            Auto => LevelPlan {
                acp_mode: Some("auto"),
                ..plan(level, Auto, "mode auto")
            },
            Full => LevelPlan {
                flags: &["--dangerously-skip-permissions"],
                acp_allow_all: true,
                ..plan(level, Full, "--dangerously-skip-permissions")
            },
        },
        // Qwen Code's `--approval-mode` (0.28.4): default asks before edits and commands,
        // auto-edit approves edits, auto lets a classifier approve safe actions and block
        // risky ones, yolo approves everything. The flag wins over `tools.approvalMode` in
        // settings, and with neither the CLI starts in auto, so Ask is always passed.
        Driver::Qwen => {
            let (flags, native): (&'static [&'static str], _) = match level {
                Ask => (&["--approval-mode", "default"], "--approval-mode default"),
                AcceptEdits => (
                    &["--approval-mode", "auto-edit"],
                    "--approval-mode auto-edit",
                ),
                Auto => (&["--approval-mode", "auto"], "--approval-mode auto"),
                Full => (&["--approval-mode", "yolo"], "--approval-mode yolo"),
            };
            LevelPlan {
                flags,
                acp_allow_all: level == Full,
                ..plan(level, level, native)
            }
        }
        // GitHub Copilot CLI (`copilot --help`, `copilot help permissions`, 1.0.115): with no
        // allow rule it asks before edits, commands, paths outside the workspace and URLs, and
        // an ACP session starts with its `allow_all` option `off`. `--allow-tool write` accepts
        // file edits only (shell commands still ask). `--assisted-approval` is a probabilistic
        // judge whose behaviour without allow rules is not documented, so Auto runs as Ask.
        // `--allow-all-tools` / `--yolo` are never passed: at Full, Praxi approves the forwarded
        // requests itself. Every level pins the launched executable (`--no-auto-update`) and
        // keeps file access to the workspace (`--disallow-temp-dir`).
        Driver::Copilot => match level {
            Ask | Auto => LevelPlan {
                flags: &["--no-auto-update", "--disallow-temp-dir"],
                ..plan(level, Ask, "default (asks before edits and commands)")
            },
            AcceptEdits => LevelPlan {
                flags: &[
                    "--no-auto-update",
                    "--disallow-temp-dir",
                    "--allow-tool",
                    "write",
                ],
                ..plan(level, AcceptEdits, "--allow-tool write")
            },
            Full => LevelPlan {
                flags: &["--no-auto-update", "--disallow-temp-dir"],
                acp_allow_all: true,
                ..plan(level, Full, "permission requests approved automatically")
            },
        },
        // Kiro CLI V3 rejects the trust flags on `acp` and has no per-launch mode. Below Full
        // it runs on its own policy: by default reads are allowed and writes and most
        // commands ask, but the user's own Kiro permission rules still apply. Full approves
        // each `session/request_permission`.
        Driver::Kiro => match level {
            Ask | AcceptEdits | Auto => plan(level, Ask, "default permissions"),
            Full => LevelPlan {
                acp_allow_all: true,
                ..plan(level, Full, "every permission request approved")
            },
        },
        // Mistral Vibe 3.1.5 (`vibe-acp`): the ACP modes are the CLI's approval policies,
        // sent with `session/set_mode` after `session/new`, because a new session starts in
        // `default_approval_policy` from `config.toml`. `ask` approves only reads and a
        // read-only command allowlist, `accept-edits` also file edits inside the working
        // folder (outside it and sensitive files still ask), `smart-approve` lets a model
        // classifier run the safe calls and `auto-approve` bypasses every permission.
        // `smart-approve` ships behind a rollout gate and `auto-approve` can be turned off
        // (`enable_auto_approve`); `vibe-acp` answers a mode it does not offer with an
        // empty success, so the run checks the session's mode list first (`native.rs`).
        Driver::Vibe => {
            let (mode, native) = match level {
                Ask => ("ask", "mode ask"),
                AcceptEdits => ("accept-edits", "mode accept-edits"),
                Auto => ("smart-approve", "mode smart-approve"),
                Full => ("auto-approve", "mode auto-approve"),
            };
            LevelPlan {
                acp_mode: Some(mode),
                acp_allow_all: level == Full,
                ..plan(level, level, native)
            }
        }
        // Goose 1.61.1 (`goose acp`): the ACP modes are its `GooseMode`s, `approve` ("Ask
        // before every tool call"), `smart_approve` (read-only calls run, as marked by the
        // MCP server or judged by the session's own model; the rest ask) and `auto`
        // ("Automatically approve tool calls"). A new session starts in `GOOSE_MODE` from the
        // user's config, `auto` by default, so the mode is always sent with
        // `session/set_mode`; it changes that session only. Goose has no mode for file edits
        // alone, so Accept edits runs as Ask. Tools the user saved as `always_allow` in
        // `permission.yaml` still run without asking, in every mode.
        Driver::Goose => {
            let (mode, applied, native) = match level {
                Ask | AcceptEdits => ("approve", Ask, "mode approve"),
                Auto => ("smart_approve", Auto, "mode smart_approve"),
                Full => ("auto", Full, "mode auto"),
            };
            LevelPlan {
                acp_mode: Some(mode),
                acp_allow_all: level == Full,
                ..plan(level, applied, native)
            }
        }
        // Qoder CLI 1.1.97 (`qodercli --acp`, a hidden flag): the ACP mode is sent with
        // `session/set_mode` after `session/new`, because a new session starts in the
        // permission mode of the user's settings. Its ACP modes are `default` ("Prompts for
        // approval"), `acceptEdits` ("Auto-approves edit tools"), `auto` (an approval
        // classifier), `dontAsk` (refuses instead of prompting, unused) and `yolo` ("Auto-
        // approves all tools"). The session's mode is re-read from the CLI after every change,
        // so a mode set here is the one that applies.
        Driver::Qoder => {
            let (mode, native) = match level {
                Ask => ("default", "mode default"),
                AcceptEdits => ("acceptEdits", "mode acceptEdits"),
                Auto => ("auto", "mode auto"),
                Full => ("yolo", "mode yolo"),
            };
            LevelPlan {
                acp_mode: Some(mode),
                acp_allow_all: level == Full,
                ..plan(level, level, native)
            }
        }
        // Kimi Code CLI 2.7.3 (`kimi acp`): the ACP mode is sent with `session/set_mode`
        // after `session/new`, because a new session otherwise starts in the
        // `defaultPermissionMode` of `config.toml`. The ACP mode list describes `auto` as
        // "auto-approve safe operations" and `yolo` as "auto-approve everything", but they
        // map to the CLI's own permission modes the other way round: `yolo` is "Ask When
        // Needed" (routine edits and commands run, dangerous commands, sensitive files and
        // git internals still ask) and `auto` is "Never Ask". So Auto uses `yolo` and Full
        // uses `auto`. `default` ("Always Ask") approves reads, web search and fetch, and on
        // Linux and macOS (`git-cwd-write-approve`) also file edits inside a git work tree,
        // which is wider than Ask: there Ask is refused rather than widened. There is no
        // mode for edits only, so Accept edits runs as Ask.
        Driver::Kimi => match level {
            Ask | AcceptEdits if cfg!(windows) => LevelPlan {
                acp_mode: Some("default"),
                ..plan(level, Ask, "mode default (Always Ask)")
            },
            Ask | AcceptEdits => LevelPlan {
                acp_mode: Some("default"),
                offered: false,
                enforced: false,
                ..plan(
                    level,
                    Ask,
                    "not enforceable: Kimi approves edits inside a git repository on Linux and macOS",
                )
            },
            Auto => LevelPlan {
                acp_mode: Some("yolo"),
                ..plan(level, Auto, "mode yolo (Ask When Needed)")
            },
            Full => LevelPlan {
                acp_mode: Some("auto"),
                acp_allow_all: true,
                ..plan(level, Full, "mode auto (Never Ask)")
            },
        },
    }
}

/// Launch arguments of the ACP drivers (everything after the executable).
/// `None` for drivers that do not speak ACP. The prompt never appears here.
#[must_use]
pub fn acp_args(driver: Driver, plan: &LevelPlan, model: Option<&str>) -> Option<Vec<String>> {
    let own = |s: &[&str]| s.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let mut args = Vec::new();
    match driver {
        Driver::Cursor => {
            // Global flags precede `acp` (cursor-agent --help).
            args.extend(own(plan.flags));
            if let Some(m) = model {
                args.extend(["--model".into(), m.to_owned()]);
            }
            args.push("acp".into());
        }
        // Kimi: `-m` is a top-level option that `kimi acp` does not take; the model is
        // chosen with `session/set_model` after `session/new`. Goose: mode and model are
        // set after `session/new` (`session/set_mode`, the `model` config option).
        Driver::OpenCode | Driver::Kimi | Driver::Goose => args.push("acp".into()),
        // Qoder: the model is chosen with `session/set_model` after `session/new`, like the
        // mode, so a model the CLI does not list is refused instead of guessed.
        Driver::Qoder => args.push("--acp".into()),
        // Vibe: `vibe-acp` takes no options; the mode is set after `session/new`, and no
        // model is sent (Vibe saves a model chosen over ACP as the user's default).
        Driver::Vibe => {}
        Driver::Gemini | Driver::Qwen => {
            args.push("--acp".into());
            args.extend(own(plan.flags));
            if let Some(m) = model {
                args.extend(["-m".into(), m.to_owned()]);
            }
        }
        Driver::Agy => {
            args.push("acp".into());
            args.extend(own(plan.flags));
        }
        // Pinned to V3 so `acp.agentEngine` in the user's settings cannot switch harness.
        Driver::Kiro => args.extend(own(&["acp", "--agent-engine", "v3"])),
        Driver::Copilot => {
            args.push("--acp".into());
            args.extend(own(plan.flags));
            if let Some(m) = model {
                args.extend(["--model".into(), m.to_owned()]);
            }
        }
        Driver::Data(data) => return Some(data.engine().argv(plan)),
        Driver::Claude | Driver::Codex => return None,
    }
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every loop over drivers (folder rules, consent, level checks) goes through
    /// `Driver::all()`. This fails if a data-driven engine is ever left out of it, or if the
    /// Rust-only list becomes reachable again from another module.
    #[test]
    fn all_includes_every_data_driven_engine() {
        let all = Driver::all();
        assert!(!acp_data::engines().is_empty());
        assert_eq!(
            all.len(),
            Driver::BUILT_IN.len() + acp_data::engines().len(),
            "a data-driven engine is missing from Driver::all()"
        );
        for engine in acp_data::engines() {
            let driver = Driver::from_id(engine.spec.id)
                .unwrap_or_else(|| panic!("{} has no driver", engine.spec.id));
            assert!(matches!(driver, Driver::Data(_)), "{}", engine.spec.id);
            assert!(all.contains(&driver), "{} is skipped", engine.spec.id);
        }
        // The consent and level gate holds for them too: never wider than asked, and Full is
        // the only level that auto-allows.
        for driver in all {
            for level in ApprovalLevel::ALL {
                let plan = level_plan(driver, level);
                assert!(plan.applied.rank() <= level.rank(), "{}", driver.id());
                assert!(level == ApprovalLevel::Full || !plan.acp_allow_all);
            }
        }
        // No module outside this file can name the Rust-only list: it stays private.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|n| n != "levels.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    assert!(
                        !text.contains("Driver::BUILT_IN") && !text.contains("Driver::ALL"),
                        "{} loops over a list without the data-driven engines; use Driver::all()",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One table, one test.
    fn level_mapping_per_cli() {
        use ApprovalLevel::{AcceptEdits, Ask, Auto, Full};
        const KIMI_ASK: &str = if cfg!(windows) {
            "mode default (Always Ask)"
        } else {
            "not enforceable: Kimi approves edits inside a git repository on Linux and macOS"
        };
        const KIRO_ARGV: &[&str] = &["acp", "--agent-engine", "v3"];
        // (driver, level) -> (applied level, native setting, ACP argv)
        let cases: &[(Driver, ApprovalLevel, ApprovalLevel, &str, &[&str])] = &[
            (Driver::Claude, Ask, Ask, "--permission-mode default", &[]),
            (
                Driver::Claude,
                AcceptEdits,
                AcceptEdits,
                "--permission-mode acceptEdits",
                &[],
            ),
            (Driver::Claude, Auto, Auto, "--permission-mode auto", &[]),
            (
                Driver::Claude,
                Full,
                Full,
                "--permission-mode bypassPermissions",
                &[],
            ),
            (
                Driver::Codex,
                AcceptEdits,
                Ask,
                "on-request approvals, workspace-write sandbox",
                &[],
            ),
            (
                Driver::Codex,
                Auto,
                Auto,
                "workspace-write sandbox, auto_review reviewer",
                &[],
            ),
            (
                Driver::Codex,
                Full,
                Full,
                "approvals never, danger-full-access sandbox",
                &[],
            ),
            (Driver::Cursor, Ask, Ask, "default", &["acp"]),
            (Driver::Cursor, AcceptEdits, Ask, "default", &["acp"]),
            (
                Driver::Cursor,
                Auto,
                Auto,
                "--auto-review",
                &["--auto-review", "acp"],
            ),
            (Driver::Cursor, Full, Full, "--force", &["--force", "acp"]),
            (
                Driver::OpenCode,
                Auto,
                Ask,
                "not enforceable: OpenCode allows edits and commands by default",
                &["acp"],
            ),
            (
                Driver::OpenCode,
                Full,
                Full,
                "permission requests approved automatically",
                &["acp"],
            ),
            (
                Driver::Gemini,
                AcceptEdits,
                AcceptEdits,
                "--approval-mode auto_edit",
                &["--acp", "--approval-mode", "auto_edit"],
            ),
            (Driver::Gemini, Auto, Ask, "default", &["--acp"]),
            (Driver::Gemini, Full, Full, "--yolo", &["--acp", "--yolo"]),
            (Driver::Agy, Ask, Ask, "mode default", &["acp"]),
            (
                Driver::Agy,
                AcceptEdits,
                AcceptEdits,
                "mode accept-edits",
                &["acp"],
            ),
            (Driver::Agy, Auto, Auto, "mode auto", &["acp"]),
            (
                Driver::Agy,
                Full,
                Full,
                "--dangerously-skip-permissions",
                &["acp", "--dangerously-skip-permissions"],
            ),
            (
                Driver::Qwen,
                Ask,
                Ask,
                "--approval-mode default",
                &["--acp", "--approval-mode", "default"],
            ),
            (
                Driver::Qwen,
                AcceptEdits,
                AcceptEdits,
                "--approval-mode auto-edit",
                &["--acp", "--approval-mode", "auto-edit"],
            ),
            (
                Driver::Qwen,
                Auto,
                Auto,
                "--approval-mode auto",
                &["--acp", "--approval-mode", "auto"],
            ),
            (
                Driver::Qwen,
                Full,
                Full,
                "--approval-mode yolo",
                &["--acp", "--approval-mode", "yolo"],
            ),
            (
                Driver::Copilot,
                Ask,
                Ask,
                "default (asks before edits and commands)",
                &["--acp", "--no-auto-update", "--disallow-temp-dir"],
            ),
            (
                Driver::Copilot,
                AcceptEdits,
                AcceptEdits,
                "--allow-tool write",
                &[
                    "--acp",
                    "--no-auto-update",
                    "--disallow-temp-dir",
                    "--allow-tool",
                    "write",
                ],
            ),
            (
                Driver::Copilot,
                Auto,
                Ask,
                "default (asks before edits and commands)",
                &["--acp", "--no-auto-update", "--disallow-temp-dir"],
            ),
            (
                Driver::Copilot,
                Full,
                Full,
                "permission requests approved automatically",
                &["--acp", "--no-auto-update", "--disallow-temp-dir"],
            ),
            (Driver::Kiro, Ask, Ask, "default permissions", KIRO_ARGV),
            (
                Driver::Kiro,
                AcceptEdits,
                Ask,
                "default permissions",
                KIRO_ARGV,
            ),
            (Driver::Kiro, Auto, Ask, "default permissions", KIRO_ARGV),
            (
                Driver::Kiro,
                Full,
                Full,
                "every permission request approved",
                KIRO_ARGV,
            ),
            (Driver::Qoder, Ask, Ask, "mode default", &["--acp"]),
            (
                Driver::Qoder,
                AcceptEdits,
                AcceptEdits,
                "mode acceptEdits",
                &["--acp"],
            ),
            (Driver::Qoder, Auto, Auto, "mode auto", &["--acp"]),
            (Driver::Qoder, Full, Full, "mode yolo", &["--acp"]),
            (Driver::Vibe, Ask, Ask, "mode ask", &[]),
            (
                Driver::Vibe,
                AcceptEdits,
                AcceptEdits,
                "mode accept-edits",
                &[],
            ),
            (Driver::Vibe, Auto, Auto, "mode smart-approve", &[]),
            (Driver::Vibe, Full, Full, "mode auto-approve", &[]),
            (Driver::Goose, Ask, Ask, "mode approve", &["acp"]),
            (Driver::Goose, AcceptEdits, Ask, "mode approve", &["acp"]),
            (Driver::Goose, Auto, Auto, "mode smart_approve", &["acp"]),
            (Driver::Goose, Full, Full, "mode auto", &["acp"]),
            (Driver::Kimi, Ask, Ask, KIMI_ASK, &["acp"]),
            (Driver::Kimi, AcceptEdits, Ask, KIMI_ASK, &["acp"]),
            (
                Driver::Kimi,
                Auto,
                Auto,
                "mode yolo (Ask When Needed)",
                &["acp"],
            ),
            (Driver::Kimi, Full, Full, "mode auto (Never Ask)", &["acp"]),
        ];
        for (driver, level, applied, native, argv) in cases {
            let p = level_plan(*driver, *level);
            assert_eq!(p.applied, *applied, "{driver:?} {level:?}");
            assert_eq!(p.native, *native, "{driver:?} {level:?}");
            assert_eq!(p.offered, level == applied && p.enforced);
            if let Some(got) = acp_args(*driver, &p, None) {
                assert_eq!(got, *argv, "{driver:?} {level:?}");
            }
        }
        // Nothing widens: an applied level is never wider than the requested one.
        for driver in Driver::all() {
            for level in ApprovalLevel::ALL {
                assert!(level_plan(driver, level).applied.rank() <= level.rank());
            }
        }
        // Full access is never the default and only Full auto-allows ACP requests.
        assert_eq!(ApprovalLevel::default(), Ask);
        for driver in Driver::all() {
            for level in [Ask, AcceptEdits, Auto] {
                assert!(!level_plan(driver, level).acp_allow_all);
            }
        }
        assert_eq!(
            acp_args(
                Driver::Cursor,
                &level_plan(Driver::Cursor, Ask),
                Some("auto")
            )
            .unwrap(),
            ["--model", "auto", "acp"]
        );
        assert_eq!(Driver::Gemini.support(), Support::Experimental);
        assert_eq!(Driver::Qwen.support(), Support::Experimental);
        assert_eq!(
            acp_args(
                Driver::Qwen,
                &level_plan(Driver::Qwen, Ask),
                Some("qwen3-coder-plus")
            )
            .unwrap(),
            [
                "--acp",
                "--approval-mode",
                "default",
                "-m",
                "qwen3-coder-plus"
            ]
        );
        assert_eq!(Driver::Kiro.support(), Support::Experimental);
        assert!(level_plan(Driver::Kiro, Full).acp_allow_all);
        assert_eq!(
            acp_args(Driver::Kiro, &level_plan(Driver::Kiro, Ask), Some("x")).unwrap(),
            KIRO_ARGV
        );
        // Qoder CLI: the mode always goes through `session/set_mode`; only Full is `yolo`.
        assert_eq!(Driver::Qoder.support(), Support::Experimental);
        let qoder: Vec<_> = ApprovalLevel::ALL
            .into_iter()
            .map(|l| level_plan(Driver::Qoder, l).acp_mode)
            .collect();
        assert_eq!(
            qoder,
            [
                Some("default"),
                Some("acceptEdits"),
                Some("auto"),
                Some("yolo")
            ]
        );
        assert_eq!(
            acp_args(Driver::Qoder, &level_plan(Driver::Qoder, Ask), Some("auto")).unwrap(),
            ["--acp"]
        );
        // Mistral Vibe: the four approval policies, always sent; no option or model in argv.
        assert_eq!(Driver::Vibe.support(), Support::Experimental);
        let vibe: Vec<_> = ApprovalLevel::ALL
            .into_iter()
            .map(|l| level_plan(Driver::Vibe, l).acp_mode)
            .collect();
        assert_eq!(
            vibe,
            [
                Some("ask"),
                Some("accept-edits"),
                Some("smart-approve"),
                Some("auto-approve")
            ]
        );
        assert!(level_plan(Driver::Vibe, Full).acp_allow_all);
        assert!(!level_plan(Driver::Vibe, Auto).acp_allow_all);
        assert_eq!(
            acp_args(
                Driver::Vibe,
                &level_plan(Driver::Vibe, Ask),
                Some("devstral-2")
            ),
            Some(Vec::new())
        );
        // Goose: the mode is always sent (its own default is `auto`), Accept edits never
        // widens to anything but `approve`, and argv is only `acp` (no model, no mode).
        assert_eq!(Driver::Goose.support(), Support::Experimental);
        let goose: Vec<_> = ApprovalLevel::ALL
            .into_iter()
            .map(|l| level_plan(Driver::Goose, l).acp_mode)
            .collect();
        assert_eq!(
            goose,
            [
                Some("approve"),
                Some("approve"),
                Some("smart_approve"),
                Some("auto")
            ]
        );
        assert!(level_plan(Driver::Goose, Full).acp_allow_all);
        assert!(!level_plan(Driver::Goose, Auto).acp_allow_all);
        assert_eq!(
            acp_args(
                Driver::Goose,
                &level_plan(Driver::Goose, Ask),
                Some("gpt-4o")
            )
            .unwrap(),
            ["acp"]
        );
        // Kimi Code CLI: the mode always goes through `session/set_mode`, and the CLI's
        // `yolo` ("Ask When Needed") is Auto while its `auto` ("Never Ask") is Full.
        assert_eq!(Driver::Kimi.support(), Support::Experimental);
        let kimi: Vec<_> = ApprovalLevel::ALL
            .into_iter()
            .map(|l| level_plan(Driver::Kimi, l).acp_mode)
            .collect();
        assert_eq!(
            kimi,
            [Some("default"), Some("default"), Some("yolo"), Some("auto")]
        );
        assert_eq!(level_plan(Driver::Kimi, Ask).enforced, cfg!(windows));
        assert!(level_plan(Driver::Kimi, Auto).enforced);
        assert_eq!(
            acp_args(
                Driver::Kimi,
                &level_plan(Driver::Kimi, Ask),
                Some("kimi-for-coding")
            )
            .unwrap(),
            ["acp"]
        );
        assert_eq!(Driver::OpenCode.support(), Support::Experimental);
        assert!(!level_plan(Driver::OpenCode, Ask).enforced);
        // Antigravity: Ask is its default mode, and only Full drops the prompts.
        assert_eq!(level_plan(Driver::Agy, Ask).acp_mode, Some("default"));
        assert_eq!(level_plan(Driver::Agy, Full).acp_mode, None);
        // Copilot CLI: no level passes an allow-all switch; Full approves on Praxi's side.
        assert_eq!(Driver::Copilot.support(), Support::Experimental);
        for level in ApprovalLevel::ALL {
            let args =
                acp_args(Driver::Copilot, &level_plan(Driver::Copilot, level), None).unwrap();
            for wide in [
                "--allow-all-tools",
                "--allow-all",
                "--yolo",
                "--allow-all-paths",
            ] {
                assert!(!args.iter().any(|a| a == wide), "{level:?} passes {wide}");
            }
        }
        assert_eq!(
            acp_args(
                Driver::Copilot,
                &level_plan(Driver::Copilot, Ask),
                Some("auto")
            )
            .unwrap(),
            [
                "--acp",
                "--no-auto-update",
                "--disallow-temp-dir",
                "--model",
                "auto"
            ]
        );
    }
}

//! Engine modes and approval levels for coding CLIs that run with their own tools.
//!
//! A bot either keeps Praxi in charge of files and shell (`praxi_governed`, the
//! confined engine) or hands them to the CLI (`native_tools`). In the second
//! mode the bot's approval level is mapped onto the CLI's own permission mode.
//! Nothing here spawns a process; [`level_plan`] is the one table the runtime,
//! the API and the UI read.

use serde::{Deserialize, Serialize};

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
    /// Qoder CLI (`qodercli --acp`).
    Qoder,
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
    pub const ALL: [Self; 10] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::OpenCode,
        Self::Gemini,
        Self::Agy,
        Self::Qwen,
        Self::Copilot,
        Self::Kiro,
        Self::Qoder,
    ];

    /// Registry id (`claude`, `codex`, `cursor`, `opencode`, `gemini`, `agy`, `qwen`, `copilot`, `kiro`, `qoder`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Agy => "agy",
            Self::Qwen => "qwen",
            Self::Kiro => "kiro",
            Self::Copilot => "copilot",
            Self::Qoder => "qoder",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == id)
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini CLI",
            Self::Agy => "Antigravity",
            Self::Qwen => "Qwen Code",
            Self::Kiro => "Kiro CLI",
            Self::Copilot => "GitHub Copilot CLI",
            Self::Qoder => "Qoder CLI",
        }
    }

    #[must_use]
    pub const fn transport(self) -> Transport {
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
            | Self::Qoder => Transport::Acp,
        }
    }

    /// Gemini CLI is not installed on the machine this was written on and its
    /// install command is unverified, so its driver ships as experimental.
    #[must_use]
    pub const fn support(self) -> Support {
        match self {
            Self::Gemini
            | Self::OpenCode
            | Self::Agy
            | Self::Qwen
            | Self::Copilot
            | Self::Kiro
            | Self::Qoder => Support::Experimental,
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
        Driver::OpenCode => args.push("acp".into()),
        // Qoder: the model is chosen with `session/set_model` after `session/new`, like the
        // mode, so a model the CLI does not list is refused instead of guessed.
        Driver::Qoder => args.push("--acp".into()),
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
        Driver::Claude | Driver::Codex => return None,
    }
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::too_many_lines)] // One table, one test.
    fn level_mapping_per_cli() {
        use ApprovalLevel::{AcceptEdits, Ask, Auto, Full};
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
        for driver in Driver::ALL {
            for level in ApprovalLevel::ALL {
                assert!(level_plan(driver, level).applied.rank() <= level.rank());
            }
        }
        // Full access is never the default and only Full auto-allows ACP requests.
        assert_eq!(ApprovalLevel::default(), Ask);
        for driver in Driver::ALL {
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

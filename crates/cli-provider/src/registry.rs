//! Fixed vendor descriptors. Evidence is recorded in docs/guide/cli-engines.md.
//! No request or model text is ever interpreted as an installer command.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// Verified in a local vendor reference, not tested against the network.
    pub verified: bool,
    /// Only locally verified fixed argv may execute.
    pub executable: bool,
}

impl InstallCommand {
    #[must_use]
    pub fn display(self) -> String {
        if self.program == "powershell.exe" {
            return format!(
                "powershell.exe -NoProfile -Command \"{}\"",
                self.args.last().unwrap_or(&"")
            );
        }
        if self.program == "sh" {
            return format!("sh -c '{}'", self.args.last().unwrap_or(&""));
        }
        std::iter::once(self.program)
            .chain(self.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliEngineSpec {
    pub id: &'static str,
    pub display_name: &'static str,
    pub command: &'static str,
    pub aliases: &'static [&'static str],
    pub runnable: bool,
    pub version_args: &'static [&'static str],
    pub auth_args: &'static [&'static str],
    /// None means no locally verified security floor, never "any version".
    pub minimum_version: Option<(u32, u32, u32)>,
    pub install_windows: Option<InstallCommand>,
    pub install_unix: Option<InstallCommand>,
    /// LAST-RESORT fallback only, used when the CLI cannot be asked and has no local model
    /// cache (see `models.rs`). Deliberately minimal: live lists come from the CLI itself.
    pub models: &'static [&'static str],
    pub login: &'static str,
    pub docs_url: &'static str,
}

impl CliEngineSpec {
    /// The native-tools driver for this engine, if it has one.
    #[must_use]
    pub fn driver(&self) -> Option<crate::levels::Driver> {
        crate::levels::Driver::from_id(self.id)
    }

    /// Can be enabled as a provider: the confined engine runs it, or it has a
    /// native-tools driver (which only runs with the CLI's own tools).
    #[must_use]
    pub fn can_enable(&self) -> bool {
        self.runnable || self.driver().is_some()
    }

    #[must_use]
    pub const fn install(&self) -> Option<InstallCommand> {
        if cfg!(windows) {
            self.install_windows
        } else {
            self.install_unix
        }
    }
}

#[allow(clippy::unnecessary_wraps)] // Descriptors use Option uniformly, including unsupported platforms.
const fn npm(args: &'static [&'static str], verified: bool) -> Option<InstallCommand> {
    Some(InstallCommand {
        program: "npm",
        args,
        verified,
        executable: verified,
    })
}

/// `uv tool install`, shown for copying only: not verified against the network.
#[allow(clippy::unnecessary_wraps)] // Descriptors use Option uniformly, including unsupported platforms.
const fn uv(args: &'static [&'static str]) -> Option<InstallCommand> {
    Some(InstallCommand {
        program: "uv",
        args,
        verified: false,
        executable: false,
    })
}

/// Floor required by the native-tool confinement contract.
pub const CLAUDE_SECURITY_FLOOR: (u32, u32, u32) = (2, 1, 257);

pub const CLI_ENGINES: &[CliEngineSpec] = &[
    CliEngineSpec {
        id: "claude",
        display_name: "Claude Code",
        command: "claude",
        aliases: &[],
        runnable: true,
        version_args: &["--version"],
        auth_args: &["auth", "status", "--json"],
        minimum_version: Some(CLAUDE_SECURITY_FLOOR),
        install_windows: npm(&["install", "-g", "@anthropic-ai/claude-code"], true),
        install_unix: npm(&["install", "-g", "@anthropic-ai/claude-code"], true),
        models: &["default", "fable", "opus", "sonnet", "haiku"],
        login: "claude auth login",
        docs_url: "https://docs.anthropic.com/en/docs/claude-code/setup",
    },
    CliEngineSpec {
        id: "codex",
        display_name: "Codex",
        command: "codex",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &["login", "status"],
        minimum_version: None,
        install_windows: npm(&["install", "-g", "@openai/codex"], true),
        install_unix: npm(&["install", "-g", "@openai/codex"], true),
        models: &["default"],
        login: "codex login",
        docs_url: "https://github.com/openai/codex",
    },
    CliEngineSpec {
        id: "cursor",
        display_name: "Cursor",
        command: "cursor-agent",
        aliases: &["agent"],
        runnable: false,
        version_args: &["--version"],
        auth_args: &["status", "--format", "json"],
        minimum_version: None,
        install_windows: Some(InstallCommand {
            program: "powershell.exe",
            args: &[
                "-NoProfile",
                "-Command",
                "irm 'https://cursor.com/install?win32=true' | iex",
            ],
            verified: true,
            executable: true,
        }),
        install_unix: Some(InstallCommand {
            program: "sh",
            args: &[
                "-c",
                "curl --proto =https --proto-redir =https --tlsv1.2 -fsS https://cursor.com/install | bash",
            ],
            verified: true,
            executable: true,
        }),
        models: &["auto"],
        login: "cursor-agent login",
        docs_url: "https://cursor.com/docs/cli",
    },
    CliEngineSpec {
        id: "opencode",
        display_name: "OpenCode",
        command: "opencode",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: npm(&["install", "-g", "opencode-ai"], true),
        install_unix: npm(&["install", "-g", "opencode-ai"], true),
        models: &["default"],
        login: "opencode auth login",
        docs_url: "https://opencode.ai/docs/",
    },
    // Retired by the vendor: never offered or installable; kept only so an install that
    // already exists keeps working (see `Detector::detect_fresh`).
    CliEngineSpec {
        id: "gemini",
        display_name: "Gemini CLI",
        command: "gemini",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: None,
        install_unix: None,
        models: &["default", "gemini-2.5-pro", "gemini-2.5-flash"],
        login: "gemini",
        docs_url: "https://github.com/google-gemini/gemini-cli",
    },
    // Antigravity CLI. Runs with its own tools through `agy acp` (Agent Client Protocol),
    // like Cursor/OpenCode/Gemini. Sign-in has no status command: it shows at the first turn.
    CliEngineSpec {
        id: "agy",
        display_name: "Antigravity",
        command: "agy",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: None,
        // Official installer, verified in the HyperFrames agent setup reference
        // (faydali-repo). No Windows installer is verified: the UI shows the docs page.
        install_unix: Some(InstallCommand {
            program: "sh",
            args: &[
                "-c",
                "curl --proto =https --proto-redir =https --tlsv1.2 -fsSL https://antigravity.google/cli/install.sh | bash",
            ],
            verified: true,
            executable: true,
        }),
        models: &["default"],
        login: "agy",
        docs_url: "https://antigravity.google/docs",
    },
    // Qwen Code (a Gemini CLI fork). Runs with its own tools through `qwen --acp`.
    // Interface read from the npm package 0.28.4, not from a signed-in run.
    CliEngineSpec {
        id: "qwen",
        display_name: "Qwen Code",
        command: "qwen",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: npm(&["install", "-g", "@qwen-code/qwen-code"], true),
        install_unix: npm(&["install", "-g", "@qwen-code/qwen-code"], true),
        models: &["default"],
        login: "qwen",
        docs_url: "https://qwenlm.github.io/qwen-code-docs/",
    },
    // GitHub Copilot CLI. Runs with its own tools through `copilot --acp`.
    // Interface read from the npm package 1.0.115 (`--help`, an unauthenticated ACP
    // `initialize` and `session/new`), not from a signed-in run. Sign-in is `copilot login`
    // (OS credential store) and has no status command.
    CliEngineSpec {
        id: "copilot",
        display_name: "GitHub Copilot CLI",
        command: "copilot",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: npm(&["install", "-g", "@github/copilot"], true),
        install_unix: npm(&["install", "-g", "@github/copilot"], true),
        models: &["default"],
        login: "copilot login",
        docs_url: "https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli",
    },
    // Kiro CLI (AWS). Runs with its own tools through `kiro-cli acp` (CLI V3). Interface
    // read from kiro.dev/docs/cli, not from an install. The installer is the vendor's
    // documented `https://cli.kiro.dev/install` script (read 2026-11-26: it fetches the
    // `kiro-cli` binary from `prod.download.cli.kiro.dev`). `whoami` has a status output but
    // its signed-out exit code is undocumented, so sign-in stays unverified.
    CliEngineSpec {
        id: "kiro",
        display_name: "Kiro CLI",
        command: "kiro-cli",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: None,
        install_unix: Some(InstallCommand {
            program: "sh",
            args: &[
                "-c",
                "curl --proto =https --proto-redir =https --tlsv1.2 -fsSL https://cli.kiro.dev/install | bash",
            ],
            verified: true,
            executable: true,
        }),
        models: &["default"],
        login: "kiro-cli login",
        docs_url: "https://kiro.dev/docs/cli/",
    },
    // Kimi Code CLI (Moonshot). Runs with its own tools through `kimi acp`.
    // Interface read from the npm package 2.7.3 (`--help`, an unauthenticated ACP
    // `initialize`, and the bundled ACP server), not from a signed-in run. The Python
    // `kimi-cli` (1.x) is the retired predecessor under the same command name.
    CliEngineSpec {
        id: "kimi",
        display_name: "Kimi Code CLI",
        command: "kimi",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: npm(&["install", "-g", "@moonshot-ai/kimi-code"], true),
        install_unix: npm(&["install", "-g", "@moonshot-ai/kimi-code"], true),
        models: &["default"],
        login: "kimi login",
        docs_url: "https://moonshotai.github.io/kimi-code/",
    },
    // Qoder CLI (Alibaba). Runs with its own tools through `qodercli --acp` (a hidden flag:
    // "Starts the agent in ACP mode"). Interface read from the npm package 1.1.97 (`--help`,
    // `status -o json`, an unauthenticated ACP `initialize` and `session/new`), not from a
    // signed-in run.
    CliEngineSpec {
        id: "qoder",
        display_name: "Qoder CLI",
        command: "qodercli",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &["status", "-o", "json"],
        minimum_version: None,
        install_windows: npm(&["install", "-g", "@qoder-ai/qodercli"], true),
        install_unix: npm(&["install", "-g", "@qoder-ai/qodercli"], true),
        models: &["default"],
        login: "qodercli login",
        docs_url: "https://qoder.com/cli",
    },
    // Mistral Vibe (Mistral AI). Runs with its own tools through `vibe-acp`, a separate
    // entry point of the PyPI package `mistral-vibe` (Python 3.12+). Interface read from
    // 3.1.5 (`--help`, `--version`, an unauthenticated ACP `initialize` and `session/new`),
    // not from a signed-in run. The API key can live in the environment, `~/.vibe/.env`,
    // the OS keyring or a browser sign-in, and there is no status command, so sign-in is
    // never claimed.
    CliEngineSpec {
        id: "vibe",
        display_name: "Mistral Vibe",
        command: "vibe-acp",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: uv(&["tool", "install", "mistral-vibe"]),
        install_unix: uv(&["tool", "install", "mistral-vibe"]),
        models: &["default"],
        login: "vibe --setup",
        docs_url: "https://github.com/mistralai/mistral-vibe",
    },
    // Goose (AAIF, formerly Block). Runs with its own tools through `goose acp`, the ACP
    // server its own clients use. Interface read from 1.61.1 (`goose --version`,
    // `goose acp --help`, an unauthenticated ACP `initialize` and `session/new`) and its
    // source, not from a signed-in run. Keys live in the OS keyring or in
    // `secrets.yaml`, set up by `goose configure`, and there is no status command, so
    // sign-in is never claimed. The documented installer is a shell script.
    CliEngineSpec {
        id: "goose",
        display_name: "Goose",
        command: "goose",
        aliases: &[],
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: None,
        install_unix: Some(InstallCommand {
            program: "sh",
            args: &[
                "-c",
                "curl --proto =https --proto-redir =https --tlsv1.2 -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | CONFIGURE=false bash",
            ],
            verified: false,
            executable: false,
        }),
        models: &["default"],
        login: "goose configure",
        docs_url: "https://github.com/aaif-goose/goose",
    },
];

#[must_use]
pub fn engine_spec(id: &str) -> Option<&'static CliEngineSpec> {
    CLI_ENGINES.iter().find(|spec| spec.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_is_unique_and_installers_are_fixed() {
        let mut ids = std::collections::HashSet::new();
        for spec in CLI_ENGINES {
            assert!(ids.insert(spec.id));
            assert_eq!(spec.version_args, ["--version"]);
            for install in [spec.install_windows, spec.install_unix]
                .into_iter()
                .flatten()
            {
                assert_ne!(install.args, [] as [&str; 0]);
                if install.executable {
                    assert!(install.verified);
                    if install.program != "npm" {
                        assert!(matches!(install.program, "powershell.exe" | "sh"));
                        assert!(matches!(spec.id, "cursor" | "agy" | "kiro"));
                        let url = match spec.id {
                            "cursor" => "https://cursor.com/install",
                            "kiro" => "https://cli.kiro.dev/install",
                            _ => "https://antigravity.google/cli/install.sh",
                        };
                        assert!(install.args.last().unwrap().contains(url));
                        continue;
                    }
                    assert_eq!(&install.args[..2], ["install", "-g"]);
                    assert_eq!(install.args.len(), 3);
                    assert!(
                        install.args[2]
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "@/.-_".contains(c))
                    );
                }
            }
        }
        assert!(!engine_spec("opencode").unwrap().runnable);
    }

    #[test]
    fn every_engine_with_a_driver_offers_models_the_cli_accepts() {
        for spec in CLI_ENGINES.iter().filter(|s| s.driver().is_some()) {
            assert!(!spec.models.is_empty(), "{}", spec.id);
            assert!(
                spec.models
                    .iter()
                    .all(|m| !m.starts_with('-') && !m.contains(' '))
            );
        }
        assert!(engine_spec("claude").unwrap().models.contains(&"haiku"));
    }
}

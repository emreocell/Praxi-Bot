//! Bot and permission presets offered by `bot setup`.

use clap::ValueEnum;
use serde_json::{Value, json};

/// Interface language of the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Lang {
    /// English.
    En,
    /// Turkish.
    Tr,
}

impl Lang {
    /// Pick the English or Turkish text.
    #[must_use]
    pub fn t<'a>(self, en: &'a str, tr: &'a str) -> &'a str {
        match self {
            Self::En => en,
            Self::Tr => tr,
        }
    }

    /// Short code stored in settings.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Tr => "tr",
        }
    }
}

/// First-bot presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BotPreset {
    /// General assistant.
    General,
    /// Browser researcher.
    Browser,
    /// Desktop operator.
    Desktop,
}

impl BotPreset {
    /// All presets in menu order.
    pub const ALL: [Self; 3] = [Self::General, Self::Browser, Self::Desktop];

    /// Stable id stored in the bot config.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Browser => "browser",
            Self::Desktop => "desktop",
        }
    }

    /// Menu label.
    #[must_use]
    pub fn label(self, l: Lang) -> &'static str {
        match self {
            Self::General => l.t("General assistant", "Genel asistan"),
            Self::Browser => l.t("Browser researcher", "Tarayıcı araştırmacısı"),
            Self::Desktop => l.t("Desktop operator", "Masaüstü operatörü"),
        }
    }

    /// Default bot name.
    #[must_use]
    pub fn default_name(self) -> &'static str {
        match self {
            Self::General => "Assistant",
            Self::Browser => "Researcher",
            Self::Desktop => "Operator",
        }
    }

    /// System prompt seed.
    #[must_use]
    pub fn system_prompt(self) -> &'static str {
        match self {
            Self::General => "You are a helpful general-purpose assistant.",
            Self::Browser => {
                "You research topics on the web with the browser tools and cite your sources."
            }
            Self::Desktop => {
                "You operate the user's computer carefully, step by step, asking before risky actions."
            }
        }
    }

    /// Tool-name globs the preset exposes (`[tools] allow` of the profile).
    ///
    /// Every preset keeps the agent-internal tools (questions, todos,
    /// sub-agents, skills, workflows, tool discovery) so the loop works.
    /// The General preset also reaches MCP servers (`mcp:*`) and email
    /// (`email_*`).  Stored profiles are not rewritten: to give an existing bot
    /// the same reach, add `"mcp:*"` and `"email_*"` to `[tools] allow` of its
    /// profile (for example in the bot's edit form or the profile TOML).
    #[must_use]
    pub fn tool_allow(self) -> Vec<&'static str> {
        let mut v = vec![
            "ask_user",
            "todo_write",
            "spawn_subagent",
            "find_tool",
            "get_tool_details",
            "use_tool",
            "skill_*",
            "workflow_*",
        ];
        match self {
            // `mcp:*` and `email_*` only make those tools reachable; every MCP
            // tool is `Destructive` / `Accounts` unless its server is trusted and
            // `email_send` is `Destructive`, so the permission policy still asks.
            // Which bots may use which MCP server is the server's `bots` list.
            Self::General => v.extend(["fs_read", "fs_list", "fs_search", "mcp:*", "email_*"]),
            Self::Browser => v.push("browser_*"),
            Self::Desktop => v.extend(["desktop_*", "fs_*", "checkpoint_*"]),
        }
        v
    }
}

/// Default permission presets (map onto `permission-engine` policies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PermissionPreset {
    /// Deny everything.
    ReadOnly,
    /// Reads and drafts; sending asks.
    ReadAndDraft,
    /// Ask before writes / destructive actions (default).
    ApproveDestructive,
    /// Allow everything without asking.
    Full,
}

impl PermissionPreset {
    /// All presets in menu order (safest default first for the wizard).
    pub const ALL: [Self; 4] = [
        Self::ApproveDestructive,
        Self::ReadAndDraft,
        Self::ReadOnly,
        Self::Full,
    ];

    /// Stable id.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadAndDraft => "read-and-draft",
            Self::ApproveDestructive => "approve-destructive",
            Self::Full => "full",
        }
    }

    /// Menu label.
    #[must_use]
    pub fn label(self, l: Lang) -> &'static str {
        match self {
            Self::ReadOnly => l.t(
                "Read-only (deny everything else)",
                "Salt okunur (diğer her şey yasak)",
            ),
            Self::ReadAndDraft => l.t(
                "Read and draft (sending asks)",
                "Oku ve taslak hazırla (göndermek onay ister)",
            ),
            Self::ApproveDestructive => l.t(
                "Ask before writes/destructive actions (recommended)",
                "Yazma/yıkıcı işlemlerde sor (önerilen)",
            ),
            Self::Full => l.t(
                "Allow everything (no prompts)",
                "Her şeye izin ver (sormadan)",
            ),
        }
    }
}

/// Profile slug for a bot name: lower-case ASCII letters and digits joined by
/// `-`; `fallback` when nothing usable remains.
#[must_use]
pub fn slug(name: &str, fallback: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = mapped
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// The `config` JSON stored on the created bot record: a real bot profile,
/// the structure `agent_core::BotProfile` loads (same as `bot bots add`).
#[must_use]
pub fn bot_config(
    preset: BotPreset,
    perm: PermissionPreset,
    name: &str,
    model: &str,
    lang: Lang,
) -> Value {
    json!({
        "bot": {
            "id": slug(name, preset.id()),
            "name": name,
            "persona": preset.system_prompt(),
            "language": lang.code(),
        },
        "models": { "executor": model },
        "permissions": { "preset": perm.id() },
        "tools": { "allow": preset.tool_allow() },
    })
}

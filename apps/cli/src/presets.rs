//! Bot and permission presets offered by `bot setup`.

use clap::ValueEnum;
use permission_engine::Policy;
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

    /// Tool groups the preset enables.
    #[must_use]
    pub fn tool_groups(self) -> &'static [&'static str] {
        match self {
            Self::General => &["memory"],
            Self::Browser => &["memory", "browser"],
            Self::Desktop => &["memory", "desktop", "fs"],
        }
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

    /// The policy this preset stands for.
    #[must_use]
    pub fn policy(self) -> Policy {
        match self {
            Self::ReadOnly => Policy::read_only(),
            Self::ReadAndDraft => Policy::read_and_draft(),
            Self::ApproveDestructive => Policy::approve_destructive(),
            Self::Full => Policy::full(),
        }
    }
}

/// The `config` JSON stored on the created bot record.
#[must_use]
pub fn bot_config(
    preset: BotPreset,
    perm: PermissionPreset,
    model: Option<&str>,
    lang: Lang,
) -> Value {
    json!({
        "preset": preset.id(),
        "language": lang.code(),
        "system_prompt": preset.system_prompt(),
        "tool_groups": preset.tool_groups(),
        "model": model,
        "permission_preset": perm.id(),
        "permissions": serde_json::to_value(perm.policy()).unwrap_or(Value::Null),
    })
}

//! Autonomy modes: how much a bot may do without asking, in three words.
//!
//! A mode is a *lane over a plain policy-level ask*. It never touches a deny, an
//! absolute floor (money, never-list), an always-ask tool, the taint gate (egress
//! or program start after untrusted content) or unevaluated limits: the caller
//! decides all of those first and only asks this module about the call that is
//! still a plain ask. That ordering is what keeps the strict posture intact.
//!
//! * [`AutonomyMode::Ask`]: every call that is not a read asks.
//! * [`AutonomyMode::Balanced`]: reads, browsing, workspace edits and work on the
//!   bot's own computer run without asking; everything outward-facing,
//!   destructive, financial or on the user's own PC asks.
//! * [`AutonomyMode::Full`]: the policy allows every category; only the floors
//!   and the taint gate remain.

use protocol::PermissionCategory;

/// Per-bot autonomy mode. Derived from the stored permission preset, so no
/// profile or API shape changes and old profiles keep working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AutonomyMode {
    /// Confirm anything with a side effect.
    #[default]
    Ask,
    /// Reads, browsing, workspace edits and own-computer work run freely.
    Balanced,
    /// No prompts inside the grants; hard stops and the taint gate remain.
    Full,
}

/// The one spelling rule for a stored preset name: edge spaces, case and `_` vs `-` do not matter
/// (`" Full "` and `approve_destructive` are what the owner meant). The app applies the same rule.
#[must_use]
pub fn normalize_preset(preset: &str) -> String {
    preset.trim().to_ascii_lowercase().replace('_', "-")
}

impl AutonomyMode {
    /// Mode implied by a stored preset name (`approve-destructive`,
    /// `read-and-draft`, `full`, `read-only`; `_` and `-` are interchangeable).
    /// Unknown, custom and absent presets are [`AutonomyMode::Ask`]: a bot that
    /// someone configured by hand never gets a lane it did not choose.
    #[must_use]
    pub fn from_preset(preset: Option<&str>) -> Self {
        match preset.map(normalize_preset).as_deref() {
            Some("full") => Self::Full,
            Some("approve-destructive") => Self::Balanced,
            _ => Self::Ask,
        }
    }

    /// Stable lower-case name (audit records, API).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Balanced => "balanced",
            Self::Full => "full",
        }
    }
}

/// How much a tool call can change, as the tool declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Impact {
    /// Only reads.
    Read,
    /// Writes, reversible or low risk.
    Write,
    /// Irreversible or high risk.
    Destructive,
}

/// What the Balanced lane needs to know about one call.
#[derive(Debug, Clone, Copy)]
pub struct CallFacts {
    /// Permission category of the tool.
    pub category: PermissionCategory,
    /// Declared impact of the tool.
    pub impact: Impact,
    /// The call runs on the bot's own computer (its host), not the user's PC.
    pub own_computer: bool,
    /// The call asks for a named, possibly signed-in browser profile instead of
    /// the isolated temporary one.
    pub named_browser_profile: bool,
}

/// Why the lane allowed a call. The code goes into the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowReason {
    /// A file edit (the file tools enforce their own roots).
    WorkspaceEdit,
    /// Navigation and page work in an isolated browser profile.
    Browse,
    /// A shell or desktop call on the bot's own computer.
    OwnComputer,
}

impl AllowReason {
    /// Stable reason code for the audit log.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WorkspaceEdit => "mode.balanced.workspace_edit",
            Self::Browse => "mode.balanced.browse",
            Self::OwnComputer => "mode.balanced.own_computer",
        }
    }
}

/// The Balanced lane: `Some(reason)` when this call may skip the prompt.
///
/// Money, messaging, accounts, system and remote calls never get a lane, nor do
/// destructive file operations, anything on the user's own PC (shell, scripts,
/// desktop) or a browser profile that may be signed in.
#[must_use]
pub fn balanced_lane(facts: &CallFacts) -> Option<AllowReason> {
    use PermissionCategory as C;
    match (facts.category, facts.impact) {
        (C::Fs, Impact::Write) => Some(AllowReason::WorkspaceEdit),
        (C::Browser, Impact::Read | Impact::Write) if !facts.named_browser_profile => {
            Some(AllowReason::Browse)
        }
        (C::Shell | C::Script, _) if facts.own_computer => Some(AllowReason::OwnComputer),
        (C::Desktop, Impact::Read | Impact::Write) if facts.own_computer => {
            Some(AllowReason::OwnComputer)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(category: PermissionCategory, impact: Impact) -> CallFacts {
        CallFacts {
            category,
            impact,
            own_computer: false,
            named_browser_profile: false,
        }
    }

    #[test]
    fn presets_map_to_modes_and_unknown_never_gets_a_lane() {
        assert_eq!(
            AutonomyMode::from_preset(Some("approve_destructive")),
            AutonomyMode::Balanced
        );
        assert_eq!(AutonomyMode::from_preset(Some("full")), AutonomyMode::Full);
        // One spelling rule for the daemon and the app: case, edge spaces and `_` do not matter.
        for spelled in ["Full", " FULL ", "fu_ll"] {
            let want = if spelled == "fu_ll" {
                AutonomyMode::Ask
            } else {
                AutonomyMode::Full
            };
            assert_eq!(
                AutonomyMode::from_preset(Some(spelled)),
                want,
                "{spelled:?}"
            );
        }
        assert_eq!(
            AutonomyMode::from_preset(Some(" Approve_Destructive\n")),
            AutonomyMode::Balanced
        );
        for other in [Some("read-and-draft"), Some("read-only"), Some("x"), None] {
            assert_eq!(AutonomyMode::from_preset(other), AutonomyMode::Ask);
        }
    }

    #[test]
    fn balanced_allows_edits_and_isolated_browsing() {
        assert_eq!(
            balanced_lane(&facts(PermissionCategory::Fs, Impact::Write)),
            Some(AllowReason::WorkspaceEdit)
        );
        assert_eq!(
            balanced_lane(&facts(PermissionCategory::Browser, Impact::Write)),
            Some(AllowReason::Browse)
        );
        let mut named = facts(PermissionCategory::Browser, Impact::Write);
        named.named_browser_profile = true;
        assert_eq!(balanced_lane(&named), None);
    }

    #[test]
    fn balanced_never_covers_deletes_sends_money_or_the_users_pc() {
        use PermissionCategory as C;
        assert_eq!(balanced_lane(&facts(C::Fs, Impact::Destructive)), None);
        for c in [C::Messaging, C::Money, C::Accounts, C::System, C::Remote] {
            for i in [Impact::Read, Impact::Write, Impact::Destructive] {
                let mut f = facts(c, i);
                f.own_computer = true;
                assert_eq!(balanced_lane(&f), None, "{c:?}/{i:?}");
            }
        }
        for c in [C::Shell, C::Script, C::Desktop] {
            assert_eq!(
                balanced_lane(&facts(c, Impact::Write)),
                None,
                "{c:?} on the PC"
            );
        }
    }

    #[test]
    fn own_computer_runs_shell_but_desktop_destructive_still_asks() {
        let mut shell = facts(PermissionCategory::Shell, Impact::Destructive);
        shell.own_computer = true;
        assert_eq!(balanced_lane(&shell), Some(AllowReason::OwnComputer));
        let mut desktop = facts(PermissionCategory::Desktop, Impact::Destructive);
        desktop.own_computer = true;
        assert_eq!(balanced_lane(&desktop), None);
    }
}

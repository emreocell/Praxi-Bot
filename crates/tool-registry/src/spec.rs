//! Tool specification types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export shared types from the protocol crate so users of `tool-registry`
// only need to import from one place.
pub use protocol::{PermissionCategory, Platform};

/// The effect level a tool has on the system.
///
/// A `Write` effect **must** be explicitly marked — the registry enforces that
/// anything capable of writing is at least `Destructive` unless the author
/// opts in to the weaker `Write` level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// The tool only reads state; it cannot modify anything.
    Read,
    /// The tool can write state but changes are reversible or low-risk.
    Write,
    /// The tool can cause irreversible or high-risk changes.
    Destructive,
}

impl Default for Effect {
    /// The safe default for any tool that writes is [`Effect::Destructive`].
    fn default() -> Self {
        Self::Destructive
    }
}

fn default_true() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes `&bool`
fn is_false(value: &bool) -> bool {
    !*value
}

/// How a tool relates to untrusted content, private data, and data leaving
/// the machine.
///
/// Every registered tool must set this with [`ContentClass::declared`].
/// [`ContentClass::unspecified`] is the conservative default (all three
/// flags true) and is **not** a declaration: the registry refuses it.
///
/// The classification bits and whether an author declared them are
/// independent. `executes` defaults to false so an older spec does not
/// start asking on every call. Folding the bits into an enum would hide the
/// "missing means all three, undeclared" serde default.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentClass {
    /// The result can carry web pages, feeds, e-mail, MCP output, files from
    /// outside the workspace, screenshots, OCR, or attachments.
    #[serde(default = "default_true")]
    pub ingests_untrusted: bool,
    /// The result can carry private data (files, mailbox, memory, desktop or
    /// browser observation, MCP tools that are not marked public).
    #[serde(default = "default_true")]
    pub reads_private: bool,
    /// The call can send data off the machine.
    #[serde(default = "default_true")]
    pub egress: bool,
    /// The call starts an arbitrary program chosen by its arguments (a shell,
    /// a script, a desktop launch). A run tainted by untrusted content asks
    /// before every such call, once, and the egress waiver does not cover it.
    /// Missing on the wire means it does not.
    #[serde(default, skip_serializing_if = "is_false")]
    pub executes: bool,
    /// The call acts only inside a browser the OWNER connected as a browser
    /// (an MCP server whose configured kind is `browser`, or the built-in
    /// Playwright preset): navigate, snapshot, click, type, evaluate inside the
    /// page. Its egress is that browser's own page traffic. The egress gate
    /// lets it through while the run has read only untrusted content and asks
    /// once anything private was read. Set only by the MCP bridge from owner
    /// configuration, never from a server's or a model's text. Missing on the
    /// wire means it does not.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_owner_browser: bool,
    /// `false` only for [`ContentClass::unspecified`] and for a spec that was
    /// deserialized without an explicit declaration.
    #[serde(default)]
    declared: bool,
    /// The description and input schema were supplied from outside the daemon
    /// (a remote MCP server, an `OpenAPI` document). Showing that metadata to
    /// the model ingests untrusted text, whether or not the tool is called.
    #[serde(default)]
    external_metadata: bool,
}

impl Default for ContentClass {
    /// Conservative and undeclared. Registration rejects this.
    fn default() -> Self {
        Self::unspecified()
    }
}

impl ContentClass {
    /// An explicit classification. This is what a tool author writes.
    #[must_use]
    pub const fn declared(ingests_untrusted: bool, reads_private: bool, egress: bool) -> Self {
        Self {
            ingests_untrusted,
            reads_private,
            egress,
            executes: false,
            in_owner_browser: false,
            declared: true,
            external_metadata: false,
        }
    }

    /// This call acts only inside the browser the owner connected (see
    /// [`ContentClass::in_owner_browser`]).
    #[must_use]
    pub const fn in_owner_browser(mut self) -> Self {
        self.in_owner_browser = true;
        self
    }

    /// This call starts an arbitrary program.
    #[must_use]
    pub const fn executes(mut self) -> Self {
        self.executes = true;
        self
    }

    /// This spec's description and schema are attacker-controlled text.
    #[must_use]
    pub const fn with_external_metadata(mut self) -> Self {
        self.external_metadata = true;
        self
    }

    /// No untrusted ingestion, no private read, no egress.
    #[must_use]
    pub const fn none() -> Self {
        Self::declared(false, false, false)
    }

    /// Reads private data and does not send it anywhere.
    #[must_use]
    pub const fn private_read() -> Self {
        Self::declared(false, true, false)
    }

    /// Ingests untrusted content and does not read private data or egress.
    #[must_use]
    pub const fn untrusted() -> Self {
        Self::declared(true, false, false)
    }

    /// Ingests untrusted content and reads private data. No egress.
    #[must_use]
    pub const fn untrusted_private() -> Self {
        Self::declared(true, true, false)
    }

    /// Can send data off the machine. Does not itself ingest content.
    #[must_use]
    pub const fn egress() -> Self {
        Self::declared(false, false, true)
    }

    /// Sends data out and the reply is untrusted (search, fetch). Not private.
    #[must_use]
    pub const fn untrusted_egress() -> Self {
        Self::declared(true, false, true)
    }

    /// All three flags. The conservative declared choice when a tool can do
    /// any of them (shell, script, a browser action that both reads the page
    /// and can submit it).
    #[must_use]
    pub const fn all() -> Self {
        Self::declared(true, true, true)
    }

    /// Forgotten classification. Treated as all three flags, and rejected by
    /// [`crate::ToolRegistry::register`].
    #[must_use]
    pub const fn unspecified() -> Self {
        Self {
            ingests_untrusted: true,
            reads_private: true,
            egress: true,
            executes: false,
            in_owner_browser: false,
            declared: false,
            external_metadata: false,
        }
    }

    /// The author set the three flags explicitly.
    #[must_use]
    pub const fn is_declared(self) -> bool {
        self.declared
    }

    /// Description and schema come from outside the daemon.
    #[must_use]
    pub const fn has_external_metadata(self) -> bool {
        self.external_metadata
    }
}

/// What kind of full, current-state observation a built-in tool returns.  A
/// newer observation of the same target makes an older one stale, so the agent
/// loop may shorten the older copy it re-sends to the model (never the stored
/// step output).
///
/// Only tools that return the whole observation are listed: diff-returning
/// browser actions (`browser_click`, ...) are not, because their output is
/// only meaningful next to the full snapshot they refer to.
///
/// Kept as a lookup ([`ObservationScope::of_builtin`]) rather than a field on
/// [`ToolSpec`] so every existing spec literal (built-in, MCP, CLI, plugin)
/// stays valid and unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationScope {
    /// The full page of the browser (`browser_open`, `browser_snapshot`).
    Page,
    /// The accessibility state of a desktop window.
    Window,
    /// A directory listing.
    FsListing,
    /// The list of open windows.
    AppListing,
}

impl ObservationScope {
    /// Scope for a bare tool name, without checking what is registered under
    /// that name.  Prefer [`Self::of_builtin`].
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        match name {
            "browser_open" | "browser_snapshot" => Some(Self::Page),
            "desktop_get_state" => Some(Self::Window),
            "fs_list" => Some(Self::FsListing),
            "desktop_list_apps" => Some(Self::AppListing),
            _ => None,
        }
    }

    /// Scope of the registered tool `spec`, only when it really is the
    /// built-in one: the name matches and so does the permission category
    /// (a CLI, plugin or MCP tool that happens to share a bare name does not).
    #[must_use]
    pub fn of_builtin(spec: &ToolSpec) -> Option<Self> {
        let scope = Self::of_name(&spec.name)?;
        let category = match scope {
            Self::Page => PermissionCategory::Browser,
            Self::Window | Self::AppListing => PermissionCategory::Desktop,
            Self::FsListing => PermissionCategory::Fs,
        };
        (spec.category == category).then_some(scope)
    }
}

/// Static description of a tool, registered once at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool name (e.g. `"fs.read_file"`).
    pub name: String,
    /// Human-readable description used for tool discovery and display.
    pub description: String,
    /// JSON Schema for the tool's input object.
    pub input_schema: Value,
    /// Effect level of this tool.
    pub effect: Effect,
    /// Permission category this tool belongs to.
    pub category: PermissionCategory,
    /// Platforms on which this tool is available.
    pub platforms: Vec<Platform>,
    /// Untrusted / private / egress classification. Missing on the wire is
    /// [`ContentClass::unspecified`] (conservative, not a declaration).
    #[serde(default)]
    pub content: ContentClass,
}

impl ToolSpec {
    /// Return `true` if the spec passes basic validity rules.
    ///
    /// Currently checks:
    /// - `name` is non-empty
    /// - `description` is non-empty
    /// - `input_schema` is a JSON object
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.description.is_empty()
            && self.input_schema.is_object()
            && self.content.is_declared()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_spec() -> ToolSpec {
        ToolSpec {
            name: "fs.write_file".into(),
            description: "Write bytes to a file.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            effect: Effect::Write,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
            content: ContentClass::private_read(),
        }
    }

    #[test]
    fn write_effect_must_be_explicitly_marked() {
        // A spec with effect=Write must NOT auto-upgrade to Destructive;
        // the author consciously chose Write.  The registry enforces that
        // anything that *writes* is at minimum Write (not Read).
        let spec = write_spec();
        assert!(
            spec.effect >= Effect::Write,
            "write spec must have at least Write effect"
        );
    }

    #[test]
    fn read_only_spec_has_read_effect() {
        let spec = ToolSpec {
            name: "fs.read_file".into(),
            description: "Read bytes from a file.".into(),
            input_schema: json!({ "type": "object" }),
            effect: Effect::Read,
            category: PermissionCategory::Fs,
            platforms: vec![Platform::All],
            content: ContentClass::untrusted_private(),
        };
        assert_eq!(spec.effect, Effect::Read);
    }

    #[test]
    fn observation_scopes_cover_the_full_observation_tools_only() {
        let mk = |name: &str, category| ToolSpec {
            name: name.into(),
            description: "d".into(),
            input_schema: json!({ "type": "object" }),
            effect: Effect::Read,
            category,
            platforms: vec![Platform::All],
            content: ContentClass::none(),
        };
        assert_eq!(
            ObservationScope::of_builtin(&mk("browser_snapshot", PermissionCategory::Browser)),
            Some(ObservationScope::Page)
        );
        assert_eq!(
            ObservationScope::of_builtin(&mk("desktop_get_state", PermissionCategory::Desktop)),
            Some(ObservationScope::Window)
        );
        assert_eq!(
            ObservationScope::of_builtin(&mk("fs_list", PermissionCategory::Fs)),
            Some(ObservationScope::FsListing)
        );
        assert_eq!(
            ObservationScope::of_builtin(&mk("desktop_list_apps", PermissionCategory::Desktop)),
            Some(ObservationScope::AppListing)
        );
        // Diff-returning actions and unrelated tools are not observations.
        for n in [
            "browser_click",
            "browser_fill",
            "browser_eval",
            "fs_write_file",
        ] {
            assert_eq!(ObservationScope::of_name(n), None, "{n}");
        }
        // A colliding bare name of another kind of tool does not qualify.
        assert_eq!(
            ObservationScope::of_builtin(&mk("fs_list", PermissionCategory::Script)),
            None
        );
        assert_eq!(ObservationScope::of_name("mcp:x/browser_snapshot"), None);
    }

    #[test]
    fn default_effect_is_destructive() {
        assert_eq!(Effect::default(), Effect::Destructive);
    }

    #[test]
    fn valid_spec_passes() {
        assert!(write_spec().is_valid());
    }

    #[test]
    fn empty_name_fails_validity() {
        let mut spec = write_spec();
        spec.name = String::new();
        assert!(!spec.is_valid());
    }
}

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

/// How a tool relates to untrusted content, private data, and data leaving
/// the machine.
///
/// Every registered tool must set this with [`ContentClass::declared`].
/// [`ContentClass::unspecified`] is the conservative default (all three
/// flags true) and is **not** a declaration: the registry refuses it.
///
/// The four flags are independent: the three classification bits and whether
/// an author declared them. Folding them into an enum would hide the
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
            declared: true,
            external_metadata: false,
        }
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

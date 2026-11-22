//! Pinning of approved MCP tool definitions (a defence against "rug pulls").
//!
//! A server can change a tool's description or schema after the owner
//! connected it, and put new instructions in front of the model. The first
//! time the owner's connected server lists a tool, a fingerprint of the whole
//! definition is stored (the *approval*). When a later listing hashes
//! differently, the tool is **not registered** — so it can neither be called
//! nor reach the model through `find_tool` / `get_tool_details` — until the
//! owner approves the new definition ([`crate::McpManager::approve_tool_changes`]).

use std::{collections::BTreeMap, fs, path::PathBuf};

use parking_lot::Mutex;
use rmcp::model::Tool as McpTool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::tool::clean_description;

/// What was approved for one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPin {
    /// SHA-256 (hex) over name, title, description, schemas and annotations.
    pub hash: String,
    /// The approved description as the model would see it (cleaned), kept to
    /// show a before/after.
    pub description: String,
    /// Hash of the raw description (catches changes of hidden characters).
    pub description_hash: String,
    /// Hash of the input schema.
    pub schema_hash: String,
}

/// A tool whose definition no longer matches what was approved.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ToolChange {
    /// Server-side tool name.
    pub tool: String,
    /// Approved description.
    pub before: String,
    /// Description the server offers now (cleaned, bounded).
    pub after: String,
    /// The visible description text differs.
    pub description_changed: bool,
    /// The input schema differs.
    pub schema_changed: bool,
    /// Something else differs (hidden characters, title, annotations, output
    /// schema) while the visible text may look the same.
    pub other_changed: bool,
    /// The tool was not part of the approved set of a server that already has
    /// approved tools (a new or renamed tool): `before` is empty.
    pub new_tool: bool,
    /// Hash of the definition offered now. An approval must quote it, so only
    /// the exact definition the owner was shown can be approved.
    pub hash: String,
    /// The definition offered now; stored by an approval, never shown.
    pub(crate) pending: ToolPin,
}

fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for k in keys {
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                canonical(&map[k], out);
                out.push(',');
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for i in items {
                canonical(i, out);
                out.push(',');
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn digest(value: &Value) -> String {
    let mut text = String::new();
    canonical(value, &mut text);
    let sum = Sha256::digest(text.as_bytes());
    sum.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Fingerprint of a tool definition as the server sent it.
pub(crate) fn pin_of(tool: &McpTool) -> ToolPin {
    let schema = Value::Object((*tool.input_schema).clone());
    let description = tool.description.as_deref().unwrap_or_default();
    let whole = serde_json::json!({
        "name": tool.name,
        "title": tool.title,
        "description": tool.description,
        "input_schema": schema,
        "output_schema": tool.output_schema,
        "annotations": tool.annotations,
    });
    ToolPin {
        hash: digest(&whole),
        description: clean_description(description),
        description_hash: digest(&Value::String(description.to_owned())),
        schema_hash: digest(&schema),
    }
}

/// How `current` differs from the approved `pinned`.
pub(crate) fn change_of(tool: &str, pinned: &ToolPin, current: ToolPin) -> ToolChange {
    let description_changed = pinned.description != current.description;
    let schema_changed = pinned.schema_hash != current.schema_hash;
    ToolChange {
        tool: tool.to_owned(),
        before: pinned.description.clone(),
        after: current.description.clone(),
        description_changed,
        schema_changed,
        other_changed: !description_changed
            && (!schema_changed || pinned.description_hash != current.description_hash),
        new_tool: false,
        hash: current.hash.clone(),
        pending: current,
    }
}

/// A tool the owner never approved, offered by a server that already has
/// approved tools (or whose pin file could not be read).
pub(crate) fn new_tool_change(tool: &str, current: ToolPin) -> ToolChange {
    let none = ToolPin {
        hash: String::new(),
        description: String::new(),
        description_hash: String::new(),
        schema_hash: String::new(),
    };
    let mut change = change_of(tool, &none, current);
    change.new_tool = true;
    change.description_changed = true;
    change.other_changed = false;
    change
}

/// Where approved fingerprints live. Implementations must be cheap: they are
/// called while a server's tools are registered.
pub trait PinStore: Send + Sync {
    /// The approved pin of `server`'s `tool`.
    fn get(&self, server: &str, tool: &str) -> Option<ToolPin>;
    /// Whether `server` already has approved tools. A tool without a pin on
    /// such a server is new and needs approval; only a server that was never
    /// seen gets its first listing approved automatically. A store that could
    /// not read its data answers `true` for every server (fail closed).
    fn has_server(&self, server: &str) -> bool;
    /// Record an approval.
    fn put(&self, server: &str, tool: &str, pin: ToolPin);
    /// Forget everything about a removed server.
    fn forget_server(&self, server: &str);
}

type Pins = BTreeMap<String, BTreeMap<String, ToolPin>>;

/// Pins kept in memory only (tests, hosts without a state directory).
#[derive(Debug, Default)]
pub struct MemoryPinStore(Mutex<Pins>);

impl PinStore for MemoryPinStore {
    fn get(&self, server: &str, tool: &str) -> Option<ToolPin> {
        self.0.lock().get(server)?.get(tool).cloned()
    }
    fn has_server(&self, server: &str) -> bool {
        self.0.lock().contains_key(server)
    }
    fn put(&self, server: &str, tool: &str, pin: ToolPin) {
        self.0
            .lock()
            .entry(server.to_owned())
            .or_default()
            .insert(tool.to_owned(), pin);
    }
    fn forget_server(&self, server: &str) {
        self.0.lock().remove(server);
    }
}

/// Pins persisted as a JSON file (write-then-rename). A missing file means no
/// pins yet; a file that exists but cannot be read or parsed fails closed:
/// every listed tool is treated as not approved until the owner approves it.
#[derive(Debug)]
pub struct FilePinStore {
    path: PathBuf,
    pins: Mutex<Pins>,
    unreadable: bool,
}

impl FilePinStore {
    /// Load `path` (missing or unreadable = no pins yet).
    #[must_use]
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (pins, unreadable) = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(pins) => (pins, false),
                Err(_) => (Pins::default(), true),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Pins::default(), false),
            Err(_) => (Pins::default(), true),
        };
        if unreadable {
            tracing::warn!("MCP tool pin file is unreadable; tools need approval again");
        }
        Self {
            path,
            pins: Mutex::new(pins),
            unreadable,
        }
    }

    fn persist(&self, pins: &Pins) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
                fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            fs::write(&tmp, serde_json::to_vec_pretty(pins)?)?;
            fs::rename(&tmp, &self.path)
        };
        if let Err(error) = write() {
            tracing::warn!(%error, "could not save MCP tool pins");
        }
    }
}

impl PinStore for FilePinStore {
    fn get(&self, server: &str, tool: &str) -> Option<ToolPin> {
        self.pins.lock().get(server)?.get(tool).cloned()
    }
    fn has_server(&self, server: &str) -> bool {
        self.unreadable || self.pins.lock().contains_key(server)
    }
    fn put(&self, server: &str, tool: &str, pin: ToolPin) {
        let mut pins = self.pins.lock();
        pins.entry(server.to_owned())
            .or_default()
            .insert(tool.to_owned(), pin);
        self.persist(&pins);
    }
    fn forget_server(&self, server: &str) {
        let mut pins = self.pins.lock();
        if pins.remove(server).is_some() {
            self.persist(&pins);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    fn tool(description: &str, schema: &Value) -> McpTool {
        McpTool::new(
            "echo",
            description.to_owned(),
            Arc::new(schema.as_object().cloned().unwrap()),
        )
    }

    #[test]
    fn hash_covers_description_schema_and_hidden_characters() {
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let base = pin_of(&tool("Say hello", &schema));
        // Key order of the schema does not matter.
        let reordered = json!({"properties": {"a": {"type": "string"}}, "type": "object"});
        assert_eq!(base.hash, pin_of(&tool("Say hello", &reordered)).hash);

        let text = change_of("echo", &base, pin_of(&tool("Say hello, then X", &schema)));
        assert!(text.description_changed && !text.schema_changed && !text.other_changed);

        let other_schema = json!({"type": "object", "properties": {"b": {}}});
        let sch = change_of("echo", &base, pin_of(&tool("Say hello", &other_schema)));
        assert!(sch.schema_changed && !sch.description_changed);

        // Only invisible characters differ: the visible text is the same, but
        // the tool is still held back and the card says "something else".
        let hidden = pin_of(&tool("Say hel\u{200B}lo", &schema));
        assert_ne!(base.hash, hidden.hash);
        let c = change_of("echo", &base, hidden);
        assert!(!c.description_changed && c.other_changed);
        assert_eq!(c.before, c.after);
    }
}

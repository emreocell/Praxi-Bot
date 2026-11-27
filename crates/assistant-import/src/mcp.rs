//! MCP servers from other apps' settings files.

use std::path::Path;

use mcp_bridge::McpTransport;
use serde_json::{Map, Value};

use crate::{
    App, Ctx, Item, Kind, MAX_CONFIG_BYTES, MAX_SERVERS_PER_FILE, Reason, SecretSlot, ServerImport,
};

/// Where the server map sits in a settings file.
#[derive(Clone, Copy)]
pub(crate) enum Shape {
    /// `{"mcpServers": {...}}` (Claude, Cursor).
    McpServers,
    /// `{"servers": {...}}` (VS Code `mcp.json`, which may carry comments).
    VsCode,
    /// The bare map, or wrapped in `mcpServers` (OpenClaw).
    Bare,
}

impl Ctx<'_> {
    /// Add every server of a JSON settings file.
    pub(crate) fn json_servers(&mut self, app: App, file: &Path, shape: Shape) {
        let source = self.roots.show(file);
        let text = match self.read_capped(file, MAX_CONFIG_BYTES) {
            Ok(t) => t,
            Err(why) => {
                self.unreadable(app, &source, &format!("the file {why}"));
                return;
            }
        };
        let text = if matches!(shape, Shape::VsCode) {
            strip_jsonc(&text)
        } else {
            text
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            self.unreadable(app, &source, "the file is not valid JSON");
            return;
        };
        let servers = match shape {
            Shape::McpServers => value.get("mcpServers"),
            Shape::VsCode => value
                .get("servers")
                .or_else(|| value.get("mcp").and_then(|m| m.get("servers"))),
            Shape::Bare => Some(value.get("mcpServers").unwrap_or(&value)),
        };
        let Some(map) = servers.and_then(Value::as_object) else {
            return;
        };
        for (name, entry) in map.iter().take(MAX_SERVERS_PER_FILE) {
            self.server(app, &source, name, entry.clone(), Vec::new());
        }
        self.too_many(app, &source, map.len());
    }

    /// Add every `[mcp_servers.<name>]` table of a Codex `config.toml`.
    pub(crate) fn codex_servers(&mut self, file: &Path) {
        let source = self.roots.show(file);
        let text = match self.read_capped(file, MAX_CONFIG_BYTES) {
            Ok(t) => t,
            Err(why) => {
                self.unreadable(App::Codex, &source, &format!("the file {why}"));
                return;
            }
        };
        let Ok(doc) = text.parse::<toml::Table>() else {
            self.unreadable(App::Codex, &source, "the file is not valid TOML");
            return;
        };
        let Some(servers) = doc.get("mcp_servers").and_then(toml::Value::as_table) else {
            return;
        };
        self.too_many(App::Codex, &source, servers.len());
        for (name, table) in servers.iter().take(MAX_SERVERS_PER_FILE) {
            let Some(table) = table.as_table() else {
                continue;
            };
            let (entry, dropped) = codex_entry(table);
            self.server(App::Codex, &source, name, entry, dropped);
        }
    }

    /// A note when a file holds more servers than are looked at.
    fn too_many(&mut self, app: App, source: &str, count: usize) {
        if count > MAX_SERVERS_PER_FILE {
            let note =
                format!("only the first {MAX_SERVERS_PER_FILE} of {count} servers are listed");
            self.other(app, source, source, Reason::NotImported, &note);
        }
    }

    fn unreadable(&mut self, app: App, source: &str, note: &str) {
        self.other(app, source, source, Reason::Unreadable, note);
    }

    /// Plan one server. `dropped` are names the caller already left out.
    fn server(
        &mut self,
        app: App,
        source: &str,
        name: &str,
        mut entry: Value,
        mut dropped: Vec<String>,
    ) {
        let item = Item::new(app, Kind::McpServer, name, source);
        // The same server in two files of one app is listed once.
        if self.plan.items.iter().any(|i| i.id == item.id) {
            return;
        }
        dropped.extend(strip_references(&mut entry));
        let mut item = item;
        item.dropped = dropped;

        let text = command_text(&entry);
        if text.contains("${") {
            let item = item.skipped(
                Reason::Variables,
                "the command or URL uses ${...} variables that Praxi-Bot does not fill in",
            );
            self.push(item);
            return;
        }
        if !secret_store::detect_secrets(&text).is_empty() {
            let item = item.skipped(
                Reason::SecretInCommand,
                "the command line or URL holds what looks like a key: add this server by hand so the key goes to the secret store",
            );
            self.push(item);
            return;
        }
        let mut parsed = match mcp_bridge::paste::parse_entry(name, &entry) {
            Ok(p) => p,
            Err(why) => {
                let reason = classify(&why);
                let item = item.skipped(reason, why);
                self.push(item);
                return;
            }
        };
        let id = parsed.config.id.to_ascii_lowercase();
        if self.taken_servers.contains(&id) {
            let item = item.skipped(
                Reason::AlreadyHave,
                "you already have a server with this name",
            );
            self.push(item);
            return;
        }
        if self
            .plan
            .servers
            .iter()
            .any(|s| s.parsed.config.id.eq_ignore_ascii_case(&id))
        {
            let item = item.skipped(
                Reason::Duplicate,
                "another server in this list has the same name",
            );
            self.push(item);
            return;
        }
        parsed.config.enabled = false;
        item.secrets = slots(&parsed);
        let (line, cut) = summary(&parsed);
        item.detail = Some(line);
        let n = item.secrets.len();
        let item = if n == 0 {
            item.ready(Reason::Ready, "arrives switched off")
        } else {
            item.ready(
                Reason::ReadyWithSecrets,
                format!("arrives switched off; {n} secret value(s) go to the secret store"),
            )
        };
        let mut item = item;
        if cut {
            // The owner cannot read all of it here, so it is not ticked for them.
            item.default_on = false;
            item.note
                .push_str("; the command line is longer than shown: read the file first");
        }
        let item_id = item.id.clone();
        self.push(item);
        self.plan.servers.push(ServerImport { item_id, parsed });
    }
}

fn classify(why: &str) -> Reason {
    if why.contains("never-list") {
        Reason::NeverList
    } else if why.contains("unpinned") || why.contains("not accepted") {
        Reason::Unpinned
    } else if why.contains("not supported") {
        Reason::Unsupported
    } else {
        Reason::Invalid
    }
}

fn slots(parsed: &mcp_bridge::paste::Parsed) -> Vec<SecretSlot> {
    let cfg = &parsed.config;
    let env = cfg.secret_env.iter().map(|(key, name)| SecretSlot {
        name: name.clone(),
        key: key.clone(),
        place: "env",
    });
    let headers = cfg.secret_headers.iter().map(|(key, name)| SecretSlot {
        name: name.clone(),
        key: key.clone(),
        place: "header",
    });
    env.chain(headers).collect()
}

/// Longest command line or URL shown, in characters.
const MAX_COMMAND_CHARS: usize = 1_000;

/// The command line (command and every argument) or the URL exactly as the file has it, cut
/// after [`MAX_COMMAND_CHARS`] characters with a visible `…`. The second value says it was cut.
/// What the owner switches on is this text, so no flag or argument is left out; an argument that
/// holds a space or a control character is quoted so it cannot pass for two. Values of `env` and
/// `headers` are not part of it. The command line was already checked for keys and `${...}`.
fn summary(parsed: &mcp_bridge::paste::Parsed) -> (String, bool) {
    let line = match &parsed.config.transport {
        McpTransport::Stdio { command, args, .. } => std::iter::once(command)
            .chain(args)
            .map(|part| quote(part))
            .collect::<Vec<_>>()
            .join(" "),
        McpTransport::Http { url, .. } => url.clone(),
    };
    match line.char_indices().nth(MAX_COMMAND_CHARS) {
        Some((end, _)) => (format!("{}…", &line[..end]), true),
        None => (line, false),
    }
}

/// `part` as it is, or in JSON quotes when it is empty or has whitespace, quotes or control
/// characters.
fn quote(part: &str) -> String {
    let plain = !part.is_empty()
        && !part
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\'');
    if plain {
        part.to_owned()
    } else {
        serde_json::Value::from(part).to_string()
    }
}

/// The command, arguments and URL of an entry as one text (to look for variables and keys in).
fn command_text(entry: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["command", "url", "serverUrl"] {
        if let Some(s) = entry.get(key).and_then(Value::as_str) {
            parts.push(s.to_owned());
        }
    }
    if let Some(args) = entry.get("args").and_then(Value::as_array) {
        parts.extend(args.iter().filter_map(|a| a.as_str().map(str::to_owned)));
    }
    parts.join(" ")
}

/// Remove `env` / `headers` values that are only a variable reference (`${env:TOKEN}`): they
/// hold no secret, and the literal text would be wrong. Returns the names removed.
fn strip_references(entry: &mut Value) -> Vec<String> {
    let mut dropped = Vec::new();
    for field in ["env", "headers"] {
        if let Some(map) = entry.get_mut(field).and_then(Value::as_object_mut) {
            map.retain(|key, value| {
                let reference = value.as_str().is_some_and(|v| v.contains("${"));
                if reference {
                    dropped.push(key.clone());
                }
                !reference
            });
        }
    }
    dropped
}

/// A Codex `[mcp_servers.x]` table as the JSON shape of the other apps, plus the header names
/// that take their value from an environment variable (not copied).
fn codex_entry(table: &toml::Table) -> (Value, Vec<String>) {
    let mut entry = Map::new();
    let mut dropped = Vec::new();
    let strings = |v: Option<&toml::Value>| -> Map<String, Value> {
        v.and_then(toml::Value::as_table)
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), Value::from(s))))
                    .collect()
            })
            .unwrap_or_default()
    };
    if let Some(url) = table.get("url").and_then(toml::Value::as_str) {
        entry.insert("url".into(), url.into());
        let headers = strings(table.get("http_headers"));
        if !headers.is_empty() {
            entry.insert("headers".into(), Value::Object(headers));
        }
        if table.contains_key("bearer_token_env_var") {
            dropped.push("Authorization".to_owned());
        }
        dropped.extend(
            strings(table.get("env_http_headers"))
                .into_iter()
                .map(|(k, _)| k),
        );
    } else {
        if let Some(command) = table.get("command").and_then(toml::Value::as_str) {
            entry.insert("command".into(), command.into());
        }
        if let Some(args) = table.get("args").and_then(toml::Value::as_array) {
            let args: Vec<Value> = args
                .iter()
                .filter_map(|a| a.as_str().map(Value::from))
                .collect();
            entry.insert("args".into(), Value::Array(args));
        }
        let env = strings(table.get("env"));
        if !env.is_empty() {
            entry.insert("env".into(), Value::Object(env));
        }
    }
    (Value::Object(entry), dropped)
}

/// JSON with `//` and `/* */` comments and trailing commas removed (VS Code's `mcp.json`).
pub(crate) fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if c == ',' {
            // A trailing comma: the next significant character closes an object or array.
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

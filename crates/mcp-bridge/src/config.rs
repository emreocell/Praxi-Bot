//! Declarative MCP server configuration and its persistence.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{McpError, Result};

fn default_true() -> bool {
    true
}

/// How to reach an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Spawn a child process and speak MCP over its stdin/stdout.
    Stdio {
        /// Executable (resolved through the *parent's* `PATH`).
        command: String,
        /// Arguments.
        #[serde(default)]
        args: Vec<String>,
        /// The **only** environment variables the child receives (plus a tiny
        /// OS-essential set, see [`crate::OS_ESSENTIAL_ENV`]).  ADR-0004
        /// isolation: nothing is inherited implicitly.
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Working directory.
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    /// Streamable HTTP endpoint.
    Http {
        /// Endpoint URL.
        url: String,
        /// Extra request headers (e.g. `Authorization`).
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// One configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Stable id; becomes the `<server>` part of `mcp:<server>/<tool>`.
    pub id: String,
    /// Transport.
    pub transport: McpTransport,
    /// Disabled servers are kept in the config but never connected.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Glob patterns over the server-side tool name; empty = allow all.
    #[serde(default)]
    pub tool_allow: Vec<String>,
    /// Glob patterns over the server-side tool name; applied after the allow list.
    #[serde(default)]
    pub tool_exclude: Vec<String>,
}

impl McpServerConfig {
    /// A stdio server with defaults (enabled, no filters, empty env).
    #[must_use]
    pub fn stdio(
        id: impl Into<String>,
        command: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            transport: McpTransport::Stdio {
                command: command.into(),
                args: args.into_iter().map(Into::into).collect(),
                env: BTreeMap::new(),
                cwd: None,
            },
            enabled: true,
            tool_allow: Vec::new(),
            tool_exclude: Vec::new(),
        }
    }

    /// A streamable-HTTP server with defaults.
    #[must_use]
    pub fn http(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            transport: McpTransport::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
            enabled: true,
            tool_allow: Vec::new(),
            tool_exclude: Vec::new(),
        }
    }

    /// Check the config for obvious mistakes.
    ///
    /// # Errors
    /// [`McpError::Config`] describing the first problem found.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(McpError::Config(m));
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return bad(format!(
                "server id `{}` must be non-empty and use only [A-Za-z0-9._-]",
                self.id
            ));
        }
        match &self.transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                return bad("stdio `command` is empty".into());
            }
            McpTransport::Http { url, .. }
                if !(url.starts_with("http://") || url.starts_with("https://")) =>
            {
                return bad(format!("http `url` must start with http(s)://: `{url}`"));
            }
            _ => {}
        }
        self.tool_filter().map(|_| ())
    }

    /// Compile `tool_allow` / `tool_exclude`.
    pub(crate) fn tool_filter(&self) -> Result<ToolFilter> {
        let build = |patterns: &[String]| -> Result<GlobSet> {
            let mut b = GlobSetBuilder::new();
            for p in patterns {
                b.add(
                    Glob::new(p)
                        .map_err(|e| McpError::Config(format!("bad tool glob `{p}`: {e}")))?,
                );
            }
            b.build()
                .map_err(|e| McpError::Config(format!("bad tool globs: {e}")))
        };
        Ok(ToolFilter {
            allow_all: self.tool_allow.is_empty(),
            allow: build(&self.tool_allow)?,
            exclude: build(&self.tool_exclude)?,
        })
    }
}

/// Compiled allow/exclude lists.
pub(crate) struct ToolFilter {
    allow_all: bool,
    allow: GlobSet,
    exclude: GlobSet,
}

impl ToolFilter {
    pub(crate) fn permits(&self, tool: &str) -> bool {
        (self.allow_all || self.allow.is_match(tool)) && !self.exclude.is_match(tool)
    }
}

/// Persistence boundary for server configs.  A `storage`-backed implementation
/// can be added without touching the bridge; [`TomlFileStore`] is the default.
pub trait McpConfigStore: Send + Sync {
    /// Load all configured servers (empty if nothing was saved yet).
    ///
    /// # Errors
    /// [`McpError::Store`] on I/O or parse failure; [`McpError::Config`] if an
    /// entry is invalid.
    fn load(&self) -> Result<Vec<McpServerConfig>>;
    /// Replace the persisted configuration.
    ///
    /// # Errors
    /// [`McpError::Store`] on I/O failure.
    fn save(&self, servers: &[McpServerConfig]) -> Result<()>;
}

#[derive(Serialize, Deserialize, Default)]
struct FileLayout {
    #[serde(default, rename = "server")]
    servers: Vec<McpServerConfig>,
}

/// Stores the configuration as a TOML file with one `[[server]]` table per server.
#[derive(Debug, Clone)]
pub struct TomlFileStore {
    path: PathBuf,
}

impl TomlFileStore {
    /// Use `path` (created on first save, along with its parent directories).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The backing file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl McpConfigStore for TomlFileStore {
    fn load(&self) -> Result<Vec<McpServerConfig>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(McpError::Store(format!("{}: {e}", self.path.display()))),
        };
        let layout: FileLayout = toml::from_str(&text)
            .map_err(|e| McpError::Store(format!("{}: {e}", self.path.display())))?;
        let mut seen = std::collections::HashSet::new();
        for s in &layout.servers {
            s.validate()?;
            if !seen.insert(s.id.as_str()) {
                return Err(McpError::Config(format!("duplicate server id `{}`", s.id)));
            }
        }
        Ok(layout.servers)
    }

    fn save(&self, servers: &[McpServerConfig]) -> Result<()> {
        let text = toml::to_string_pretty(&FileLayout {
            servers: servers.to_vec(),
        })
        .map_err(|e| McpError::Store(e.to_string()))?;
        let io = |e: std::io::Error| McpError::Store(format!("{}: {e}", self.path.display()));
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(io)?;
        }
        // Write-then-rename so a crash never leaves a half-written file.
        let tmp = self.path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(io)?;
        fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_ARGS: [&str; 0] = [];

    fn sample() -> Vec<McpServerConfig> {
        let mut a = McpServerConfig::stdio("fs", "npx", ["-y", "server-fs"]);
        if let McpTransport::Stdio { env, cwd, .. } = &mut a.transport {
            env.insert("TOKEN".into(), "x".into());
            *cwd = Some(PathBuf::from("work"));
        }
        a.tool_allow = vec!["read_*".into()];
        a.tool_exclude = vec!["read_secret".into()];
        let mut b = McpServerConfig::http("web", "https://example.com/mcp");
        b.enabled = false;
        if let McpTransport::Http { headers, .. } = &mut b.transport {
            headers.insert("Authorization".into(), "Bearer t".into());
        }
        vec![a, b]
    }

    #[test]
    fn toml_roundtrip_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = TomlFileStore::new(dir.path().join("nested/mcp.toml"));
        assert_eq!(store.load().unwrap().len(), 0);
        store.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), sample());
    }

    #[test]
    fn defaults_apply_when_fields_are_omitted() {
        let cfg: FileLayout = toml::from_str(
            "[[server]]\nid = \"a\"\n[server.transport]\ntype = \"stdio\"\ncommand = \"x\"\n",
        )
        .unwrap();
        let s = &cfg.servers[0];
        assert!(s.enabled && s.tool_allow.is_empty() && s.tool_exclude.is_empty());
    }

    #[test]
    fn validation_rejects_bad_configs() {
        assert!(
            McpServerConfig::stdio("a/b", "x", NO_ARGS)
                .validate()
                .is_err()
        );
        assert!(McpServerConfig::stdio("", "x", NO_ARGS).validate().is_err());
        assert!(
            McpServerConfig::stdio("a", " ", NO_ARGS)
                .validate()
                .is_err()
        );
        assert!(McpServerConfig::http("a", "ftp://x").validate().is_err());
        let mut c = McpServerConfig::stdio("a", "x", NO_ARGS);
        c.tool_allow = vec!["[".into()];
        assert!(c.validate().is_err());
        assert!(
            McpServerConfig::stdio("a-b_c.1", "x", NO_ARGS)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn duplicate_ids_are_rejected_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = TomlFileStore::new(dir.path().join("m.toml"));
        let one = McpServerConfig::stdio("a", "x", NO_ARGS);
        store.save(&[one.clone(), one]).unwrap();
        assert!(matches!(store.load(), Err(McpError::Config(_))));
    }

    #[test]
    fn filter_applies_allow_then_exclude() {
        let mut c = McpServerConfig::stdio("a", "x", NO_ARGS);
        assert!(c.tool_filter().unwrap().permits("anything"));
        c.tool_allow = vec!["read_*".into(), "list".into()];
        c.tool_exclude = vec!["read_secret".into()];
        let f = c.tool_filter().unwrap();
        assert!(f.permits("read_file") && f.permits("list"));
        assert!(!f.permits("read_secret") && !f.permits("write_file"));
    }
}

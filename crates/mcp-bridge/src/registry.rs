//! Read-only search of the official MCP Registry
//! (<https://github.com/modelcontextprotocol/registry>, Apache-2.0 for new
//! code, MIT for older; see `THIRD_PARTY_NOTICES.md`).
//!
//! Only the public `GET /v0/servers` endpoint and the documented `server.json`
//! shape are used; no upstream code is copied. Registry entries are written by
//! anyone, so everything here is untrusted text: [`search`] only lists,
//! [`RegistryEntry::to_config`] builds a config that still has to go through
//! the owner's approval card, and an unpinned package is refused.

use std::{collections::BTreeMap, time::Duration};

use secret_store::{SecretRef, SecretStore};
use serde_json::Value;

use crate::{
    catalog::check_stdio_package_pin,
    config::McpServerConfig,
    error::{McpError, Result},
    mcpb::save_secrets,
};

/// The only host this module talks to.
pub const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 500;

/// Registry text is written by anyone: drop control characters (including
/// newlines and bidi/zero-width marks) and cap the length.
fn clean_text(text: &str) -> String {
    let flat: String = text
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200B}'..='\u{200F}'
                        | '\u{202A}'..='\u{202E}'
                        | '\u{2060}'..='\u{2064}'
                        | '\u{FEFF}'
                )
        })
        .collect();
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
    if u.scheme() != "https" {
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
            !(d == "localhost" || local(".localhost") || local(".local"))
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `npx -y <identifier>@<version>`
    Npm {
        /// Package name.
        identifier: String,
        /// Exact version.
        version: String,
        /// Variables to provide.
        env: Vec<Needed>,
    },
    /// `uvx <identifier>==<version>`
    Pypi {
        /// Package name.
        identifier: String,
        /// Exact version.
        version: String,
        /// Variables to provide.
        env: Vec<Needed>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let source = self
            .sources
            .get(index)
            .ok_or_else(|| err("no such way to run this server"))?;
        let id = id_of(&self.name)?;
        let (mut config, needs, is_header) = match source {
            Source::Npm {
                identifier,
                version,
                env,
            } => (
                McpServerConfig::stdio(
                    id.clone(),
                    "npx",
                    ["-y".to_string(), format!("{identifier}@{version}")],
                ),
                env,
                false,
            ),
            Source::Pypi {
                identifier,
                version,
                env,
            } => (
                McpServerConfig::stdio(id.clone(), "uvx", [format!("{identifier}=={version}")]),
                env,
                false,
            ),
            Source::Remote { url, headers } => {
                if !public_https(url) {
                    return Err(err("only https servers on a public address are accepted"));
                }
                (
                    McpServerConfig::http(id.clone(), url.clone()),
                    headers,
                    true,
                )
            }
        };
        if let crate::McpTransport::Stdio { command, args, .. } = &config.transport {
            check_stdio_package_pin(&id, command, args)?;
        }
        let mut pending: Vec<(String, String)> = Vec::new();
        for need in needs {
            let value = values.get(&need.name).filter(|v| !v.is_empty());
            let Some(value) = value else {
                if need.required {
                    return Err(err(format!("please fill in {}", need.name)));
                }
                continue;
            };
            if need.secret {
                let sname = format!("mcp/{id}/{}", need.name.to_lowercase());
                SecretRef::new(sname.as_str())
                    .map_err(|_| err(format!("`{}` is not a usable name", need.name)))?;
                pending.push((sname.clone(), value.clone()));
                if is_header {
                    config.secret_headers.insert(need.name.clone(), sname);
                } else {
                    config.secret_env.insert(need.name.clone(), sname);
                }
            } else if let crate::McpTransport::Stdio { env, .. } = &mut config.transport {
                env.insert(need.name.clone(), value.clone());
            } else if let crate::McpTransport::Http { headers, .. } = &mut config.transport {
                headers.insert(need.name.clone(), value.clone());
            }
        }
        config.validate()?;
        // Only now, with everything checked, touch the secret store.
        save_secrets(secrets, &pending)?;
        Ok(config)
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

fn needed(list: Option<&Value>) -> Vec<Needed> {
    list.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some(Needed {
                        name: v.get("name")?.as_str()?.to_string(),
                        description: clean_text(
                            v.get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                        secret: v.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                        required: v
                            .get("isRequired")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a `GET /v0/servers` response. Entries Praxi cannot run (OCI images,
/// unpinned packages, non-https remotes) keep no sources and sort last.
///
/// # Errors
/// [`McpError::Config`] when the body is not the expected JSON.
#[allow(clippy::too_many_lines)]
pub fn parse_search(body: &[u8]) -> Result<Vec<RegistryEntry>> {
    let root: Value =
        serde_json::from_slice(body).map_err(|_| err("the registry answer is not readable"))?;
    let list = root
        .get("servers")
        .and_then(Value::as_array)
        .ok_or_else(|| err("the registry answer has no server list"))?;
    let mut out = Vec::new();
    for item in list {
        // Current shape wraps `{server, _meta}`; older answers are flat.
        let s = item.get("server").unwrap_or(item);
        let Some(name) = s.get("name").and_then(Value::as_str) else {
            continue;
        };
        let text = |k: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let status = s
            .get("status")
            .and_then(Value::as_str)
            .or_else(|| {
                item.pointer("/_meta/io.modelcontextprotocol.registry~1official/status")
                    .and_then(Value::as_str)
            })
            .unwrap_or("active");
        let mut sources = Vec::new();
        for p in s
            .get("packages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(kind), Some(identifier), Some(version)) = (
                p.get("registryType").and_then(Value::as_str),
                p.get("identifier").and_then(Value::as_str),
                p.get("version").and_then(Value::as_str),
            ) else {
                continue;
            };
            let stdio = p
                .pointer("/transport/type")
                .and_then(Value::as_str)
                .is_none_or(|t| t == "stdio");
            let exact = !version.is_empty()
                && version != "latest"
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
            if !stdio || !exact {
                continue;
            }
            let env = needed(p.get("environmentVariables"));
            let (identifier, version) = (identifier.to_string(), version.to_string());
            match kind {
                "npm" => sources.push(Source::Npm {
                    identifier,
                    version,
                    env,
                }),
                "pypi" => sources.push(Source::Pypi {
                    identifier,
                    version,
                    env,
                }),
                _ => {}
            }
        }
        for r in s
            .get("remotes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let url = r.get("url").and_then(Value::as_str);
            let kind = r.get("type").and_then(Value::as_str);
            if let (Some(url), Some("streamable-http")) = (url, kind)
                && public_https(url)
            {
                sources.push(Source::Remote {
                    url: url.to_string(),
                    headers: needed(r.get("headers")),
                });
            }
        }
        out.push(RegistryEntry {
            name: name.to_string(),
            description: clean_text(&text("description")),
            version: text("version"),
            repository: s
                .pointer("/repository/url")
                .and_then(Value::as_str)
                .map(str::to_string),
            sources,
            active: status == "active",
        });
    }
    out.sort_by(|a, b| b.rank().cmp(&a.rank()).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// Search the registry (read-only). `query` is URL-encoded here.
///
/// # Errors
/// [`McpError::Transport`] when the registry cannot be reached or answers with
/// something unusable or too large.
///
/// # Panics
/// Never in practice: the URL is a compile-time constant.
pub async fn search(query: &str, limit: u32) -> Result<Vec<RegistryEntry>> {
    let fail = |m: &str| McpError::Transport {
        server: "registry".into(),
        message: m.to_string(),
    };
    let mut url = url::Url::parse(&format!("{REGISTRY_BASE}/v0/servers")).expect("static url");
    url.query_pairs_mut()
        .append_pair("search", query)
        .append_pair("limit", &limit.clamp(1, 50).to_string())
        .append_pair("version", "latest");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| fail("cannot start the request"))?;
    let resp = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| fail("the MCP registry cannot be reached"))?;
    if !resp.status().is_success() {
        return Err(fail("the MCP registry answered with an error"));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|_| fail("the MCP registry answer was interrupted"))?;
    if body.len() > MAX_RESPONSE {
        return Err(fail("the MCP registry answer is too large"));
    }
    parse_search(&body)
}

#[cfg(test)]
mod tests {
    use secret_store::MemoryStore;

    use super::*;

    const BODY: &str = r#"{"servers":[
      {"server":{"name":"io.github.z/plain","description":"d","version":"1.0.0"},"_meta":{}},
      {"server":{"name":"io.github.a/maps","description":"Maps","version":"2.1.0",
        "repository":{"url":"https://github.com/a/maps"},
        "packages":[{"registryType":"npm","identifier":"@a/maps-mcp","version":"2.1.0",
          "transport":{"type":"stdio"},
          "environmentVariables":[{"name":"MAPS_KEY","isSecret":true,"isRequired":true}]},
          {"registryType":"npm","identifier":"@a/floating","version":"latest","transport":{"type":"stdio"}},
          {"registryType":"oci","identifier":"x","version":"1","transport":{"type":"stdio"}}]},"_meta":{}}]}"#;

    #[test]
    fn ranks_checkable_entries_first_and_drops_unpinned_sources() {
        let list = parse_search(BODY.as_bytes()).unwrap();
        assert_eq!(list[0].name, "io.github.a/maps");
        assert_eq!(list[0].sources.len(), 1);
        assert_eq!(list[1].sources.len(), 0);
    }

    #[test]
    fn secret_goes_to_store_and_package_is_pinned() {
        let list = parse_search(BODY.as_bytes()).unwrap();
        let store = MemoryStore::new();
        let none = BTreeMap::new();
        assert!(list[0].to_config(0, &none, &store).is_err());
        let vals = BTreeMap::from([("MAPS_KEY".to_string(), "abc123".to_string())]);
        let cfg = list[0].to_config(0, &vals, &store).unwrap();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("@a/maps-mcp@2.1.0") && !json.contains("abc123"));
        assert_eq!(
            store
                .get("mcp/io.github.a-maps/maps_key")
                .unwrap()
                .unwrap()
                .expose(),
            "abc123"
        );
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
          "packages":[{"registryType":"npm","identifier":"p","version":"1.0.0",
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
        assert!(store.inner.get("mcp/a-two/k1").unwrap().is_none());
    }
}

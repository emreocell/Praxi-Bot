//! `bot mcp add <paste>`: turn what MCP server READMEs tell users to copy into
//! server configs.  Four inputs are understood:
//!
//! 1. a bare server URL (`https://mcp.example.com/mcp`): streamable HTTP;
//! 2. a `claude mcp add ...` line (stdio after `--`, or `--transport http`);
//! 3. an `npx` / `uvx` / ... launch line, optionally after `KEY=value` pairs;
//! 4. JSON: `{"mcpServers": {...}}`, `{"<name>": {...}}` or one server object.
//!
//! Values found in the paste (`-e KEY=value`, `--header "Authorization: ..."`,
//! `"env": {...}`) are never written to the config: each becomes a secret
//! reference (`secret_env` / `secret_headers`) and the value is returned in
//! [`Parsed::secrets`] for the caller to put into the secret store first.
//! Every result passes [`McpServerConfig::validate`] (stdio commands on the
//! shell never-list are refused) and, for stdio, the catalog's exact-version
//! rule ([`mcp_bridge::check_stdio_package_pin`]).

use std::collections::BTreeMap;

use mcp_bridge::{McpServerConfig, McpTransport};
use serde_json::{Value, json};

/// One server read from the paste.
#[derive(Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The config to create (secret references only).
    pub config: McpServerConfig,
    /// `(secret name, value)` pairs to store before creating the server.
    pub secrets: Vec<(String, String)>,
}

impl std::fmt::Debug for Parsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parsed")
            .field("config", &self.config)
            .field(
                "secrets",
                &self.secrets.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .finish()
    }
}

const FORMATS: &str = "paste a server URL, a `claude mcp add ...` line, an `npx`/`uvx` command line, or MCP JSON (`{\"mcpServers\": {...}}`)";

/// Parse `input`; `name` overrides the derived server name (single server only).
///
/// # Errors
/// Unrecognised input, an unsupported transport, a refused or unpinned stdio
/// command, or an invalid name.
pub fn parse(input: &str, name: Option<&str>) -> Result<Vec<Parsed>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(format!("nothing to add: {FORMATS}"));
    }
    let mut parsed = if input.starts_with('{') {
        parse_json(input)?
    } else if is_url(input) && !input.contains(char::is_whitespace) {
        vec![http_server(&name_from_url(input)?, input, Vec::new())?]
    } else {
        let words = split_words(input)?;
        match words.as_slice() {
            [claude, mcp, add, rest @ ..] if claude == "claude" && mcp == "mcp" && add == "add" => {
                vec![parse_claude(rest)?]
            }
            _ => vec![parse_launch(&words)?],
        }
    };
    if let Some(name) = name {
        let [one] = parsed.as_mut_slice() else {
            return Err("--name applies to a paste with exactly one server".into());
        };
        rename(one, name)?;
    }
    for p in &parsed {
        check(&p.config)?;
    }
    Ok(parsed)
}

/// [`parse`] for command-line arguments: one argument is the paste itself
/// (quoted by the user); several are a line the shell already split, used
/// as-is so quoting inside it is not lost.
///
/// # Errors
/// As [`parse`].
pub fn parse_argv(args: &[String], name: Option<&str>) -> Result<Vec<Parsed>, String> {
    match args {
        [one] => parse(one, name),
        [first, ..] if first.trim_start().starts_with('{') || is_url(first) => {
            parse(&args.join(" "), name)
        }
        words => {
            let mut parsed = match words {
                [claude, mcp, add, rest @ ..]
                    if claude == "claude" && mcp == "mcp" && add == "add" =>
                {
                    vec![parse_claude(rest)?]
                }
                _ => vec![parse_launch(words)?],
            };
            if let Some(name) = name {
                rename(&mut parsed[0], name)?;
            }
            for p in &parsed {
                check(&p.config)?;
            }
            Ok(parsed)
        }
    }
}

/// `bot mcp add`: create the server, store each pasted value in the secret
/// store (through the daemon, write-only), then enable it; the daemon
/// connects it and answers with its state.
///
/// Staged so a refused add never touches a live credential (review finding
/// 8): a name already in use (in any letter case) is refused before anything
/// is written; the server is created **disabled** first (a 409 / 422 writes
/// no secret); a failed secret write removes the new server again.
///
/// # Errors
/// The daemon refusing a secret or the server (exit 6), or unreachable.
pub async fn add(
    c: &crate::client::Client,
    parsed: &[Parsed],
    dry_run: bool,
    json_out: bool,
    out: &mut dyn std::io::Write,
) -> crate::Result<()> {
    let mut results = Vec::new();
    for p in parsed {
        let body = create_body(&p.config);
        if dry_run {
            let secrets: Vec<&str> = p.secrets.iter().map(|(n, _)| n.as_str()).collect();
            results.push(json!({"would_create": body, "would_store_secrets": secrets}));
            continue;
        }
        let existing = c
            .request(reqwest::Method::GET, "/v1/mcp/servers", None)
            .await?;
        let taken = existing.as_array().is_some_and(|servers| {
            servers.iter().any(|s| {
                s["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&p.config.id))
            })
        });
        if taken {
            return Err(crate::email_cmd::rejected(
                "a server with this name already exists; choose another with --name",
            ));
        }
        if p.secrets.is_empty() {
            results.push(
                c.request(reqwest::Method::POST, "/v1/mcp/servers", Some(body))
                    .await?,
            );
            continue;
        }
        let mut disabled = body;
        disabled["enabled"] = json!(false);
        c.request(reqwest::Method::POST, "/v1/mcp/servers", Some(disabled))
            .await?;
        let path = format!("/v1/mcp/servers/{}", p.config.id);
        for (secret, value) in &p.secrets {
            let stored = c
                .request(
                    reqwest::Method::POST,
                    &format!("/v1/secrets/{secret}"),
                    Some(json!({ "value": value })),
                )
                .await;
            if let Err(error) = stored {
                let _ = c.request(reqwest::Method::DELETE, &path, None).await;
                return Err(error);
            }
        }
        results.push(
            c.request(
                reqwest::Method::PUT,
                &path,
                Some(json!({ "enabled": p.config.enabled })),
            )
            .await?,
        );
    }
    if json_out {
        writeln!(out, "{}", serde_json::to_string_pretty(&results)?)?;
        return Ok(());
    }
    for r in &results {
        if let Some(body) = r.get("would_create") {
            writeln!(
                out,
                "would add {} ({}); secrets to store: {}",
                body["name"].as_str().unwrap_or("?"),
                body["transport"].as_str().unwrap_or("?"),
                r["would_store_secrets"]
            )?;
            continue;
        }
        let name = r["name"].as_str().unwrap_or("?");
        let status = r["status"].as_str().unwrap_or("?");
        writeln!(out, "added {name}: {status}")?;
        match status {
            "auth_required" => writeln!(
                out,
                "  {name} needs a sign-in: open Apps in the desktop app and choose Sign in"
            )?,
            "error" | "quarantined" => {
                if let Some(e) = r["error"].as_str() {
                    writeln!(out, "  {e}")?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The daemon's `POST /v1/mcp/servers` body for `cfg`.
#[must_use]
pub fn create_body(cfg: &McpServerConfig) -> Value {
    let mut body = json!({
        "name": cfg.id,
        "enabled": cfg.enabled,
        "secret_env": cfg.secret_env,
        "secret_headers": cfg.secret_headers,
    });
    match &cfg.transport {
        McpTransport::Stdio { command, args, .. } => {
            body["transport"] = json!("stdio");
            body["command"] = json!(command);
            body["args"] = json!(args);
        }
        McpTransport::Http { url, .. } => {
            body["transport"] = json!("http");
            body["url"] = json!(url);
        }
    }
    body
}

fn check(cfg: &McpServerConfig) -> Result<(), String> {
    cfg.validate().map_err(|e| without_paste(&e.to_string()))?;
    if let McpTransport::Stdio { command, args, .. } = &cfg.transport {
        mcp_bridge::check_stdio_package_pin(&cfg.id, command, args)
            .map_err(|e| without_paste(&e.to_string()))?;
    }
    Ok(())
}

/// A validation failure as a fixed message (review finding 12): the bridge's
/// texts quote URLs, command lines and package specs, and any of those can
/// carry a token the user pasted.
fn without_paste(message: &str) -> String {
    if message.contains("never-list") {
        "the command is on the shell never-list and will not be run".into()
    } else if message.contains("unpinned")
        || message.contains("@latest")
        || message.contains("no package argument")
    {
        "the package is unpinned: use an exact version (`pkg@1.2.3` or `pkg==1.2.3`), never @latest or a floating install".into()
    } else if message.contains("is not accepted") || message.contains("is only accepted as") {
        "this runner form or option is not accepted (it could run or install something unpinned)"
            .into()
    } else if message.contains("server id") {
        "the server name must be 1 to 100 characters of A-Z, a-z, 0-9, `.`, `_` or `-`".into()
    } else if message.contains("managed OAuth record") {
        "a pasted value may not name an MCP sign-in record".into()
    } else {
        "the pasted server configuration is not valid".into()
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// Which config map a pasted value goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Env,
    Header,
}

/// Secret-store name for a pasted value (review finding 8):
/// `mcp/<server>/env/<KEY>` or `mcp/<server>/header/<name>`.  Nothing is
/// folded or replaced, so distinct servers and keys never share a secret
/// (`a-b` vs `a_b`, `X_A` vs `X-A`); header names are lower-cased because
/// HTTP treats them case-insensitively.  Never a managed OAuth name.
fn secret_name(server: &str, kind: Kind, key: &str) -> String {
    match kind {
        Kind::Env => format!("mcp/{server}/env/{key}"),
        Kind::Header => format!("mcp/{server}/header/{}", key.to_ascii_lowercase()),
    }
}

/// Config key -> secret name.
type SecretRefs = BTreeMap<String, String>;

/// Map `values` (pasted `KEY=value` / headers) to secret references.
fn to_secrets(
    server: &str,
    kind: Kind,
    values: Vec<(String, String)>,
) -> Result<(SecretRefs, Vec<(String, String)>), String> {
    let mut refs = BTreeMap::new();
    let mut secrets = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in values {
        // Windows environments and HTTP headers are case-insensitive.
        if !seen.insert(key.to_ascii_lowercase()) {
            return Err(match kind {
                Kind::Env => "the paste sets the same environment variable twice".into(),
                Kind::Header => "the paste sets the same header twice".into(),
            });
        }
        let reference = secret_name(server, kind, &key);
        if secret_store::validate_name(&reference).is_err() {
            return Err("a server name and key are too long to keep as a secret".into());
        }
        refs.insert(key, reference.clone());
        secrets.push((reference, value));
    }
    Ok((refs, secrets))
}

/// Keep `[A-Za-z0-9._-]`, collapse the rest to `-`.
fn clean_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches(|c| matches!(c, '-' | '.')).to_owned()
}

/// `https://mcp.linear.app/mcp` → `linear`; `https://api.example.com/x` → `example`.
fn name_from_url(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|_| "not a valid URL".to_owned())?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "the URL has no host".to_owned())?;
    if host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') {
        return Ok(clean_name(host));
    }
    let labels: Vec<&str> = host.split('.').collect();
    let meaningful = labels[..labels.len() - 1]
        .iter()
        .find(|l| !matches!(**l, "mcp" | "www" | "api" | "server"))
        .unwrap_or(&labels[0]);
    Ok(clean_name(meaningful))
}

/// `@scope/server-github@1.2.3` → `github`; `mcp-server-fetch==0.6.2` → `fetch`.
fn name_from_package(spec: &str) -> String {
    let unversioned = spec.strip_prefix('@').map_or_else(
        || spec.split(['@', '=']).next().unwrap_or(spec).to_owned(),
        |rest| format!("@{}", rest.split(['@', '=']).next().unwrap_or(rest)),
    );
    let base = unversioned.rsplit('/').next().unwrap_or(&unversioned);
    let mut name = base;
    for prefix in ["mcp-server-", "server-", "mcp-"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            name = rest;
            break;
        }
    }
    for suffix in ["-mcp-server", "-mcp", "-server"] {
        if let Some(rest) = name.strip_suffix(suffix) {
            name = rest;
            break;
        }
    }
    // `@playwright/mcp`: the scope names the server better than `mcp`.
    let scope = unversioned
        .strip_prefix('@')
        .and_then(|rest| rest.split_once('/'))
        .map(|(scope, _)| scope);
    match (name, scope) {
        ("" | "mcp" | "server" | "mcp-server", Some(scope)) => clean_name(scope),
        ("", None) => clean_name(base),
        _ => clean_name(name),
    }
}

fn rename(p: &mut Parsed, name: &str) -> Result<(), String> {
    if p.config.id == name {
        return Ok(());
    }
    let values: BTreeMap<String, String> = p.secrets.drain(..).collect();
    let mut secrets = Vec::new();
    for (kind, map) in [
        (Kind::Env, &mut p.config.secret_env),
        (Kind::Header, &mut p.config.secret_headers),
    ] {
        let pasted: Vec<(String, String)> = map
            .iter()
            .map(|(key, old)| (key.clone(), values.get(old).cloned().unwrap_or_default()))
            .collect();
        let (refs, mut stored) = to_secrets(name, kind, pasted)?;
        *map = refs;
        secrets.append(&mut stored);
    }
    p.secrets = secrets;
    name.clone_into(&mut p.config.id);
    Ok(())
}

fn http_server(name: &str, url: &str, headers: Vec<(String, String)>) -> Result<Parsed, String> {
    if !is_url(url) {
        return Err("an HTTP server needs an http(s):// URL".into());
    }
    let mut config = McpServerConfig::http(name, url);
    let (refs, secrets) = to_secrets(name, Kind::Header, headers)?;
    config.secret_headers = refs;
    Ok(Parsed { config, secrets })
}

fn stdio_server(
    name: &str,
    command: &str,
    args: &[String],
    env: Vec<(String, String)>,
) -> Result<Parsed, String> {
    if env.iter().any(|(key, _)| !is_env_key(key)) {
        return Err("an environment variable name is not valid".into());
    }
    let mut config = McpServerConfig::stdio(name, command, args.iter().cloned());
    let (refs, secrets) = to_secrets(name, Kind::Env, env)?;
    config.secret_env = refs;
    Ok(Parsed { config, secrets })
}

/// An environment variable name: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_env_key(key: &str) -> bool {
    key.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `KEY=value`, judged by the key alone (the value may hold `/`, `=`, a URL).
fn env_assignment(word: &str) -> Option<(String, String)> {
    word.split_once('=')
        .filter(|(k, _)| is_env_key(k))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
}

fn split_env(pair: &str) -> Result<(String, String), String> {
    env_assignment(pair).ok_or_else(|| "an `-e` value is not KEY=value".to_owned())
}

/// A header name: letters, digits and `-` (what MCP servers document).
fn is_header_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn split_header(raw: &str) -> Result<(String, String), String> {
    raw.split_once(':')
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, _)| is_header_name(k))
        .ok_or_else(|| "a `--header` value is not `Name: value`".to_owned())
}

/// The derived name of a stdio launch: the package spec of a runner, else the
/// command's file name.
fn launch_name(command: &str, args: &[String]) -> String {
    let base = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd");
    let runner = matches!(
        base,
        "npx" | "bunx" | "uvx" | "pipx" | "pnpm" | "yarn" | "dlx"
    );
    let spec = runner
        .then(|| {
            args.iter()
                .find(|a| !a.starts_with('-') && !matches!(a.as_str(), "dlx" | "run" | "exec"))
        })
        .flatten();
    spec.map_or_else(|| clean_name(base), |s| name_from_package(s))
}

/// `claude mcp add [options] <name> <url | -- command args...>`.
fn parse_claude(words: &[String]) -> Result<Parsed, String> {
    let (head, tail) = match words.iter().position(|w| w == "--") {
        Some(i) => (&words[..i], Some(&words[i + 1..])),
        None => (words, None),
    };
    let mut transport = None;
    let mut env = Vec::new();
    let mut headers = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < head.len() {
        let word = head[i].as_str();
        let (flag, inline) = match word.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (word, None),
        };
        let takes_value = matches!(
            flag,
            "-t" | "--transport" | "-s" | "--scope" | "-e" | "--env" | "-H" | "--header"
        );
        if takes_value {
            let value = if let Some(v) = inline {
                v
            } else {
                i += 1;
                head.get(i)
                    .cloned()
                    .ok_or_else(|| format!("`{flag}` needs a value"))?
            };
            match flag {
                "-t" | "--transport" => transport = Some(value),
                "-e" | "--env" => env.push(split_env(&value)?),
                "-H" | "--header" => headers.push(split_header(&value)?),
                _ => {} // --scope: Claude Code's own setting, meaningless here
            }
        } else if word.starts_with('-') {
            // The option name only: a value after `=` may be a secret.
            let name = word.split('=').next().unwrap_or_default();
            let shown = if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                name
            } else {
                "?"
            };
            return Err(format!("unknown `claude mcp add` option `{shown}`"));
        } else {
            positional.push(word.to_owned());
        }
        i += 1;
    }
    let Some((name, rest)) = positional.split_first() else {
        return Err("`claude mcp add` needs a server name".into());
    };
    let name = clean_name(name);
    match (transport.as_deref(), tail) {
        (Some("sse"), _) => Err(
            "the deprecated SSE transport is not supported; use the server's streamable HTTP URL"
                .into(),
        ),
        (Some("http") | None, None) if rest.len() == 1 && is_url(&rest[0]) => {
            if !env.is_empty() {
                return Err("`-e` applies to stdio servers only".into());
            }
            http_server(&name, &rest[0], headers)
        }
        (Some("stdio") | None, Some(_)) | (Some("stdio"), None) => {
            let command: Vec<String> = match tail {
                // Only the part after `--` runs; more words before it are ambiguous.
                Some(after) if rest.is_empty() => after.to_vec(),
                Some(_) => {
                    return Err("unexpected words between the name and `--`".into());
                }
                None => rest.to_vec(),
            };
            let Some((program, args)) = command.split_first() else {
                return Err("`claude mcp add` gives no command to run".into());
            };
            if !headers.is_empty() {
                return Err("`--header` applies to HTTP servers only".into());
            }
            stdio_server(&name, program, args, env)
        }
        (Some(other), _) if !matches!(other, "http" | "stdio") => {
            Err("unknown transport (use `http` or `stdio`)".into())
        }
        _ => Err(format!("cannot read this `claude mcp add` line: {FORMATS}")),
    }
}

/// `[KEY=value ...] <command> [args...]` (an `npx`/`uvx` line).
fn parse_launch(words: &[String]) -> Result<Parsed, String> {
    let mut env = Vec::new();
    let mut rest = words;
    while let Some((first, tail)) = rest.split_first() {
        match env_assignment(first) {
            Some(pair) => {
                env.push(pair);
                rest = tail;
            }
            None => break,
        }
    }
    let Some((command, args)) = rest.split_first() else {
        return Err(format!("no command found: {FORMATS}"));
    };
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let known = [
        "npx", "bunx", "uvx", "pipx", "pnpm", "yarn", "docker", "node", "python", "python3", "uv",
        "deno",
    ];
    if !known
        .iter()
        .any(|k| base == *k || base == format!("{k}.exe") || base == format!("{k}.cmd"))
    {
        return Err(format!(
            "the command is not a recognised MCP launch command: {FORMATS}"
        ));
    }
    stdio_server(&launch_name(command, args), command, args, env)
}

fn str_map(value: Option<&Value>, what: &str) -> Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| format!("`{what}` must be an object"))?;
    map.iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|s| (k.clone(), s.to_owned()))
                .ok_or_else(|| format!("`{what}.{k}` must be a string"))
        })
        .collect()
}

/// One server entry of another app's config (`name` + its JSON object), as
/// [`parse`] would read it: values become secret references, the result is
/// validated, and the error text never quotes the entry.
///
/// # Errors
/// An unsupported transport, a refused or unpinned stdio command, a bad name.
pub fn parse_entry(name: &str, value: &Value) -> Result<Parsed, String> {
    let parsed = server_from_json(&clean_name(name), value)?;
    check(&parsed.config)?;
    Ok(parsed)
}

fn server_from_json(name: &str, value: &Value) -> Result<Parsed, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("server `{name}` must be a JSON object"))?;
    let kind = object
        .get("type")
        .or_else(|| object.get("transport"))
        .and_then(Value::as_str);
    if matches!(kind, Some("sse")) {
        return Err(format!(
            "server `{name}`: the deprecated SSE transport is not supported"
        ));
    }
    if let Some(url) = object
        .get("url")
        .or_else(|| object.get("serverUrl"))
        .and_then(Value::as_str)
    {
        if object.contains_key("env") {
            return Err(format!(
                "server `{name}`: `env` applies to stdio servers only"
            ));
        }
        return http_server(name, url, str_map(object.get("headers"), "headers")?);
    }
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("server `{name}` has neither `command` nor `url`"))?;
    let args = match object.get("args") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|a| {
                a.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("server `{name}`: every arg must be a string"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("server `{name}`: `args` must be an array")),
    };
    if object.contains_key("headers") {
        return Err(format!(
            "server `{name}`: `headers` apply to HTTP servers only"
        ));
    }
    stdio_server(name, command, &args, str_map(object.get("env"), "env")?)
}

fn parse_json(input: &str) -> Result<Vec<Parsed>, String> {
    let value: Value = serde_json::from_str(input)
        .map_err(|e| format!("invalid JSON (line {}, column {})", e.line(), e.column()))?;
    let object = value.as_object().ok_or("the JSON must be an object")?;
    let servers = object
        .get("mcpServers")
        .or_else(|| object.get("servers"))
        .or_else(|| object.get("mcp").and_then(|m| m.get("servers")));
    let single = object.contains_key("command") || object.contains_key("url");
    let list: Vec<(String, &Value)> = if let Some(servers) = servers {
        servers
            .as_object()
            .ok_or("`mcpServers` must be an object")?
            .iter()
            .map(|(k, v)| (k.clone(), v))
            .collect()
    } else if single {
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .map(clean_name)
            .or_else(|| {
                object
                    .get("url")
                    .and_then(Value::as_str)
                    .and_then(|u| name_from_url(u).ok())
            })
            .or_else(|| {
                object.get("command").and_then(Value::as_str).map(|c| {
                    let args: Vec<String> = object
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    launch_name(c, &args)
                })
            })
            .unwrap_or_default();
        vec![(name, &value)]
    } else {
        object.iter().map(|(k, v)| (k.clone(), v)).collect()
    };
    if list.is_empty() {
        return Err("the JSON lists no servers".into());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (name, v) in list {
        let name = clean_name(&name);
        // `a/b` and `a-b` both become `a-b`: one name, one server.
        if !names.insert(name.to_ascii_lowercase()) {
            return Err("two servers in the paste have the same name".into());
        }
        out.push(server_from_json(&name, v)?);
    }
    Ok(out)
}

/// POSIX-like word splitting: whitespace, `'...'`, `"..."`, backslash escapes
/// (outside single quotes), and `\` + newline continuations.
fn split_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => return Err("unterminated single quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(ch @ ('"' | '\\' | '$' | '`')) => current.push(ch),
                            Some('\n') => {}
                            Some(ch) => {
                                current.push('\\');
                                current.push(ch);
                            }
                            None => return Err("unterminated double quote".into()),
                        },
                        Some(ch) => current.push(ch),
                        None => return Err("unterminated double quote".into()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some(ch) if ch != '\n' => {
                    in_word = true;
                    current.push(ch);
                }
                _ => {}
            },
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compact, stable rendering of a parse result for the golden tests.
    fn golden(input: &str) -> String {
        match parse(input, None) {
            Err(e) => format!("error: {e}"),
            Ok(list) => list
                .iter()
                .map(|p| {
                    let body = create_body(&p.config);
                    let secrets: Vec<&str> = p.secrets.iter().map(|(n, _)| n.as_str()).collect();
                    format!("{body} secrets={secrets:?}")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn golden_bare_url() {
        assert_eq!(
            golden("  https://mcp.linear.app/mcp \n"),
            r#"{"enabled":true,"name":"linear","secret_env":{},"secret_headers":{},"transport":"http","url":"https://mcp.linear.app/mcp"} secrets=[]"#
        );
        assert_eq!(
            golden("http://127.0.0.1:8931/mcp"),
            r#"{"enabled":true,"name":"127.0.0.1","secret_env":{},"secret_headers":{},"transport":"http","url":"http://127.0.0.1:8931/mcp"} secrets=[]"#
        );
    }

    #[test]
    fn golden_claude_mcp_add_lines() {
        assert_eq!(
            golden("claude mcp add --transport http notion https://mcp.notion.com/mcp"),
            r#"{"enabled":true,"name":"notion","secret_env":{},"secret_headers":{},"transport":"http","url":"https://mcp.notion.com/mcp"} secrets=[]"#
        );
        assert_eq!(
            golden(
                "claude mcp add --transport http stripe https://mcp.stripe.com \\\n  --header \"Authorization: Bearer sk_test_123\""
            ),
            r#"{"enabled":true,"name":"stripe","secret_env":{},"secret_headers":{"Authorization":"mcp/stripe/header/authorization"},"transport":"http","url":"https://mcp.stripe.com"} secrets=["mcp/stripe/header/authorization"]"#
        );
        assert_eq!(
            golden(
                "claude mcp add github -s user -e GITHUB_TOKEN=ghp_x -- npx -y @modelcontextprotocol/server-github"
            ),
            "error: the package is unpinned: use an exact version (`pkg@1.2.3` or `pkg==1.2.3`), never @latest or a floating install"
        );
        assert_eq!(
            golden(
                "claude mcp add files -e ROOT_TOKEN=abc -- npx -y @modelcontextprotocol/server-filesystem@0.6.2 /home/me"
            ),
            r#"{"args":["-y","@modelcontextprotocol/server-filesystem@0.6.2","/home/me"],"command":"npx","enabled":true,"name":"files","secret_env":{"ROOT_TOKEN":"mcp/files/env/ROOT_TOKEN"},"secret_headers":{},"transport":"stdio"} secrets=["mcp/files/env/ROOT_TOKEN"]"#
        );
        assert!(golden("claude mcp add --transport sse old https://x.example/sse").contains("SSE"));
    }

    #[test]
    fn golden_npx_and_uvx_lines() {
        assert_eq!(
            golden("npx -y @playwright/mcp@0.0.41"),
            r#"{"args":["-y","@playwright/mcp@0.0.41"],"command":"npx","enabled":true,"name":"playwright","secret_env":{},"secret_headers":{},"transport":"stdio"} secrets=[]"#
        );
        assert_eq!(
            golden("BRAVE_API_KEY=xyz npx -y @brave/brave-search-mcp-server@1.3.0"),
            r#"{"args":["-y","@brave/brave-search-mcp-server@1.3.0"],"command":"npx","enabled":true,"name":"brave-search","secret_env":{"BRAVE_API_KEY":"mcp/brave-search/env/BRAVE_API_KEY"},"secret_headers":{},"transport":"stdio"} secrets=["mcp/brave-search/env/BRAVE_API_KEY"]"#
        );
        assert_eq!(
            golden("uvx mcp-server-fetch==2025.4.7"),
            r#"{"args":["mcp-server-fetch==2025.4.7"],"command":"uvx","enabled":true,"name":"fetch","secret_env":{},"secret_headers":{},"transport":"stdio"} secrets=[]"#
        );
        // The catalog's pin rule: floating installs are refused.
        assert!(golden("npx -y @modelcontextprotocol/server-memory").contains("unpinned"));
        assert!(golden("npx -y some-server@latest").contains("@latest"));
        assert!(golden("rm -rf /").starts_with("error:"));
    }

    #[test]
    fn golden_json_configs() {
        assert_eq!(
            golden(
                r#"{"mcpServers": {
                    "filesystem": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem@0.6.2", "/tmp"]},
                    "linear": {"type": "http", "url": "https://mcp.linear.app/mcp", "headers": {"X-Api-Key": "lin_abc"}}
                }}"#
            ),
            [
                r#"{"args":["-y","@modelcontextprotocol/server-filesystem@0.6.2","/tmp"],"command":"npx","enabled":true,"name":"filesystem","secret_env":{},"secret_headers":{},"transport":"stdio"} secrets=[]"#,
                r#"{"enabled":true,"name":"linear","secret_env":{},"secret_headers":{"X-Api-Key":"mcp/linear/header/x-api-key"},"transport":"http","url":"https://mcp.linear.app/mcp"} secrets=["mcp/linear/header/x-api-key"]"#,
            ]
            .join("\n")
        );
        assert_eq!(
            golden(
                r#"{"command": "uvx", "args": ["mcp-server-git==0.6.2"], "env": {"GIT_TOKEN": "t"}}"#
            ),
            r#"{"args":["mcp-server-git==0.6.2"],"command":"uvx","enabled":true,"name":"git","secret_env":{"GIT_TOKEN":"mcp/git/env/GIT_TOKEN"},"secret_headers":{},"transport":"stdio"} secrets=["mcp/git/env/GIT_TOKEN"]"#
        );
        assert!(
            golden(r#"{"x": {"type": "sse", "url": "https://a.example/sse"}}"#).contains("SSE")
        );
        assert!(golden("{not json").starts_with("error: invalid JSON"));
    }

    #[test]
    fn pasted_values_never_reach_the_config_and_names_can_be_overridden() {
        let parsed = parse(
            "claude mcp add files -e ROOT_TOKEN=very-secret-value -- npx -y @modelcontextprotocol/server-filesystem@0.6.2 /home/me",
            Some("my-files"),
        )
        .unwrap();
        let [p] = parsed.as_slice() else { panic!() };
        assert_eq!(p.config.id, "my-files");
        assert_eq!(
            p.secrets,
            vec![(
                "mcp/my-files/env/ROOT_TOKEN".to_owned(),
                "very-secret-value".to_owned()
            )]
        );
        assert_eq!(p.config.secret_env["ROOT_TOKEN"], p.secrets[0].0);
        let rendered = format!("{} {:?}", create_body(&p.config), p);
        assert!(!rendered.contains("very-secret-value"), "{rendered}");
        let McpTransport::Stdio { env, .. } = &p.config.transport else {
            panic!()
        };
        assert!(env.is_empty());
        // Two servers cannot share one --name.
        assert!(
            parse(
                r#"{"a": {"url": "https://a.example/mcp"}, "b": {"url": "https://b.example/mcp"}}"#,
                Some("x")
            )
            .is_err()
        );
    }

    #[test]
    fn already_split_arguments_keep_their_quoting() {
        let args: Vec<String> = [
            "claude",
            "mcp",
            "add",
            "files",
            "-e",
            "NOTE=two words",
            "--",
            "npx",
            "-y",
            "@modelcontextprotocol/server-filesystem@0.6.2",
            "/My Documents",
        ]
        .map(str::to_owned)
        .to_vec();
        let [p] = parse_argv(&args, None).unwrap().try_into().unwrap();
        assert_eq!(p.secrets[0].1, "two words");
        let McpTransport::Stdio { args, .. } = &p.config.transport else {
            panic!()
        };
        assert_eq!(args.last().unwrap(), "/My Documents");
        let one = vec!["https://mcp.linear.app/mcp".to_owned()];
        assert_eq!(parse_argv(&one, Some("lin")).unwrap()[0].config.id, "lin");
        let unpinned: Vec<String> = ["npx", "-y", "pkg"].map(str::to_owned).to_vec();
        assert!(
            parse_argv(&unpinned, None)
                .unwrap_err()
                .contains("unpinned")
        );
    }

    /// `bot mcp add` against a live daemon with a real `McpHost`: the pasted
    /// header value lands in the secret store only, the server is created and
    /// tried, and a 401 is reported as a sign-in to do.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_stores_pasted_values_as_secrets_and_reports_the_state() {
        use std::sync::Arc;
        const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        const VALUE: &str = "pasted-header-value-sentinel-4be0";
        let remote = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/mcp"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).insert_header("www-authenticate", "Bearer"),
            )
            .mount(&remote)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
        let agent = crate::serve::build_agent(
            &storage,
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            dir.path(),
            None,
        );
        let store: Arc<dyn secret_store::SecretStore> = Arc::new(secret_store::MemoryStore::new());
        let host = crate::mcp_host::McpHost::start(
            dir.path(),
            agent.tool_registry(),
            Some(Arc::clone(&store)),
            None,
            None,
        );
        let state = daemon_api::AppState::new(agent, storage, daemon_api::DaemonConfig::new(TOKEN))
            .unwrap();
        state.set_owner_key(crate::client::TEST_OWNER_KEY.into());
        state.set_mcp(Arc::clone(&host) as Arc<dyn daemon_api::McpControl>);
        state.set_secret_store(Arc::clone(&store));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
        let client = crate::client::Client::new(&url, TOKEN.to_owned())
            .with_owner_key(Some(crate::client::TEST_OWNER_KEY.to_owned()));

        let paste = format!(
            "claude mcp add --transport http remote {}/mcp --header \"X-Api-Key: {VALUE}\"",
            remote.uri()
        );
        let parsed = parse(&paste, None).unwrap();
        let mut dry = Vec::new();
        add(&client, &parsed, true, false, &mut dry).await.unwrap();
        let dry = String::from_utf8(dry).unwrap();
        assert!(dry.contains("would add remote (http)"), "{dry}");
        assert!(store.get("mcp/remote/header/x-api-key").unwrap().is_none());

        let mut out = Vec::new();
        add(&client, &parsed, false, false, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("added remote: auth_required"), "{out}");
        assert!(out.contains("Sign in"), "{out}");
        assert_eq!(
            store
                .get("mcp/remote/header/x-api-key")
                .unwrap()
                .unwrap()
                .expose(),
            VALUE
        );
        let file = std::fs::read_to_string(dir.path().join(crate::doctor::MCP_FILE)).unwrap();
        assert!(file.contains("mcp/remote/header/x-api-key"), "{file}");
        for text in [&dry, &out, &file] {
            assert!(!text.contains(VALUE));
        }
        host.shutdown().await;
    }

    /// A daemon with a real `McpHost`; `secrets: false` leaves the secret
    /// store out, so every secret write fails.
    async fn paste_daemon(
        secrets: bool,
    ) -> (
        crate::client::Client,
        std::sync::Arc<dyn secret_store::SecretStore>,
        std::sync::Arc<crate::mcp_host::McpHost>,
        tempfile::TempDir,
    ) {
        use std::sync::Arc;
        const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
        let agent = crate::serve::build_agent(
            &storage,
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            dir.path(),
            None,
        );
        let store: Arc<dyn secret_store::SecretStore> = Arc::new(secret_store::MemoryStore::new());
        let host = crate::mcp_host::McpHost::start(
            dir.path(),
            agent.tool_registry(),
            Some(Arc::clone(&store)),
            None,
            None,
        );
        let state = daemon_api::AppState::new(agent, storage, daemon_api::DaemonConfig::new(TOKEN))
            .unwrap();
        state.set_owner_key(crate::client::TEST_OWNER_KEY.into());
        state.set_mcp(Arc::clone(&host) as Arc<dyn daemon_api::McpControl>);
        if secrets {
            state.set_secret_store(Arc::clone(&store));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
        (
            crate::client::Client::new(&url, TOKEN.to_owned())
                .with_owner_key(Some(crate::client::TEST_OWNER_KEY.to_owned())),
            store,
            host,
            dir,
        )
    }

    /// Review finding 8: distinct servers and keys never share a secret, and
    /// a refused or failing add never alters a live credential.
    #[tokio::test(flavor = "multi_thread")]
    async fn pasted_secret_names_never_collide_and_a_refused_add_writes_nothing() {
        // Names differing only in `-` / `_` / case stay apart.
        let a = parse("A_TOKEN=one npx -y pkg@1.2.3", Some("a-b")).unwrap();
        let b = parse("A_TOKEN=two npx -y pkg@1.2.3", Some("a_b")).unwrap();
        assert_ne!(a[0].secrets[0].0, b[0].secrets[0].0);
        let headers = parse(
            "claude mcp add --transport http s https://s.example/mcp -H \"X-A: 1\" -H \"X_B: 2\"",
            None,
        );
        assert!(headers.is_err(), "`X_B` is not a header name");
        for dup in [
            "claude mcp add --transport http s https://s.example/mcp -H \"X-A: 1\" -H \"x-a: 2\"",
            "A=1 a=2 npx -y pkg@1.2.3",
            r#"{"mcpServers": {"a/b": {"url": "https://a.example/mcp"}, "a-b": {"url": "https://b.example/mcp"}}}"#,
        ] {
            assert!(parse(dup, None).is_err(), "{dup}");
        }
        // Names stay within the secret store's 128 bytes; ids are bounded.
        let long = "x".repeat(101);
        assert!(parse("https://s.example/mcp", Some(&long)).is_err());
        let near = "x".repeat(100);
        let near_limit = parse(
            &format!(
                "claude mcp add --transport http {near} https://s.example/mcp -H \"{}: v\"",
                "h".repeat(20)
            ),
            None,
        );
        assert!(near_limit.is_err(), "secret name over 128 bytes accepted");

        let (client, store, host, _dir) = paste_daemon(true).await;
        // An existing server's credential is not overwritten by a second add
        // of the same name (in any letter case).
        // A command that does not exist: nothing is installed or fetched.
        let missing = |value: &str| {
            format!(
                r#"{{"command": "definitely-missing-mcp-server", "env": {{"API_KEY": "{value}"}}}}"#
            )
        };
        let first = parse(&missing("original-value"), Some("files")).unwrap();
        let mut out = Vec::new();
        add(&client, &first, false, false, &mut out).await.unwrap();
        assert_eq!(
            store
                .get("mcp/files/env/API_KEY")
                .unwrap()
                .unwrap()
                .expose(),
            "original-value"
        );
        for name in ["files", "FILES"] {
            let again = parse(&missing("attacker-value"), Some(name)).unwrap();
            assert!(
                add(&client, &again, false, false, &mut Vec::new())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            store
                .get("mcp/files/env/API_KEY")
                .unwrap()
                .unwrap()
                .expose(),
            "original-value"
        );
        host.shutdown().await;

        // A secret that cannot be stored leaves no server behind.
        let (client, _store, host, _dir) = paste_daemon(false).await;
        let parsed = parse(&missing("v"), Some("orphan")).unwrap();
        assert!(
            add(&client, &parsed, false, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(
            daemon_api::McpControl::list(host.as_ref()).len(),
            0,
            "the new server was not rolled back"
        );
        host.shutdown().await;
    }

    /// Review finding 12: a value is recognised by its key, and no error
    /// quotes a pasted value, header, option value, URL or command.
    #[test]
    fn parser_errors_never_echo_pasted_values() {
        const S: &str = "sentinel-secret-7c21";
        let ok = parse("TOKEN=abc/def npx -y pkg@1.2.3", None).unwrap();
        assert_eq!(ok[0].secrets[0].1, "abc/def");
        let ok = parse("DB_URL=postgres://u:p@h/db?x=1 uvx pkg==1.0.0", None).unwrap();
        assert_eq!(ok[0].secrets[0].1, "postgres://u:p@h/db?x=1");
        for input in [
            format!("claude mcp add x -e {S}-no-equals -- npx -y pkg@1.2.3"),
            format!("claude mcp add --transport http x https://x.example/mcp -H {S}-no-colon"),
            format!("claude mcp add x --api-key={S} -- npx -y pkg@1.2.3"),
            format!("claude mcp add x {S} extra -- npx -y pkg@1.2.3"),
            format!("claude mcp add --transport {S} x https://x.example/mcp"),
            format!("{S}=bad=key? npx -y pkg@1.2.3"),
            format!("not-a-runner --token={S}"),
            format!("rm -rf / --token={S}"),
            format!("npx -y pkg --api-key={S}"),
            format!("npx --registry=https://{S}.example pkg@1.2.3"),
            format!("https://{S}:pw@[bad"),
            format!(r#"{{"mcpServers": {{"x": {{"command": "npx", "args": ["{S}"]}}}}}}"#),
            format!(r#"{{"mcpServers": {{"x": "{S}"}}"#),
        ] {
            let error = parse(&input, None).unwrap_err();
            assert!(!error.contains(S), "{input} -> {error}");
        }
    }

    /// Review finding 7, through every paste entry point: a package named
    /// by `--package` / `--with` / `--from` must be pinned too.
    #[test]
    fn every_entry_point_holds_runner_options_to_the_pin_rule() {
        let argv = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        for input in [
            "npx --package=unpinned-package pinned-command@1.2.3",
            "uvx --with unpinned-dependency pinned-tool==1.2.3",
            "claude mcp add x -- npx -p unpinned-package pinned-command@1.2.3",
            "claude mcp add x -- uvx --from unpinned-src tool",
            r#"{"command": "npx", "args": ["--package=unpinned", "cmd@1.2.3"]}"#,
            r#"{"mcpServers": {"x": {"command": "uvx", "args": ["--with", "dep", "tool==1.0.0"]}}}"#,
        ] {
            let error = parse(input, None).unwrap_err();
            assert!(error.contains("unpinned"), "{input}: {error}");
        }
        for line in [
            "npx --package=unpinned-package pinned-command@1.2.3",
            "claude mcp add x -- uvx --with unpinned-dependency pinned-tool==1.2.3",
        ] {
            assert!(parse_argv(&argv(line), None).is_err(), "{line}");
        }
        assert!(parse("npx -y --package=a@1.2.3 cmd", None).is_ok());
    }

    #[test]
    fn words_split_like_a_shell() {
        assert_eq!(
            split_words(
                r#"a 'b c' "d \"e\"" f\ g \
h"#
            )
            .unwrap(),
            ["a", "b c", "d \"e\"", "f g", "h"]
        );
        assert!(split_words("'open").is_err());
    }
}

#[cfg(test)]
mod property_tests {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::sample::select;
    use proptest::test_runner::RngSeed;

    use super::{create_body, parse, parse_argv};

    fn config() -> ProptestConfig {
        let mut config = ProptestConfig::default();
        if config.rng_seed == RngSeed::Random {
            config.rng_seed = RngSeed::Fixed(461);
        }
        config
    }

    fn untrusted_paste() -> impl Strategy<Value = String> {
        const FRAGMENTS: &[&str] = &[
            "claude mcp add ",
            "--transport http ",
            "--transport sse ",
            "-e KEY=val ",
            "-H \"X-Key: val\" ",
            "-- npx -y @scope/pkg@1.2.3 ",
            "uvx pkg==1.0.0 ",
            "https://mcp.example.com/mcp",
            "{\"mcpServers\":{}}",
            "\"",
            "'",
            "\\",
            "\n",
            "\0",
            "=",
            ":",
            "İıŞş🦀",
        ];
        let part = prop_oneof![
            3 => select(FRAGMENTS).prop_map(str::to_owned),
            2 => ".{0,24}",
        ];
        vec(part, 0..=16).prop_map(|parts| parts.concat())
    }

    proptest! {
        #![proptest_config(config())]

        /// `mcp_paste::parse` and `parse_argv` never panic on arbitrary UTF-8 strings or argument lists.
        #[test]
        fn paste_parsers_never_panic_on_arbitrary_text_or_argv(
            input in untrusted_paste(),
            override_name in proptest::option::of(".{0,32}"),
            argv in vec(".{0,32}", 0..=12),
        ) {
            let _ = parse(&input, override_name.as_deref());
            let _ = parse_argv(&argv, override_name.as_deref());
        }

        /// Pasted environment and header secret values are isolated into `Parsed::secrets` and never appear in the config or Debug output.
        #[test]
        fn pasted_secrets_never_leak_into_config_or_debug(
            env_key in "[A-Z][A-Z0-9_]{2,16}",
            header_key in "X-[A-Za-z0-9]{2,16}",
            secret in "sec_[A-Za-z0-9/_+=.-]{12,32}",
        ) {
            let stdio = format!("{env_key}={secret} npx -y @scope/server-demo@1.2.3");
            let parsed_stdio = parse(&stdio, None).unwrap();
            prop_assert_eq!(parsed_stdio.len(), 1);
            let p = &parsed_stdio[0];
            prop_assert_eq!(&p.secrets[0].1, &secret);
            let dump = format!("{} {:?}", create_body(&p.config), p);
            prop_assert!(!dump.contains(&secret), "secret leaked into stdio config/debug: {dump}");

            let http = format!(
                "claude mcp add --transport http demo https://mcp.example.com/mcp -H \"{header_key}: {secret}\""
            );
            let parsed_http = parse(&http, None).unwrap();
            prop_assert_eq!(parsed_http.len(), 1);
            let p_http = &parsed_http[0];
            prop_assert_eq!(&p_http.secrets[0].1, &secret);
            let dump_http = format!("{} {:?}", create_body(&p_http.config), p_http);
            prop_assert!(
                !dump_http.contains(&secret),
                "secret leaked into http config/debug: {dump_http}"
            );
        }
    }
}

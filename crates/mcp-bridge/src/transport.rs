//! Opening a raw MCP client session over stdio or streamable HTTP.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use http::{HeaderName, HeaderValue};
use parking_lot::Mutex;
use rmcp::{
    RoleClient, ServiceExt,
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig, which_command,
    },
};
use secret_store::{SecretRef, SecretStore, resolve};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    config::{McpServerConfig, McpTransport},
    error::{McpError, Result},
};

/// A live MCP client session.
pub(crate) type Session = RunningService<RoleClient, ()>;

/// Environment variables that are passed to stdio children even though the
/// config does not declare them, because the OS runtime itself breaks without
/// them (Winsock / CRT need `SystemRoot` on Windows).  Empty on other platforms.
#[cfg(windows)]
pub const OS_ESSENTIAL_ENV: &[&str] = &["SystemRoot", "windir"];
/// See the Windows variant.
#[cfg(not(windows))]
pub const OS_ESSENTIAL_ENV: &[&str] = &[];

/// How many stderr lines are kept per server.
const STDERR_TAIL_LINES: usize = 200;

/// Ring buffer of a server's most recent stderr lines (also forwarded to `tracing`).
#[derive(Debug, Default)]
pub(crate) struct StderrLog {
    lines: Mutex<VecDeque<String>>,
}

impl StderrLog {
    fn push(&self, line: String) {
        let mut lines = self.lines.lock();
        if lines.len() == STDERR_TAIL_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub(crate) fn snapshot(&self) -> Vec<String> {
        self.lines.lock().iter().cloned().collect()
    }
}

/// Secret values resolved for one connect attempt.  Lives only for the
/// duration of [`open`]; `Debug` never prints the values.
#[derive(Default)]
pub(crate) struct ResolvedSecrets {
    headers: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
}

impl fmt::Debug for ResolvedSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedSecrets")
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Resolve `secret_headers` / `secret_env` through the store.  Fails (so the
/// server is never started) if there is no store or a secret is missing.
/// Error messages carry the secret *name*, never a value.
pub(crate) fn resolve_secrets(
    config: &McpServerConfig,
    store: Option<&dyn SecretStore>,
) -> Result<ResolvedSecrets> {
    let mut out = ResolvedSecrets::default();
    if config.secret_headers.is_empty() && config.secret_env.is_empty() {
        return Ok(out);
    }
    let id = &config.id;
    let store = store.ok_or_else(|| {
        McpError::Config(format!(
            "server `{id}` uses secret references but the manager has no secret store"
        ))
    })?;
    let fetch = |kind: &str, key: &str, name: &str| -> Result<String> {
        let r = SecretRef::new(name)
            .map_err(|e| McpError::Config(format!("server `{id}`: `{kind}.{key}`: {e}")))?;
        resolve(store, &r)
            .map(|v| v.expose().to_owned())
            .map_err(|e| {
                McpError::Config(format!(
                    "server `{id}`: cannot resolve secret `{name}` for `{kind}.{key}`: {e}"
                ))
            })
    };
    for (k, name) in &config.secret_headers {
        out.headers
            .insert(k.clone(), fetch("secret_headers", k, name)?);
    }
    for (k, name) in &config.secret_env {
        out.env.insert(k.clone(), fetch("secret_env", k, name)?);
    }
    Ok(out)
}

fn transport_err(server: &str, e: impl std::fmt::Display) -> McpError {
    McpError::Transport {
        server: server.to_owned(),
        message: e.to_string(),
    }
}

/// Build the command for a stdio server with a minimal, explicit environment.
fn build_command(
    server: &str,
    config: &McpTransport,
    secrets: &ResolvedSecrets,
) -> Result<tokio::process::Command> {
    let McpTransport::Stdio {
        command,
        args,
        env,
        cwd,
    } = config
    else {
        unreachable!("build_command is only called for stdio transports");
    };
    let spawn_err = |e: std::io::Error| McpError::Spawn {
        server: server.to_owned(),
        message: format!("`{command}`: {e}"),
    };
    // Resolve against the parent's PATH *before* clearing the child's env.
    let mut cmd = which_command(command).map_err(spawn_err)?;
    cmd.env_clear();
    for key in OS_ESSENTIAL_ENV {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.args(args)
        .envs(env)
        .envs(&secrets.env)
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    Ok(cmd)
}

fn forward_stderr(server: String, stderr: tokio::process::ChildStderr, log: Arc<StderrLog>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(target: "mcp_bridge::stderr", server = %server, "{line}");
            log.push(line);
        }
    });
}

fn http_config(
    server: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
    secret_headers: &BTreeMap<String, String>,
) -> Result<StreamableHttpClientTransportConfig> {
    let mut map = HashMap::new();
    for (k, v) in headers.iter().chain(secret_headers) {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| McpError::Config(format!("{server}: bad header name `{k}`: {e}")))?;
        let mut value = HeaderValue::from_str(v)
            .map_err(|e| McpError::Config(format!("{server}: bad value for header `{k}`: {e}")))?;
        value.set_sensitive(true);
        map.insert(name, value);
    }
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    cfg.custom_headers = map;
    Ok(cfg)
}

/// Connect and complete the MCP handshake, bounded by `timeout`.
pub(crate) async fn open(
    config: &McpServerConfig,
    secrets: &ResolvedSecrets,
    stderr: &Arc<StderrLog>,
    timeout: Duration,
) -> Result<Session> {
    let id = config.id.as_str();
    let handshake = async {
        match &config.transport {
            McpTransport::Stdio { .. } => {
                let cmd = build_command(id, &config.transport, secrets)?;
                let (transport, child_stderr) = TokioChildProcess::builder(cmd)
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| McpError::Spawn {
                        server: id.to_owned(),
                        message: e.to_string(),
                    })?;
                if let Some(pipe) = child_stderr {
                    forward_stderr(id.to_owned(), pipe, Arc::clone(stderr));
                }
                ().serve(transport).await.map_err(|e| transport_err(id, e))
            }
            McpTransport::Http { url, headers } => {
                let transport = StreamableHttpClientTransport::from_config(http_config(
                    id,
                    url,
                    headers,
                    &secrets.headers,
                )?);
                ().serve(transport).await.map_err(|e| transport_err(id, e))
            }
        }
    };
    tokio::time::timeout(timeout, handshake)
        .await
        .map_err(|_| McpError::Timeout {
            server: id.to_owned(),
            seconds: timeout.as_secs().max(1),
        })?
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn http_headers_are_validated_and_marked_sensitive() {
        let mut h = BTreeMap::new();
        h.insert("Authorization".to_owned(), "Bearer abc".to_owned());
        let cfg = http_config("s", "https://x/mcp", &h, &BTreeMap::new()).unwrap();
        let v = cfg.custom_headers.values().next().unwrap();
        assert!(v.is_sensitive());

        h.insert("bad header".to_owned(), "v".to_owned());
        assert!(matches!(
            http_config("s", "https://x", &h, &BTreeMap::new()),
            Err(McpError::Config(_))
        ));
    }

    struct Fake(BTreeMap<String, String>);

    impl SecretStore for Fake {
        fn get(
            &self,
            name: &str,
        ) -> std::result::Result<Option<secret_store::SecretValue>, secret_store::SecretError>
        {
            Ok(self.0.get(name).map(secret_store::SecretValue::new))
        }
        fn set(&self, _: &str, _: &str) -> std::result::Result<(), secret_store::SecretError> {
            unimplemented!()
        }
        fn delete(&self, _: &str) -> std::result::Result<bool, secret_store::SecretError> {
            unimplemented!()
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            unimplemented!()
        }
    }

    #[test]
    fn secrets_resolve_at_connect_and_never_show_in_debug() {
        let store = Fake(BTreeMap::from([(
            "tok".to_owned(),
            "s3cr3t-value".to_owned(),
        )]));
        let mut cfg = McpServerConfig::http("s", "https://x/mcp");
        cfg.secret_headers
            .insert("Authorization".into(), "tok".into());
        let r = resolve_secrets(&cfg, Some(&store)).unwrap();
        assert_eq!(r.headers["Authorization"], "s3cr3t-value");
        assert!(!format!("{r:?}").contains("s3cr3t-value"));
        assert!(!format!("{cfg:?}").contains("s3cr3t-value"));
        assert!(!toml::to_string(&cfg).unwrap().contains("s3cr3t-value"));
        // The resolved value reaches the HTTP config, marked sensitive.
        let http = http_config("s", "https://x/mcp", &BTreeMap::new(), &r.headers).unwrap();
        assert!(http.custom_headers.values().all(HeaderValue::is_sensitive));
        assert!(!format!("{http:?}").contains("s3cr3t-value"));
    }

    #[test]
    fn missing_secret_or_store_is_a_config_error() {
        let mut cfg = McpServerConfig::stdio("s", "x", [""; 0]);
        cfg.secret_env.insert("KEY".into(), "absent".into());
        let empty = Fake(BTreeMap::new());
        let err = resolve_secrets(&cfg, Some(&empty)).unwrap_err();
        assert!(
            matches!(err, McpError::Config(ref m) if m.contains("absent")),
            "{err}"
        );
        assert!(matches!(
            resolve_secrets(&cfg, None),
            Err(McpError::Config(_))
        ));
        // No secret refs: no store needed.
        let plain = McpServerConfig::stdio("s", "x", [""; 0]);
        assert!(resolve_secrets(&plain, None).is_ok());
    }

    #[test]
    fn stderr_log_is_a_bounded_ring() {
        let log = StderrLog::default();
        for i in 0..(STDERR_TAIL_LINES + 10) {
            log.push(format!("l{i}"));
        }
        let snap = log.snapshot();
        assert_eq!(snap.len(), STDERR_TAIL_LINES);
        assert_eq!(snap[0], "l10");
    }

    #[tokio::test]
    async fn missing_command_is_a_spawn_error() {
        let cfg = McpServerConfig::stdio("s", "definitely-not-a-real-binary-xyz", [""; 0]);
        let err = open(
            &cfg,
            &ResolvedSecrets::default(),
            &Arc::default(),
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, McpError::Spawn { .. }), "{err}");
    }
}

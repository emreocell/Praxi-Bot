//! Opening a raw MCP client session over stdio or streamable HTTP.

use std::{
    collections::{HashMap, VecDeque},
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

fn transport_err(server: &str, e: impl std::fmt::Display) -> McpError {
    McpError::Transport {
        server: server.to_owned(),
        message: e.to_string(),
    }
}

/// Build the command for a stdio server with a minimal, explicit environment.
fn build_command(server: &str, config: &McpTransport) -> Result<tokio::process::Command> {
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
    cmd.args(args).envs(env).kill_on_drop(true);
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
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<StreamableHttpClientTransportConfig> {
    let mut map = HashMap::new();
    for (k, v) in headers {
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
    stderr: &Arc<StderrLog>,
    timeout: Duration,
) -> Result<Session> {
    let id = config.id.as_str();
    let handshake = async {
        match &config.transport {
            McpTransport::Stdio { .. } => {
                let cmd = build_command(id, &config.transport)?;
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
                let transport =
                    StreamableHttpClientTransport::from_config(http_config(id, url, headers)?);
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
        let cfg = http_config("s", "https://x/mcp", &h).unwrap();
        let v = cfg.custom_headers.values().next().unwrap();
        assert!(v.is_sensitive());

        h.insert("bad header".to_owned(), "v".to_owned());
        assert!(matches!(
            http_config("s", "https://x", &h),
            Err(McpError::Config(_))
        ));
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
        let err = open(&cfg, &Arc::default(), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Spawn { .. }), "{err}");
    }
}

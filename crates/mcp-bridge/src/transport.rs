//! Opening a raw MCP client session over stdio or streamable HTTP.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use http::{HeaderName, HeaderValue};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use rmcp::{
    RoleClient, ServiceExt,
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess, Transport,
        streamable_http_client::StreamableHttpClientTransportConfig, which_command,
    },
};
use secret_store::{SecretRef, SecretStore, resolve};
use tracing::instrument::WithSubscriber;

use crate::{
    config::{McpServerConfig, McpTransport, refused_stdio_command},
    diagnostics::{Redactor, StderrLog, capture_stderr},
    error::{McpError, Result},
    session::{self, Session},
};

/// Environment variables that are passed to stdio children even though the
/// config does not declare them, because the OS runtime itself breaks without
/// them (Winsock / CRT need `SystemRoot` on Windows).  Empty on other platforms.
#[cfg(windows)]
pub const OS_ESSENTIAL_ENV: &[&str] = &["SystemRoot", "windir"];
/// See the Windows variant.
#[cfg(not(windows))]
pub const OS_ESSENTIAL_ENV: &[&str] = &[];

/// Secret values resolved for one connect attempt.  Lives only for the
/// duration of [`open`]; its redactor stays with the session. `Debug` never
/// prints the values.
#[derive(Clone, Default)]
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
            .map_err(|_| {
                // A backend error may contain credential material that never
                // reached ResolvedSecrets, so do not expose its arbitrary text.
                McpError::Config(format!(
                    "server `{id}`: cannot resolve secret `{name}` for `{kind}.{key}`"
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

/// Put the child in its own process group (Unix) or a kill-on-close job
/// (Windows) so wrapper trees (`npx`, `uvx`) die with the server.
fn isolate_child(cmd: tokio::process::Command) -> CommandWrap {
    let mut wrap = CommandWrap::from(cmd);
    // `KillOnDrop` is what makes the Windows job use `KILL_ON_JOB_CLOSE`.
    wrap.wrap(KillOnDrop);
    #[cfg(unix)]
    wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        wrap.wrap(process_wrap::tokio::CreationFlags(CREATE_NO_WINDOW));
        wrap.wrap(process_wrap::tokio::JobObject);
    }
    wrap
}

/// Build the command for a stdio server with a minimal, explicit environment.
fn build_command(
    server: &str,
    config: &McpTransport,
    secrets: &ResolvedSecrets,
) -> Result<CommandWrap> {
    let McpTransport::Stdio {
        command,
        args,
        env,
        cwd,
    } = config
    else {
        unreachable!("build_command is only called for stdio transports");
    };
    if let Some(why) = refused_stdio_command(command, args) {
        return Err(McpError::Config(format!("server `{server}`: {why}")));
    }
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
    cmd.args(args).envs(env).envs(&secrets.env);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    Ok(isolate_child(cmd))
}

fn forward_stderr(
    server: String,
    stderr: tokio::process::ChildStderr,
    log: Arc<StderrLog>,
    redactor: Redactor,
    diagnostics: tracing::Dispatch,
) {
    tokio::spawn(
        async move { capture_stderr(&server, stderr, &log, &redactor).await }
            .with_subscriber(diagnostics),
    );
}

/// Own the process tree until it has been killed and reaped. rmcp's graceful
/// close releases its wrapper as soon as the direct child exits, which can
/// leave grandchildren running. Cancellation must also kill synchronously:
/// runtime teardown may discard a newly spawned asynchronous cleanup task.
struct ChildTree(Option<TokioChildProcess>);

struct ReapChild(Option<Box<dyn ChildWrapper>>);

impl Drop for ReapChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

impl ChildTree {
    fn take_child(&mut self) -> ReapChild {
        ReapChild(self.0.take().and_then(TokioChildProcess::into_inner))
    }
}

impl Drop for ChildTree {
    fn drop(&mut self) {
        drop(self.take_child());
    }
}

impl Transport<RoleClient> for ChildTree {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = std::io::Result<()>> + Send + 'static {
        let send = self.0.as_mut().map(|transport| transport.send(item));
        async move {
            match send {
                Some(send) => send.await,
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "MCP child transport closed",
                )),
            }
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.0.as_mut()?.receive().await
    }

    async fn close(&mut self) -> std::io::Result<()> {
        let mut guard = self.take_child();
        if let Some(child) = guard.0.as_mut() {
            child.start_kill()?;
            child.wait().await?;
        }
        guard.0 = None;
        Ok(())
    }
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
    let redactor = Redactor::new(
        secrets
            .headers
            .values()
            .chain(secrets.env.values())
            .map(String::as_str),
    );
    let config = config.clone();
    let secrets = secrets.clone();
    let stderr = Arc::clone(stderr);
    let diagnostics = tracing::dispatcher::get_default(Clone::clone);
    let stderr_redactor = redactor.clone();
    session::open(config.id.clone(), redactor, timeout, move || async move {
        let id = config.id.as_str();
        match &config.transport {
            McpTransport::Stdio { .. } => {
                let cmd = build_command(id, &config.transport, &secrets)?;
                let (transport, child_stderr) = TokioChildProcess::builder(cmd)
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| McpError::Spawn {
                        server: id.to_owned(),
                        message: e.to_string(),
                    })?;
                if let Some(pipe) = child_stderr {
                    forward_stderr(id.to_owned(), pipe, stderr, stderr_redactor, diagnostics);
                }
                ().serve(ChildTree(Some(transport)))
                    .await
                    .map_err(|e| transport_err(id, e))
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
    })
    .await
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

    struct LeakyBackend;

    impl SecretStore for LeakyBackend {
        fn get(
            &self,
            _: &str,
        ) -> std::result::Result<Option<secret_store::SecretValue>, secret_store::SecretError>
        {
            Err(secret_store::SecretError::Keyring(
                "backend printed unresolved-secret-sentinel".into(),
            ))
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
    fn secret_backend_error_text_is_not_exposed_before_a_redactor_exists() {
        let mut cfg = McpServerConfig::http("backend", "https://unused.invalid/mcp");
        cfg.secret_headers
            .insert("Authorization".into(), "key/ref".into());
        let error = resolve_secrets(&cfg, Some(&LeakyBackend))
            .unwrap_err()
            .to_string();
        assert!(!error.contains("unresolved-secret-sentinel"));
        assert!(error.contains("key/ref"));
        assert!(error.contains("secret_headers.Authorization"));
    }

    #[tokio::test]
    async fn never_list_command_is_refused_before_spawn() {
        let cfg = McpServerConfig::stdio("s", "rm", ["-rf", "/"]);
        let err = open(
            &cfg,
            &ResolvedSecrets::default(),
            &Arc::default(),
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, McpError::Config(ref m) if m.contains("never-list")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn transport_errors_scrub_both_environment_and_header_secret_values() {
        let secrets = ResolvedSecrets {
            env: BTreeMap::from([("KEY".into(), "environment-sentinel".into())]),
            headers: BTreeMap::from([("Authorization".into(), "header-sentinel".into())]),
        };
        let cfg = McpServerConfig::stdio(
            "secret-errors",
            "missing-binary-environment-sentinel-header-sentinel",
            [""; 0],
        );
        let error = open(&cfg, &secrets, &Arc::default(), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::Spawn { .. }));
        let message = error.to_string();
        assert!(!message.contains("environment-sentinel"));
        assert!(!message.contains("header-sentinel"));
        assert!(message.contains("[REDACTED]"));
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

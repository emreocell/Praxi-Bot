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
        StreamableHttpClientTransport, Transport, async_rw::AsyncRwTransport,
        streamable_http_client::StreamableHttpClientTransportConfig, which_command,
    },
};
use secret_store::{SecretRef, SecretStore, resolve};
use tracing::instrument::WithSubscriber;

use crate::{
    client::{BridgeClient, Signals},
    config::{McpServerConfig, McpTransport, refused_stdio_command},
    diagnostics::{ExternalRedactor, Redactor, StderrLog, capture_stderr},
    error::{McpError, Result, runtime_install_hint},
    session::{self, Session},
};

/// Environment variables that are passed to stdio children even though the
/// config does not declare them, because the launchers themselves break
/// without them: `npx.cmd` / `uvx` cannot find `node`, `npm` or Python without
/// `PATH`, and npm / uv need the profile and temp directories for their caches.
/// None of these carry credentials (proxy variables are deliberately left out:
/// they may embed passwords; declare them in the server's `env` if needed).
#[cfg(windows)]
pub const OS_ESSENTIAL_ENV: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "PATH",
    "PATHEXT",
    "ComSpec",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
];
/// See the Windows variant.
#[cfg(not(windows))]
pub const OS_ESSENTIAL_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"];

/// Largest single JSON-RPC frame accepted from a server: one stdout line, or
/// one SSE event.  Enforced while reading, before rmcp buffers or parses the
/// frame, so it is also the per-page limit of `tools/list` (the aggregate
/// 4 MiB schema budget is applied on top, across pages).  An oversized stdio
/// frame closes the transport; an oversized SSE event is rejected by rmcp.
/// (A JSON HTTP body has rmcp's own fixed bound.)
pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// How long a stdio server gets to exit on its own after its stdin was closed
/// (the shutdown order the MCP stdio transport asks for) before its process
/// tree is killed. Well below the session's close timeout.
const GRACEFUL_EXIT: Duration = Duration::from_secs(2);

/// Fails the read as soon as one line exceeds `cap` bytes, so a newline-free
/// or huge frame never grows rmcp's line buffer past `cap` plus one read.
struct FrameLimit<R> {
    inner: R,
    line: usize,
    cap: usize,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for FrameLimit<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let this = &mut *self;
        let ready = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(ready, std::task::Poll::Ready(Ok(()))) {
            for byte in &buf.filled()[before..] {
                if *byte == b'\n' {
                    this.line = 0;
                } else {
                    this.line += 1;
                }
            }
            if this.line > this.cap {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("MCP frame exceeds the {} byte limit", this.cap),
                )));
            }
        }
        ready
    }
}

type ChildIo = AsyncRwTransport<
    RoleClient,
    FrameLimit<tokio::process::ChildStdout>,
    tokio::process::ChildStdin,
>;

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

impl ResolvedSecrets {
    /// Attach the OAuth access token of this server. It is sent like a secret
    /// header (marked sensitive, masked by the session redactor) and only to
    /// the URL of the config this value was resolved for.
    pub(crate) fn with_bearer(mut self, token: &str) -> Self {
        self.headers
            .insert("Authorization".to_owned(), format!("Bearer {token}"));
        self
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
    for (k, template) in &config.secret_headers {
        out.headers
            .insert(k.clone(), render_header(&fetch, id, k, template)?);
    }
    for (k, name) in &config.secret_env {
        out.env.insert(k.clone(), fetch("secret_env", k, name)?);
    }
    Ok(out)
}

/// Opening of a header-secret placeholder: `Bearer {secret:mcp/github-token}`.
const SECRET_PLACEHOLDER_OPEN: &str = "{secret:";

/// Names a `secret_headers` value names, in order, so `validate()` can hold a
/// template's secret to the same rules as a plain name.  An empty result means
/// the value is a plain secret name; `Err` means an unterminated placeholder.
pub(crate) fn secret_header_names(spec: &str) -> std::result::Result<Vec<&str>, String> {
    let mut names = Vec::new();
    let mut rest = spec;
    while let Some(at) = rest.find(SECRET_PLACEHOLDER_OPEN) {
        let after = &rest[at + SECRET_PLACEHOLDER_OPEN.len()..];
        let Some(end) = after.find('}') else {
            return Err("has an unterminated secret placeholder".to_owned());
        };
        names.push(&after[..end]);
        rest = &after[end + 1..];
    }
    Ok(names)
}

/// Render one `secret_headers` value.
///
/// A value without a placeholder is a secret name, used exactly as before, so
/// every existing configuration keeps working.  A value that names its secret
/// once — `Bearer {secret:NAME}` — gets the fetched value substituted, which is
/// how a catalog entry asks for a bare token but sends a `Bearer` header.
///
/// The placeholder is counted and the name checked *before* anything is
/// fetched, so a malformed template never touches the store, and the fetched
/// value is never echoed in an error: the messages name the header and the
/// secret only.
fn render_header(
    fetch: &dyn Fn(&str, &str, &str) -> Result<String>,
    server: &str,
    header: &str,
    template: &str,
) -> Result<String> {
    let named = secret_header_names(template).map_err(|why| {
        McpError::Config(format!(
            "server `{server}`: `secret_headers.{header}` {why}"
        ))
    })?;
    if named.len() > 1 {
        return Err(McpError::Config(format!(
            "server `{server}`: `secret_headers.{header}` names {} secrets; exactly one is allowed",
            named.len()
        )));
    }
    // No placeholder: the value is the secret name, as it has always been.
    let Some(name) = named.first().copied() else {
        return fetch("secret_headers", header, template);
    };
    let value = fetch("secret_headers", header, name)?;
    // A header value cannot carry meaningful whitespace, and a token with a
    // stray newline is a paste accident. Refuse instead of guessing.
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(McpError::Config(format!(
            "server `{server}`: secret `{name}` for `secret_headers.{header}` is empty or contains whitespace"
        )));
    }
    // Exactly one placeholder, so one replacement is the whole template.
    let placeholder = format!("{SECRET_PLACEHOLDER_OPEN}{name}}}");
    Ok(template.replacen(&placeholder, &value, 1))
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
    // Resolve against the parent's PATH *before* clearing the child's env. A
    // launcher that is not installed is reported with an install hint before
    // anything is spawned.
    let mut cmd = which_command(command)
        .or_else(|e| {
            // Installed after the daemon started (in-app installer): look in
            // the usual per-user install folders before giving up.
            if e.kind() == std::io::ErrorKind::NotFound
                && let Some(found) = crate::runtime::find_tool(command, &[])
            {
                return Ok(tokio::process::Command::new(found));
            }
            Err(e)
        })
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                McpError::RuntimeMissing {
                    server: server.to_owned(),
                    command: command.clone(),
                    hint: runtime_install_hint(command),
                }
            } else {
                McpError::Spawn {
                    server: server.to_owned(),
                    message: format!("`{command}`: {e}"),
                }
            }
        })?;
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

/// How long a failed handshake waits for the child's last stderr lines (the
/// pipe closes as soon as the child is gone, so this is a ceiling, not a delay).
const STDERR_DRAIN: Duration = Duration::from_millis(500);

fn forward_stderr(
    server: String,
    stderr: tokio::process::ChildStderr,
    log: Arc<StderrLog>,
    redactor: Redactor,
    diagnostics: tracing::Dispatch,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(
        async move { capture_stderr(&server, stderr, &log, &redactor).await }
            .with_subscriber(diagnostics),
    )
}

/// Own the process tree until it has been killed and reaped. rmcp's graceful
/// close releases its wrapper as soon as the direct child exits, which can
/// leave grandchildren running. Cancellation must also kill synchronously:
/// runtime teardown may discard a newly spawned asynchronous cleanup task.
/// Stdout is read through [`FrameLimit`].
struct ChildTree {
    child: Option<Box<dyn ChildWrapper>>,
    io: ChildIo,
}

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
    fn spawn(mut cmd: CommandWrap) -> std::io::Result<(Self, Option<tokio::process::ChildStderr>)> {
        cmd.command_mut()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let (Some(stdin), Some(stdout)) = (
            child.inner_mut().stdin().take(),
            child.inner_mut().stdout().take(),
        ) else {
            let _ = ReapChild(Some(child));
            return Err(std::io::Error::other("child stdio was not piped"));
        };
        let stderr = child.inner_mut().stderr().take();
        let stdout = FrameLimit {
            inner: stdout,
            line: 0,
            cap: MAX_FRAME_BYTES,
        };
        Ok((
            Self {
                child: Some(child),
                io: AsyncRwTransport::new(stdout, stdin),
            },
            stderr,
        ))
    }

    fn take_child(&mut self) -> ReapChild {
        ReapChild(self.child.take())
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
        let send = self.child.as_ref().map(|_| self.io.send(item));
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
        self.child.as_ref()?;
        self.io.receive().await
    }

    async fn close(&mut self) -> std::io::Result<()> {
        // The guard kills the tree if this future is dropped at any await.
        let mut guard = self.take_child();
        if let Some(child) = guard.0.as_mut() {
            // Spec order: close the server's stdin and let it exit by itself,
            // so it can flush state and stop its own helpers.
            let _ = self.io.close().await;
            let exited = tokio::time::timeout(GRACEFUL_EXIT, child.wait())
                .await
                .is_ok_and(|status| status.is_ok());
            // Kill what is left either way: a server that ignored EOF, or the
            // wrapper tree (`npx`, `uvx`) of one that exited. After a clean
            // exit there may be nothing left to signal, which is not an error.
            let killed = child.start_kill();
            if !exited {
                killed?;
                child.wait().await?;
            }
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
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(url.to_owned())
        .max_sse_event_size(MAX_FRAME_BYTES);
    cfg.custom_headers = map;
    Ok(cfg)
}

/// A server installed from the registry was approved as "a public https
/// address", but a name can change what it resolves to. Resolve it now, refuse
/// any non-public answer, connect only to the vetted answers (no second
/// lookup, so no DNS rebinding), ignore proxies and follow no redirect.
async fn public_only(
    server: &str,
    url: &str,
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::ClientBuilder> {
    let refuse = |why: &str| McpError::Transport {
        server: server.to_owned(),
        message: format!("refused: {why}"),
    };
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse("the server address is invalid"))?;
    let addrs = tokio::time::timeout(
        Duration::from_secs(5),
        crate::oauth_net::PolicyHttpClient::new(false).vet(&parsed),
    )
    .await
    .map_err(|_| refuse("the server address could not be resolved in time"))?
    .map_err(|_| refuse("the server address is not a public https address"))?;
    let mut builder = builder
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if let Some(url::Host::Domain(name)) = parsed.host() {
        builder = builder.resolve_to_addrs(name, &addrs);
    }
    Ok(builder)
}

/// Connect and complete the MCP handshake, bounded by `timeout`.
pub(crate) async fn open(
    config: &McpServerConfig,
    secrets: &ResolvedSecrets,
    stderr: &Arc<StderrLog>,
    timeout: Duration,
    external: Option<ExternalRedactor>,
    signals: Arc<Signals>,
) -> Result<Session> {
    let redactor = Redactor::new(
        secrets
            .headers
            .values()
            .chain(secrets.env.values())
            .map(String::as_str),
        external,
    );
    let config = config.clone();
    let timeout = if matches!(config.transport, McpTransport::Http { .. }) {
        timeout.min(Duration::from_secs(5))
    } else {
        timeout
    };
    let secrets = secrets.clone();
    let stderr = Arc::clone(stderr);
    let diagnostics = tracing::dispatcher::get_default(Clone::clone);
    let stderr_redactor = redactor.clone();
    session::open(config.id.clone(), redactor, timeout, move || async move {
        let id = config.id.as_str();
        match &config.transport {
            McpTransport::Stdio { .. } => {
                let cmd = build_command(id, &config.transport, &secrets)?;
                let (transport, child_stderr) =
                    ChildTree::spawn(cmd).map_err(|e| McpError::Spawn {
                        server: id.to_owned(),
                        message: e.to_string(),
                    })?;
                let capture = child_stderr.map(|pipe| {
                    forward_stderr(id.to_owned(), pipe, stderr, stderr_redactor, diagnostics)
                });
                match (BridgeClient { signals }).serve(transport).await {
                    Ok(session) => Ok(session),
                    Err(e) => {
                        // The handshake failed: the child is being reaped, so
                        // its stderr hits EOF. Let the capture task store the
                        // last lines (its explanation) before the session
                        // runtime shuts down and the error is published.
                        if let Some(task) = capture {
                            let _ = tokio::time::timeout(STDERR_DRAIN, task).await;
                        }
                        Err(transport_err(id, e))
                    }
                }
            }
            McpTransport::Http { url, headers } => {
                // A disconnected service must fail before the long tool execution deadline.
                // No whole-request timeout: legitimate tools and SSE can run for longer.
                let mut builder =
                    reqwest::Client::builder().connect_timeout(Duration::from_secs(3));
                if config.import.is_some() {
                    builder = public_only(id, url, builder).await?;
                }
                let client = builder
                    .build()
                    .map_err(|e| transport_err(id, e.without_url()))?;
                let transport = StreamableHttpClientTransport::with_client(
                    client,
                    http_config(id, url, headers, &secrets.headers)?,
                );
                BridgeClient { signals }
                    .serve(transport)
                    .await
                    .map_err(|e| {
                        if e.is_authorization_required() {
                            McpError::AuthRequired(id.to_owned())
                        } else {
                            transport_err(id, e)
                        }
                    })
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::error::McpErrorKind;

    use super::*;

    #[tokio::test]
    async fn silent_http_handshake_fails_fast() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let holding = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let cfg = McpServerConfig::http("offline", format!("http://{address}/mcp"));
        let result = open(
            &cfg,
            &ResolvedSecrets::default(),
            &Arc::default(),
            Duration::from_secs(30),
            None,
            Arc::default(),
        )
        .await;
        holding.abort();
        assert!(
            matches!(result, Err(McpError::Timeout { seconds: 5, .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn registry_servers_are_not_connected_to_non_public_addresses() {
        for url in [
            "https://127.0.0.1/mcp",
            "https://localhost/mcp",
            "https://[::1]/mcp",
            "https://169.254.169.254/latest",
            "https://10.0.0.5/mcp",
            "http://192.168.1.1/mcp",
        ] {
            let mut cfg = McpServerConfig::http("imported", url);
            cfg.import = Some(crate::config::McpImport {
                bundle_sha256: None,
                secrets: vec![],
            });
            let result = open(
                &cfg,
                &ResolvedSecrets::default(),
                &Arc::default(),
                Duration::from_secs(30),
                None,
                Arc::default(),
            )
            .await;
            let Err(McpError::Transport { message, .. }) = &result else {
                panic!("{url}: {result:?}");
            };
            assert!(message.starts_with("refused:"), "{url}: {message}");
        }
    }

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

    /// A header value may name its secret once (`Bearer {secret:NAME}`) so a
    /// catalog entry can ask for a bare token and still send a scheme.  A value
    /// without a placeholder keeps its old meaning: a secret name, used as is.
    #[test]
    fn header_templates_substitute_one_secret_and_keep_old_values_working() {
        let store = Fake(BTreeMap::from([(
            "mcp/github-token".to_owned(),
            "ghp_example".to_owned(),
        )]));

        let mut templated = McpServerConfig::http("gh", "https://x/mcp");
        templated.secret_headers.insert(
            "Authorization".into(),
            "Bearer {secret:mcp/github-token}".into(),
        );
        let r = resolve_secrets(&templated, Some(&store)).unwrap();
        assert_eq!(r.headers["Authorization"], "Bearer ghp_example");
        assert!(!format!("{r:?}").contains("ghp_example"));

        // No placeholder: today's behaviour, the value is the secret name.
        let mut plain = McpServerConfig::http("gh", "https://x/mcp");
        plain
            .secret_headers
            .insert("Authorization".into(), "mcp/github-token".into());
        let r = resolve_secrets(&plain, Some(&store)).unwrap();
        assert_eq!(r.headers["Authorization"], "ghp_example");

        // A template that names two secrets, or none, is refused.
        let mut two = McpServerConfig::http("gh", "https://x/mcp");
        two.secret_headers.insert(
            "Authorization".into(),
            "{secret:mcp/github-token}{secret:mcp/github-token}".into(),
        );
        let err = resolve_secrets(&two, Some(&store)).unwrap_err().to_string();
        assert!(err.contains("exactly one"), "{err}");
        assert!(!err.contains("ghp_example"), "{err}");

        let mut unterminated = McpServerConfig::http("gh", "https://x/mcp");
        unterminated.secret_headers.insert(
            "Authorization".into(),
            "Bearer {secret:mcp/github-token".into(),
        );
        assert!(matches!(
            resolve_secrets(&unterminated, Some(&store)),
            Err(McpError::Config(_))
        ));
    }

    /// A token carrying whitespace (a wrapped paste) or nothing at all is
    /// refused with a message that names the secret, never the value.
    #[test]
    fn a_secret_with_whitespace_or_nothing_is_refused_in_a_header_template() {
        for (name, value) in [("mcp/padded", " ghp_example \n"), ("mcp/empty", "")] {
            let store = Fake(BTreeMap::from([(name.to_owned(), value.to_owned())]));
            let mut cfg = McpServerConfig::http("gh", "https://x/mcp");
            cfg.secret_headers
                .insert("Authorization".into(), format!("Bearer {{secret:{name}}}"));
            let err = resolve_secrets(&cfg, Some(&store)).unwrap_err().to_string();
            assert!(err.contains("empty or contains whitespace"), "{err}");
            assert!(!err.contains("ghp_example"), "{err}");
        }
    }

    /// A template naming a secret that was never stored fails the connect, and
    /// the message names the secret rather than repeating the backend's text.
    #[test]
    fn a_template_naming_a_missing_secret_fails_without_leaking() {
        let store = Fake(BTreeMap::new());
        let mut cfg = McpServerConfig::http("gh", "https://x/mcp");
        cfg.secret_headers
            .insert("Authorization".into(), "Bearer {secret:mcp/absent}".into());
        let err = resolve_secrets(&cfg, Some(&store)).unwrap_err().to_string();
        assert!(err.contains("cannot resolve secret `mcp/absent`"), "{err}");
    }

    /// Review finding 1: a configured header or child variable must never
    /// carry the stored OAuth record of this or another server.
    #[test]
    fn managed_oauth_records_cannot_be_exfiltrated_through_secret_references() {
        let record = r#"{"credentials":{"token_response":{"access_token":"victim-access-7c1"}}}"#;
        let store = Fake(BTreeMap::from([
            ("mcp/victim/oauth".to_owned(), record.to_owned()),
            ("MCP/Victim/OAuth".to_owned(), record.to_owned()),
        ]));
        for name in ["mcp/victim/oauth", "MCP/Victim/OAuth"] {
            let mut http = McpServerConfig::http("attacker", "https://attacker.example/mcp");
            http.secret_headers.insert("X-Stolen".into(), name.into());
            let mut stdio = McpServerConfig::stdio("child", "node", ["server.js"]);
            stdio.secret_env.insert("STOLEN".into(), name.into());
            for cfg in [&http, &stdio] {
                let err = cfg.validate().unwrap_err().to_string();
                assert!(err.contains("managed OAuth record"), "{err}");
                // Even a config that skipped validation (hand-edited file,
                // older version) cannot resolve it.
                let err = resolve_secrets(cfg, Some(&store)).unwrap_err().to_string();
                assert!(!err.contains("victim-access-7c1"), "{err}");
            }
        }
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
            None,
            Arc::default(),
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
        let error = open(
            &cfg,
            &secrets,
            &Arc::default(),
            Duration::from_secs(2),
            None,
            Arc::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, McpError::RuntimeMissing { .. }), "{error}");
        let message = error.to_string();
        assert!(!message.contains("environment-sentinel"));
        assert!(!message.contains("header-sentinel"));
        assert!(message.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn missing_command_is_a_runtime_missing_error_with_a_hint() {
        let cfg = McpServerConfig::stdio("s", "definitely-not-a-real-binary-xyz", [""; 0]);
        let err = open(
            &cfg,
            &ResolvedSecrets::default(),
            &Arc::default(),
            Duration::from_secs(2),
            None,
            Arc::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, McpError::RuntimeMissing { .. }), "{err}");
        assert_eq!(err.kind(), McpErrorKind::RuntimeMissing);
        assert!(err.to_string().contains("was not found on PATH"), "{err}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_cmd_shim_preserves_one_folder_argument() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("argv fixture.cmd");
        // This is an argv echo fixture, not an MCP server.
        std::fs::write(
            &shim,
            "@echo off\r\n@echo %1\r\n@if not [%2]==[] exit /b 9\r\n",
        )
        .unwrap();
        let folder = dir.path().join("chosen folder & echo injected");
        std::fs::create_dir(&folder).unwrap();
        let cfg = McpServerConfig::stdio(
            "argv",
            shim.display().to_string(),
            [folder.display().to_string()],
        );
        let mut cmd = build_command("argv", &cfg.transport, &ResolvedSecrets::default()).unwrap();
        cmd.command_mut().stdout(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let mut stdout = child.inner_mut().stdout().take().unwrap();
        let mut text = String::new();
        stdout.read_to_string(&mut text).await.unwrap();
        assert!(child.wait().await.unwrap().success());
        assert_eq!(text.trim(), format!("\"{}\"", folder.display()));
    }

    #[test]
    fn install_hints_name_the_runtime() {
        assert!(runtime_install_hint("npx").contains("nodejs.org"));
        assert!(runtime_install_hint("uvx.exe").contains("astral.sh"));
        assert!(runtime_install_hint("weird").contains("`weird`"));
    }

    #[test]
    fn launcher_environment_carries_path_and_no_credentials() {
        assert!(
            OS_ESSENTIAL_ENV
                .iter()
                .any(|k| k.eq_ignore_ascii_case("PATH")),
            "npx/uvx cannot find node/python without PATH"
        );
        for k in OS_ESSENTIAL_ENV {
            let u = k.to_ascii_uppercase();
            assert!(
                !["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "PROXY"]
                    .iter()
                    .any(|bad| u.contains(bad)),
                "{k} must not be inherited"
            );
        }
    }
}

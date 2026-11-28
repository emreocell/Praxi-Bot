//! Building blocks of OAuth for remote (streamable HTTP) MCP servers.
//!
//! The protocol work (protected-resource and authorization-server metadata
//! discovery, PKCE, issuer validation, refresh) is rmcp's `auth` module. This
//! module supplies the two parts that are ours:
//!
//! * [`SecretCredentialStore`]: rmcp's credential storage over the project's
//!   secret store. Tokens live under `mcp/<server>/oauth` and nowhere else;
//!   every token value is handed to the host redactor before it can be used.
//! * [`CallbackListener`]: the one-shot loopback redirect endpoint. It binds
//!   to `127.0.0.1` only, accepts exactly one request, requires the expected
//!   `state` and times out.
//!
//! Nothing here starts a flow by itself: a flow is started by the host on an
//! explicit user action.

use std::{net::Ipv4Addr, sync::Arc, time::Duration};

use oauth2::TokenResponse as _;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadata, AuthorizationRequest,
    AuthorizationSession, CredentialStore, OAuthClientConfig, StoredCredentials,
};
use secret_store::SecretStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use tracing::instrument::WithSubscriber as _;

use crate::error::{McpError, Result};

/// How long a user has to finish the sign-in in the browser.
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Largest redirect request that is read.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// Told about every token value before it is used, so the host can mask it
/// in logs, events and tool output (the daemon's `SharedRedactor::register`).
pub type SecretRegistrar = Arc<dyn Fn(&str) + Send + Sync>;

/// Secret-store name of a server's OAuth credentials.
#[must_use]
pub fn credential_name(server: &str) -> String {
    format!("mcp/{server}/oauth")
}

/// Secret-store name of the client identity a server's authorization server
/// issued by dynamic registration (review finding 10): kept so a refresh
/// after a restart can authenticate as the same confidential client.
#[must_use]
pub fn registered_client_name(server: &str) -> String {
    format!("mcp/{server}/oauth-client")
}

/// A dynamically registered client, bound to the issuer that registered it.
#[derive(serde::Serialize, serde::Deserialize)]
struct RegisteredClient {
    issuer: Option<String>,
    client_id: String,
    client_secret: String,
}

/// Keep the issued client identity, under the same guard as the tokens.
fn save_registered_client(
    store: &dyn SecretStore,
    server: &str,
    client: &RegisteredClient,
    guard: Option<(&CredentialGuard, u64)>,
) -> Result<()> {
    let value = serde_json::to_string(client)
        .map_err(|_| McpError::Store("cannot encode the registered client".into()))?;
    let write = || {
        store
            .set(&registered_client_name(server), &value)
            .map_err(|_| McpError::Store("cannot keep the registered OAuth client".into()))
    };
    match guard {
        None => write(),
        Some((guard, epoch)) => {
            let _write = guard.0.write.lock();
            if guard.epoch() == epoch {
                write()
            } else {
                Err(McpError::AuthRequired(server.to_owned()))
            }
        }
    }
}

/// The issued client secret for `client_id` at `issuer`, if one was kept.
fn registered_client_secret(
    store: &dyn SecretStore,
    server: &str,
    client_id: &str,
    issuer: Option<&str>,
) -> Option<String> {
    let value = store.get(&registered_client_name(server)).ok()??;
    let client: RegisteredClient = serde_json::from_str(value.expose()).ok()?;
    let issuer_matches = match (client.issuer.as_deref(), issuer) {
        (Some(kept), Some(now)) => same_issuer(kept, now),
        (None, None) => true,
        _ => false,
    };
    (client.client_id == client_id && issuer_matches).then_some(client.client_secret)
}

/// Orders every write of one server's OAuth record against sign-out and
/// removal.  A credential store made with [`SecretCredentialStore::guarded`]
/// remembers the epoch it started in; [`CredentialGuard::revoke`] moves the
/// epoch and runs its delete under the same write lock, so a sign-in exchange
/// or refresh that was in flight either saved before the delete (and is
/// deleted) or finds the epoch moved and saves nothing.  Signing out can
/// therefore never be undone by an older writer.
#[derive(Clone, Default)]
pub struct CredentialGuard(Arc<CredentialGuardInner>);

#[derive(Default)]
struct CredentialGuardInner {
    epoch: std::sync::atomic::AtomicU64,
    write: parking_lot::Mutex<()>,
}

impl CredentialGuard {
    /// The current epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.0.epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Invalidate every writer that started before this call, then run `f`
    /// (typically the delete) while no writer can save.
    pub fn revoke<R>(&self, f: impl FnOnce() -> R) -> R {
        let _write = self.0.write.lock();
        self.0
            .epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        f()
    }
}

impl std::fmt::Debug for CredentialGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialGuard")
            .field("epoch", &self.epoch())
            .finish()
    }
}

/// OAuth credentials of one MCP server, kept in the secret store.
pub struct SecretCredentialStore {
    store: Arc<dyn SecretStore>,
    name: String,
    /// Canonical URL of the MCP server the credentials were issued for.
    resource: String,
    registrar: Option<SecretRegistrar>,
    /// The guard and the epoch this store started in.
    guard: Option<(CredentialGuard, u64)>,
}

/// What is written to the secret store: the credentials and the server they
/// belong to. rmcp binds stored tokens to the issuer; the resource binding is
/// ours, so a token is never offered to another URL configured under the
/// same server name.
#[derive(serde::Serialize, serde::Deserialize)]
struct Bound {
    resource: String,
    credentials: StoredCredentials,
}

/// Scheme, host and port in lower case plus the path and the query, without
/// the fragment: the identity of an MCP server for credential binding.  The
/// query is part of it (review finding 11): a gateway that selects the MCP
/// resource by query (`?tenant=A`) must not receive A's token for B.
///
/// # Errors
/// `url` is not an absolute `http(s)` URL.
pub fn canonical_resource(url: &str) -> Result<String> {
    let bad = || McpError::Config(format!("`{url}` is not an absolute http(s) URL"));
    let uri: http::Uri = url
        .split('#')
        .next()
        .unwrap_or(url)
        .parse()
        .map_err(|_| bad())?;
    let scheme = uri
        .scheme_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(bad)?;
    let authority = uri
        .authority()
        .map(|a| a.as_str().to_ascii_lowercase())
        .ok_or_else(bad)?;
    if !matches!(scheme.as_str(), "http" | "https") || authority.contains('@') {
        return Err(bad());
    }
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    Ok(format!("{scheme}://{authority}{}{query}", uri.path()))
}

impl std::fmt::Debug for SecretCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretCredentialStore")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl SecretCredentialStore {
    /// Credentials of `server`, reachable at `url`, in `store`.
    ///
    /// # Errors
    /// `url` is not an absolute `http(s)` URL.
    pub fn new(
        store: Arc<dyn SecretStore>,
        server: &str,
        url: &str,
        registrar: Option<SecretRegistrar>,
    ) -> Result<Self> {
        Ok(Self {
            store,
            name: credential_name(server),
            resource: canonical_resource(url)?,
            registrar,
            guard: None,
        })
    }

    /// Tie this store to `guard` as of now: after the next
    /// [`CredentialGuard::revoke`] it neither loads nor saves.
    #[must_use]
    pub fn guarded(mut self, guard: Option<&CredentialGuard>) -> Self {
        self.guard = guard.map(|g| (g.clone(), g.epoch()));
        self
    }

    /// Whether a revoke happened since this store was made.
    #[must_use]
    pub fn revoked(&self) -> bool {
        self.guard
            .as_ref()
            .is_some_and(|(guard, epoch)| guard.epoch() != *epoch)
    }

    /// Hand the token values inside `serialized` to the redactor.
    fn register(&self, serialized: &str) {
        let Some(registrar) = &self.registrar else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(serialized) else {
            return;
        };
        for field in ["access_token", "refresh_token", "id_token"] {
            if let Some(token) = value["credentials"]["token_response"][field].as_str()
                && !token.is_empty()
            {
                registrar(token);
            }
        }
    }
}

/// A backend error may quote what it was asked to store: keep only its kind.
fn store_error(action: &str) -> AuthError {
    AuthError::CredentialStoreError(format!("secret store could not {action} OAuth credentials"))
}

#[async_trait::async_trait]
impl CredentialStore for SecretCredentialStore {
    async fn load(&self) -> std::result::Result<Option<StoredCredentials>, AuthError> {
        if self.revoked() {
            return Ok(None);
        }
        let Some(value) = self
            .store
            .get(&self.name)
            .map_err(|_| store_error("read"))?
        else {
            return Ok(None);
        };
        // Registered even when it is refused below: it is still a live token.
        self.register(value.expose());
        let bound: Bound =
            serde_json::from_str(value.expose()).map_err(|_| store_error("decode"))?;
        if bound.resource != self.resource {
            // The server name now points somewhere else. These credentials
            // belong to the old URL and must not be sent to the new one.
            tracing::warn!(
                secret = %self.name,
                "stored MCP OAuth credentials belong to a different server URL; discarding them"
            );
            self.store
                .delete(&self.name)
                .map_err(|_| store_error("delete"))?;
            return Ok(None);
        }
        Ok(Some(bound.credentials))
    }

    async fn save(&self, credentials: StoredCredentials) -> std::result::Result<(), AuthError> {
        let serialized = serde_json::to_string(&Bound {
            resource: self.resource.clone(),
            credentials,
        })
        .map_err(|_| store_error("encode"))?;
        // Before the store write: the value must be masked by the time anything
        // could log it.
        self.register(&serialized);
        let write = || {
            self.store
                .set(&self.name, &serialized)
                .map_err(|_| store_error("write"))
        };
        match &self.guard {
            None => write(),
            Some((guard, epoch)) => {
                // Checked and written under the guard's lock: a revoke is
                // either entirely before (refused here) or entirely after.
                let _write = guard.0.write.lock();
                if guard.epoch() == *epoch {
                    write()
                } else {
                    Err(AuthError::CredentialStoreError(
                        "the sign-in was revoked meanwhile; credentials were not stored".into(),
                    ))
                }
            }
        }
    }

    async fn clear(&self) -> std::result::Result<(), AuthError> {
        let delete = || {
            self.store
                .delete(&self.name)
                .map(|_| ())
                .map_err(|_| store_error("delete"))
        };
        match &self.guard {
            None => delete(),
            Some((guard, _)) => {
                let _write = guard.0.write.lock();
                delete()
            }
        }
    }
}

/// Refuse authorization-server metadata the flow must not be run against.
///
/// rmcp tolerates metadata without `code_challenge_methods_supported`; the MCP
/// authorization spec says a client must refuse to proceed then, so that is
/// enforced here. Endpoints must be `https`; plain `http` is accepted only for
/// a loopback host and only when the host opted in (local development).
///
/// # Errors
/// [`McpError::Config`] naming the rule that was broken.
pub fn check_metadata(
    server: &str,
    metadata: &AuthorizationMetadata,
    allow_loopback_http: bool,
) -> Result<()> {
    let refuse = |why: &str| {
        Err(McpError::Config(format!(
            "server `{server}`: authorization server refused: {why}"
        )))
    };
    if !metadata
        .code_challenge_methods_supported
        .as_ref()
        .is_some_and(|methods| methods.iter().any(|m| m == "S256"))
    {
        return refuse("it does not advertise PKCE with S256");
    }
    for endpoint in [&metadata.authorization_endpoint, &metadata.token_endpoint] {
        let Ok(uri) = endpoint.parse::<http::Uri>() else {
            return refuse("an endpoint is not a valid URL");
        };
        let host = uri.host().unwrap_or_default().trim_matches(['[', ']']);
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        match uri.scheme_str() {
            Some("https") => {}
            Some("http") if loopback && allow_loopback_http => {}
            _ => return refuse("its endpoints must use https"),
        }
    }
    Ok(())
}

/// Client identity and policy of one sign-in.
#[derive(Clone, Default)]
pub struct SignInOptions {
    /// Client id registered with the authorization server out of band.
    pub client_id: Option<String>,
    /// Secret of that client (resolved from the secret store by the host).
    pub client_secret: Option<String>,
    /// HTTPS URL of our Client ID Metadata Document, if one is published.
    pub client_metadata_url: Option<String>,
    /// `WWW-Authenticate` value of the server's 401, to seed discovery.
    pub challenge: Option<String>,
    /// Accept `http` endpoints on loopback (local development only).
    pub allow_loopback_http: bool,
    /// Orders this flow's credential writes against sign-out (set by the
    /// manager; `None` in standalone use).
    pub guard: Option<CredentialGuard>,
    /// The issuer a pre-registered client (and its secret) is bound to.
    /// Required when `client_secret` is set; compared whenever present.
    pub expected_issuer: Option<String>,
    /// DNS used by the OAuth policy client; `None` = the system resolver.
    /// Tests inject a static table so no real lookup happens.
    pub resolver: Option<Arc<dyn EndpointResolver>>,
    /// Scopes to request; empty lets the library pick from the server's metadata.
    pub scopes: Vec<String>,
    /// Also ask for a refresh token (`access_type=offline`, `prompt=consent`).
    pub offline_access: bool,
    /// The browser is NOT on this machine: it is sent back to
    /// `http://127.0.0.1:<port>/callback` on the owner's own computer, where
    /// the desktop app catches the redirect and forwards it (with
    /// [`SignIn::finish_or_pasted`]'s paste channel). No listener is opened
    /// here. `None` = this machine runs the browser and listens itself.
    pub redirect_port: Option<u16>,
}

/// Lowest port a relayed redirect may name: never a well-known service port.
pub const MIN_RELAY_PORT: u16 = 1024;

/// `https://a.example/` and `https://a.example` name the same issuer.
fn same_issuer(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

impl SignInOptions {
    /// Refuse to present a pre-registered client to any issuer other than
    /// the one it is bound to (review finding 5).  Runs before the client is
    /// configured, so a mismatched server never receives its id or secret.
    fn check_issuer(&self, server: &str, advertised: Option<&str>) -> Result<()> {
        if self.client_id.is_none() && self.client_secret.is_none() {
            return Ok(());
        }
        let Some(expected) = &self.expected_issuer else {
            if self.client_secret.is_some() {
                return Err(McpError::Config(format!(
                    "server `{server}`: a confidential OAuth client needs a configured issuer"
                )));
            }
            return Ok(());
        };
        match advertised {
            Some(actual) if same_issuer(actual, expected) => Ok(()),
            _ => Err(McpError::Config(format!(
                "server `{server}`: authorization server refused: it is not the issuer `{expected}` the configured client is registered with"
            ))),
        }
    }

    /// The policy client every OAuth request of this flow goes through.
    fn http_client(&self) -> Arc<crate::oauth_net::PolicyHttpClient> {
        Arc::new(match &self.resolver {
            Some(resolver) => crate::oauth_net::PolicyHttpClient::with_resolver(
                self.allow_loopback_http,
                Arc::clone(resolver),
            ),
            None => crate::oauth_net::PolicyHttpClient::new(self.allow_loopback_http),
        })
    }
}

impl std::fmt::Debug for SignInOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInOptions")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("client_metadata_url", &self.client_metadata_url)
            .field("allow_loopback_http", &self.allow_loopback_http)
            .field("scopes", &self.scopes)
            .field("offline_access", &self.offline_access)
            .field("redirect_port", &self.redirect_port)
            .finish_non_exhaustive()
    }
}

/// An OAuth error may quote a server response; keep it short and one line.
/// An OAuth failure as a fixed category (review finding 6).  rmcp's and
/// oauth2's texts are never used: an authorization server can reflect the
/// submitted refresh token, client secret, verifier or code in
/// `error_description`, and URLs can carry codes.  Only our own policy
/// refusals (which name a host and nothing else) are added.
fn auth_error(server: &str, error: &AuthError) -> McpError {
    let category = match error {
        AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_) => {
            "authorization is required"
        }
        AuthError::AuthorizationFailed(_) => "the authorization server refused the request",
        AuthError::TokenExchangeFailed(_) => "the authorization code was not accepted",
        AuthError::TokenRefreshFailed(_) => "the access token could not be refreshed",
        AuthError::CredentialStoreError(_) => "the secret store could not keep the credentials",
        AuthError::HttpError(_) => "the authorization server could not be reached",
        AuthError::MetadataError(_)
        | AuthError::DiscoveryRequestFailed { .. }
        | AuthError::ProtectedResourceMetadataInvalid { .. }
        | AuthError::AuthorizationServersUnavailable { .. }
        | AuthError::NoAuthorizationSupport => "authorization server discovery failed",
        AuthError::PkceUnsupported => "the authorization server does not support PKCE (S256)",
        AuthError::UrlError(_) => "an OAuth URL is invalid",
        AuthError::InvalidTokenType(_) => "the token type is not supported",
        AuthError::TokenExpired => "the token expired",
        AuthError::InvalidScope(_) | AuthError::InsufficientScope { .. } => {
            "the requested scope was refused"
        }
        AuthError::RegistrationFailed(_) => "client registration failed",
        AuthError::AuthorizationServerMismatch { .. }
        | AuthError::AuthorizationServerMissingIssuer { .. } => {
            "the authorization server's issuer does not match"
        }
        _ => "OAuth error",
    };
    // Our own refusal anywhere in the source chain: its text is ours.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = source {
        if let Some(policy) = e.downcast_ref::<crate::oauth_net::OAuthNetError>() {
            return flow_error(server, &format!("{category}: {policy}"));
        }
        source = e.source();
    }
    flow_error(server, category)
}

/// A dispatcher that drops every event: OAuth work runs under it so a
/// dependency's debug output (rmcp logs the authorization code and the
/// token response) can never reach a log, whatever subscriber is installed.
fn quiet() -> tracing::Dispatch {
    tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default())
}

/// A sign-in the user started: discovery and client registration are done,
/// the browser has to be sent to [`SignIn::authorization_url`], and
/// [`SignIn::finish`] waits for the redirect and stores the tokens.
///
/// Only the host may create one, on an explicit user action. Nothing in this
/// crate starts a sign-in on behalf of a server, a tool or a model.
pub struct SignIn {
    server: String,
    session: AuthorizationSession,
    redirect: Redirect,
    authorization_url: String,
    state: String,
}

/// Where the authorization server sends the browser back to.
#[derive(Debug)]
enum Redirect {
    /// A one-shot listener on this machine's loopback.
    Listener(CallbackListener),
    /// A loopback address on another computer (the owner's), whose redirect
    /// arrives only through the paste channel.
    Relayed { port: u16 },
}

impl Redirect {
    fn uri(&self) -> String {
        match self {
            Self::Listener(listener) => listener.redirect_uri(),
            Self::Relayed { port } => format!("http://127.0.0.1:{port}/callback"),
        }
    }
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl SignIn {
    /// Discover the authorization server of the MCP server at `url`, check
    /// it, pick a client identity (pre-registered, then Client ID Metadata
    /// Document, then dynamic registration) and open the redirect listener.
    ///
    /// # Errors
    /// Discovery, policy ([`check_metadata`], rmcp's address filter) or
    /// registration failure. Nothing is stored in that case.
    pub async fn start(
        server: &str,
        url: &str,
        store: Arc<dyn SecretStore>,
        registrar: Option<SecretRegistrar>,
        options: SignInOptions,
    ) -> Result<Self> {
        Self::start_quietly(server, url, store, registrar, options)
            .with_subscriber(quiet())
            .await
    }

    async fn start_quietly(
        server: &str,
        url: &str,
        store: Arc<dyn SecretStore>,
        registrar: Option<SecretRegistrar>,
        options: SignInOptions,
    ) -> Result<Self> {
        let registered_store = Arc::clone(&store);
        let guard_epoch = options.guard.as_ref().map(|g| (g.clone(), g.epoch()));
        let credentials = SecretCredentialStore::new(store, server, url, registrar.clone())?
            .guarded(options.guard.as_ref());
        // Every OAuth request (discovery, registration, exchange) goes through
        // the policy client: no proxy, vetted addresses on every hop.
        let http = options.http_client();
        let mut manager = AuthorizationManager::new_with_oauth_http_client(url, http.clone())
            .await
            .map_err(|e| auth_error(server, &e))?;
        manager.set_credential_store(credentials);
        let resolution = manager
            .resolve_metadata_from_challenge(options.challenge.as_deref())
            .await
            .map_err(|e| auth_error(server, &e))?;
        check_metadata(server, &resolution.metadata, options.allow_loopback_http)?;
        check_endpoint_addresses(server, &http, &resolution.metadata).await?;
        let use_metadata_document = options.client_metadata_url.is_some()
            && resolution
                .metadata
                .additional_fields
                .get("client_id_metadata_document_supported")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
        if !use_metadata_document {
            options.check_issuer(server, resolution.metadata.issuer.as_deref())?;
        }
        let issuer = resolution.metadata.issuer.clone();
        manager.set_metadata(resolution.metadata);

        let redirect = match options.redirect_port {
            Some(port) if port < MIN_RELAY_PORT => {
                return Err(flow_error(server, "the redirect port is not allowed"));
            }
            Some(port) => Redirect::Relayed { port },
            None => Redirect::Listener(CallbackListener::bind(server).await?),
        };
        let redirect_uri = redirect.uri();
        let mut request = AuthorizationRequest::new(redirect_uri.clone()).with_client_name("Praxi");
        if !options.scopes.is_empty() {
            request = request.with_scopes(options.scopes);
        }
        if let Some(id) = options.client_id.filter(|_| !use_metadata_document) {
            request = request.with_preregistered_client(id);
            if let Some(secret) = options.client_secret {
                request = request.with_client_secret(secret);
            }
        } else if !use_metadata_document {
            // Dynamic registration, done here (not inside rmcp's session) so a
            // client secret the server issues is kept for later refreshes.
            let client = manager
                .register_client("Praxi", &redirect_uri, &[])
                .await
                .map_err(|e| auth_error(server, &e))?;
            request = request.with_preregistered_client(client.client_id.clone());
            if let Some(secret) = client.client_secret {
                if let Some(register) = &registrar {
                    register(&secret);
                }
                save_registered_client(
                    registered_store.as_ref(),
                    server,
                    &RegisteredClient {
                        issuer: issuer.clone(),
                        client_id: client.client_id,
                        client_secret: secret.clone(),
                    },
                    guard_epoch.as_ref().map(|(g, e)| (g, *e)),
                )?;
                request = request.with_client_secret(secret);
            }
        }
        if let Some(document) = options.client_metadata_url {
            request = request.with_client_metadata_url(document);
        }
        if let Some(challenge) = options.challenge {
            request = request.with_challenge(challenge);
        }
        let session = AuthorizationSession::new(manager, request)
            .await
            .map_err(|(_, e)| auth_error(server, &e))?;
        let mut authorization_url = session.get_authorization_url().to_owned();
        let state = query_param(&authorization_url, "state")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| flow_error(server, "the authorization URL carries no `state`"))?;
        if options.offline_access && query_param(&authorization_url, "access_type").is_none() {
            authorization_url.push_str("&access_type=offline&prompt=consent");
        }
        Ok(Self {
            server: server.to_owned(),
            session,
            redirect,
            authorization_url,
            state,
        })
    }

    /// The URL the user's browser must open.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// The address the authorization server sends the browser back to.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        self.redirect.uri()
    }

    /// Whether the redirect is caught on another computer and has to come in
    /// through the paste channel ([`SignInOptions::redirect_port`]).
    #[must_use]
    pub fn is_relayed(&self) -> bool {
        matches!(self.redirect, Redirect::Relayed { .. })
    }

    /// Wait for the redirect, exchange the code (PKCE verifier, `resource`,
    /// RFC 9207 issuer check are rmcp's) and store the tokens.
    ///
    /// # Errors
    /// Timeout, a wrong or replayed redirect, or a refused exchange.
    pub async fn finish(self, timeout: Duration) -> Result<()> {
        self.finish_or_pasted(timeout, None).await
    }

    /// [`SignIn::finish`] for a host without a local browser: whichever comes
    /// first decides, the redirect on the loopback listener or the redirect
    /// URL the user copied from the browser's address bar and pasted (sent on
    /// `pasted`). Both pass the same checks: the URL must be our
    /// `redirect_uri`, the `state` is compared before anything else, and the
    /// flow is consumed by the first answer, right or wrong.
    ///
    /// # Errors
    /// As [`SignIn::finish`]; a pasted URL that is not our redirect.
    pub async fn finish_or_pasted(
        self,
        timeout: Duration,
        pasted: Option<tokio::sync::oneshot::Receiver<String>>,
    ) -> Result<()> {
        self.finish_quietly(timeout, pasted)
            .with_subscriber(quiet())
            .await
    }

    async fn finish_quietly(
        self,
        timeout: Duration,
        pasted: Option<tokio::sync::oneshot::Receiver<String>>,
    ) -> Result<()> {
        let redirect_uri = self.redirect.uri();
        let callback = match (self.redirect, pasted) {
            (Redirect::Listener(listener), None) => {
                listener.wait(&self.server, &self.state, timeout).await?
            }
            (Redirect::Listener(listener), Some(pasted)) => {
                tokio::select! {
                    received = listener.wait(&self.server, &self.state, timeout) => received?,
                    Ok(url) = pasted => {
                        pasted_callback(&self.server, &redirect_uri, &url, &self.state)?
                    }
                }
            }
            // Nothing listens here: the redirect can only be forwarded.
            (Redirect::Relayed { .. }, None) => {
                return Err(flow_error(
                    &self.server,
                    "a relayed sign-in needs the redirect to be forwarded",
                ));
            }
            (Redirect::Relayed { .. }, Some(pasted)) => {
                let url = tokio::time::timeout(timeout, pasted)
                    .await
                    .map_err(|_| {
                        flow_error(&self.server, "no redirect arrived before the time limit")
                    })?
                    .map_err(|_| flow_error(&self.server, "the sign-in was cancelled"))?;
                pasted_callback(&self.server, &redirect_uri, &url, &self.state)?
            }
        };
        self.session
            .handle_callback_with_issuer(&callback.code, &self.state, callback.issuer.as_deref())
            .await
            .map(|_| ())
            .map_err(|e| auth_error(&self.server, &e))
    }
}

/// An access token to connect with, and when (Unix seconds) it expires.
pub struct Bearer {
    /// The access token.
    pub token: String,
    /// Expiry, when the authorization server said.
    pub expires_at: Option<u64>,
}

impl std::fmt::Debug for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bearer")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Unix seconds now.
#[must_use]
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// When the stored access token expires (Unix seconds), if known.
fn expires_at(stored: &StoredCredentials) -> Option<u64> {
    let expiry = stored
        .token_response
        .as_ref()
        .and_then(oauth2::TokenResponse::expires_in)?;
    Some(stored.token_received_at?.saturating_add(expiry.as_secs()))
}

/// The refresh window: a token with at most this many seconds left is
/// refreshed before it is used.
pub const REFRESH_WINDOW_SECS: u64 = 60;

/// Whether the stored sign-in may be refreshed with the configured client:
/// the advertised issuer is the bound one, and the stored credentials were
/// issued by it (review finding 5).  The client secret goes to the token
/// endpoint only when this holds.
fn issuer_bound(
    server: &str,
    options: &SignInOptions,
    advertised: Option<&str>,
    stored: &StoredCredentials,
) -> bool {
    options.check_issuer(server, advertised).is_ok()
        && !options.expected_issuer.as_ref().is_some_and(|expected| {
            stored
                .issuer
                .as_deref()
                .is_some_and(|issued| !same_issuer(issued, expected))
        })
}

/// Whether at most 60 seconds of the stored access token remain.
fn refresh_due(stored: &StoredCredentials) -> bool {
    let expiry = stored
        .token_response
        .as_ref()
        .and_then(oauth2::TokenResponse::expires_in);
    match (expiry, stored.token_received_at) {
        (Some(expiry), Some(received)) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            received
                .saturating_add(expiry.as_secs())
                .saturating_sub(now)
                <= 60
        }
        _ => false,
    }
}

/// The access token to connect `server` (at `url`) with, refreshed first when
/// at most 60 seconds remain. `Ok(None)` when no sign-in was ever stored for this
/// server and URL: nothing is contacted then.
///
/// # Errors
/// [`McpError::AuthRequired`] when stored credentials can no longer be used
/// (issuer changed, refresh token missing or rejected); a transport error for
/// a transient failure of the authorization server.
pub async fn stored_bearer(
    server: &str,
    url: &str,
    store: &Arc<dyn SecretStore>,
    registrar: Option<&SecretRegistrar>,
    force_refresh: bool,
    options: &SignInOptions,
) -> Result<Option<Bearer>> {
    stored_bearer_quietly(server, url, store, registrar, force_refresh, options)
        .with_subscriber(quiet())
        .await
}

async fn stored_bearer_quietly(
    server: &str,
    url: &str,
    store: &Arc<dyn SecretStore>,
    registrar: Option<&SecretRegistrar>,
    force_refresh: bool,
    options: &SignInOptions,
) -> Result<Option<Bearer>> {
    let probe = SecretCredentialStore::new(Arc::clone(store), server, url, registrar.cloned())?
        .guarded(options.guard.as_ref());
    let revoked = {
        let (guard, epoch) = (
            options.guard.clone(),
            options.guard.as_ref().map(CredentialGuard::epoch),
        );
        move || guard.as_ref().map(CredentialGuard::epoch) != epoch
    };
    let Some(stored) = probe
        .load()
        .await
        .map_err(|e| auth_error(server, &e))?
        .filter(|c| c.token_response.is_some())
    else {
        return Ok(None);
    };
    let refresh_due = refresh_due(&stored);
    // The policy client: no proxy, vetted addresses on every hop.
    let http = options.http_client();
    let mut manager = AuthorizationManager::new_with_oauth_http_client(url, http.clone())
        .await
        .map_err(|e| auth_error(server, &e))?;
    manager.set_credential_store(probe);
    // Apply our proxy-independent endpoint policy on refresh as well as sign-in.
    let resolution = manager
        .resolve_metadata()
        .await
        .map_err(|e| auth_error(server, &e))?;
    check_metadata(server, &resolution.metadata, options.allow_loopback_http)?;
    check_endpoint_addresses(server, &http, &resolution.metadata).await?;
    let advertised_issuer = resolution.metadata.issuer.clone();
    manager.set_metadata(resolution.metadata);
    match manager.initialize_from_store().await {
        Ok(true) => {}
        Ok(false) => return Err(McpError::AuthRequired(server.to_owned())),
        Err(e) => return Err(auth_error(server, &e)),
    }
    let stored_id = &stored.client_id;
    let metadata_client = options.client_metadata_url.as_ref() == Some(stored_id);
    if !metadata_client && !issuer_bound(server, options, advertised_issuer.as_deref(), &stored) {
        return Err(McpError::AuthRequired(server.to_owned()));
    }
    if !metadata_client && options.client_id.as_ref().is_some_and(|id| id != stored_id) {
        return Err(McpError::AuthRequired(server.to_owned()));
    }
    // Reinstall the confidential identity after initialize_from_store, which
    // restores only the public client id. Refresh never reads secrets from config:
    // a pre-registered client's secret comes from its reference, a dynamically
    // registered one's from the record kept at registration (same id and issuer).
    let mut client = OAuthClientConfig::new(stored_id, "http://127.0.0.1/callback");
    let registered = (!metadata_client && options.client_id.is_none())
        .then(|| {
            registered_client_secret(
                store.as_ref(),
                server,
                stored_id,
                advertised_issuer.as_deref(),
            )
        })
        .flatten();
    if let (Some(secret), Some(register)) = (&registered, registrar) {
        register(secret);
    }
    if !metadata_client && let Some(secret) = options.client_secret.as_ref().or(registered.as_ref())
    {
        client = client.with_client_secret(secret);
    }
    manager
        .configure_client(client)
        .map_err(|e| auth_error(server, &e))?;
    let token = if force_refresh || refresh_due {
        manager
            .refresh_token()
            .await
            .map(|t| t.access_token().secret().clone())
    } else {
        manager.get_access_token().await
    };
    match token {
        Ok(token) => {
            if let Some(registrar) = registrar {
                registrar(&token);
            }
            // The expiry of what is stored now (refreshed or not).
            let expires_at = SecretCredentialStore::new(Arc::clone(store), server, url, None)?
                .load()
                .await
                .ok()
                .flatten()
                .as_ref()
                .and_then(expires_at);
            Ok(Some(Bearer { token, expires_at }))
        }
        // Signed out while refreshing: the refreshed token was not stored and
        // is not used.
        Err(_) if revoked() => Err(McpError::AuthRequired(server.to_owned())),
        Err(AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_)) => {
            Err(McpError::AuthRequired(server.to_owned()))
        }
        Err(e) => Err(auth_error(server, &e)),
    }
}

/// The token to hand back to the authorization server on sign-out, read
/// before the local copy is forgotten.
pub struct TokenToRevoke {
    token: String,
    hint: &'static str,
    client_id: String,
    issuer: Option<String>,
}

impl std::fmt::Debug for TokenToRevoke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenToRevoke").finish_non_exhaustive()
    }
}

/// How long sign-out waits for the authorization server to confirm.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);

/// The stored sign-in of `server` at `url` as something to revoke: the refresh
/// token when there is one (the server ends the access tokens with it), else
/// the access token. No network; `None` when nothing is stored.
pub async fn token_to_revoke(
    server: &str,
    url: &str,
    store: &Arc<dyn SecretStore>,
) -> Option<TokenToRevoke> {
    let stored = SecretCredentialStore::new(Arc::clone(store), server, url, None)
        .ok()?
        .load()
        .await
        .ok()
        .flatten()?;
    let response = stored.token_response.as_ref()?;
    let (token, hint) = match response.refresh_token() {
        Some(refresh) => (refresh.secret().clone(), "refresh_token"),
        None => (response.access_token().secret().clone(), "access_token"),
    };
    Some(TokenToRevoke {
        token,
        hint,
        client_id: stored.client_id.clone(),
        issuer: stored.issuer.clone(),
    })
}

/// Ask the authorization server to end a sign-in (RFC 7009), so removing
/// Praxi here also removes its access there. Best effort: `Ok(false)` when
/// the server advertises no `revocation_endpoint`, and the caller ignores an
/// error (the local copy is already gone). The endpoint is held to the same
/// policy as the others; the client secret goes along only to the issuer the
/// client is bound to.
///
/// # Errors
/// Discovery, policy or transport failure, or a refusal by the server.
pub async fn revoke_at_issuer(
    server: &str,
    url: &str,
    revoke: TokenToRevoke,
    options: &SignInOptions,
) -> Result<bool> {
    let http = options.http_client();
    let manager = AuthorizationManager::new_with_oauth_http_client(url, http.clone())
        .await
        .map_err(|e| auth_error(server, &e))?;
    let metadata = manager
        .resolve_metadata()
        .await
        .map_err(|e| auth_error(server, &e))?
        .metadata;
    check_metadata(server, &metadata, options.allow_loopback_http)?;
    let Some(endpoint) = metadata
        .additional_fields
        .get("revocation_endpoint")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(false);
    };
    let endpoint = reqwest::Url::parse(endpoint).map_err(|_| {
        McpError::Config(format!(
            "server `{server}`: authorization server refused: an endpoint is not a valid URL"
        ))
    })?;
    let bound = options
        .check_issuer(server, metadata.issuer.as_deref())
        .is_ok()
        && !options.expected_issuer.as_ref().is_some_and(|expected| {
            revoke
                .issuer
                .as_deref()
                .is_some_and(|issued| !same_issuer(issued, expected))
        });
    let mut form = vec![
        ("token", revoke.token.as_str()),
        ("token_type_hint", revoke.hint),
        ("client_id", revoke.client_id.as_str()),
    ];
    if let Some(secret) = options.client_secret.as_deref().filter(|_| bound) {
        form.push(("client_secret", secret));
    }
    let status = http
        .post_form(endpoint, &form, REVOKE_TIMEOUT)
        .await
        .map_err(|e| McpError::Transport {
            server: server.to_owned(),
            message: format!("revoking the sign-in failed: {e}"),
        })?;
    if status.is_success() {
        Ok(true)
    } else {
        Err(McpError::Transport {
            server: server.to_owned(),
            message: format!("revoking the sign-in was refused ({status})"),
        })
    }
}

/// DNS lookup used by the OAuth policy client, independent of any proxy.
/// Injectable so policy tests never use real DNS.
#[async_trait::async_trait]
pub trait EndpointResolver: Send + Sync {
    /// Resolve all answers; absence or an empty answer fails closed.
    async fn lookup(&self, host: &str, port: u16) -> Option<Vec<std::net::IpAddr>>;
}

pub(crate) struct SystemEndpointResolver;

#[async_trait::async_trait]
impl EndpointResolver for SystemEndpointResolver {
    async fn lookup(&self, host: &str, port: u16) -> Option<Vec<std::net::IpAddr>> {
        Some(
            tokio::net::lookup_host((host, port))
                .await
                .ok()?
                .map(|a| a.ip())
                .collect(),
        )
    }
}

/// Vet the advertised authorization, token and registration endpoints with
/// the same policy the OAuth client enforces on every request
/// ([`crate::oauth_net`]).  The authorization endpoint is only opened in the
/// user's browser, so this is where it is checked.
///
/// # Errors
/// [`McpError::Config`] when an endpoint is unresolvable, uses a refused
/// scheme or resolves to a refused address.
pub async fn check_endpoint_addresses(
    server: &str,
    client: &crate::oauth_net::PolicyHttpClient,
    metadata: &AuthorizationMetadata,
) -> Result<()> {
    let endpoints = [
        Some(&metadata.authorization_endpoint),
        Some(&metadata.token_endpoint),
        metadata.registration_endpoint.as_ref(),
    ];
    for endpoint in endpoints.into_iter().flatten() {
        let url = reqwest::Url::parse(endpoint).map_err(|_| {
            McpError::Config(format!(
                "server `{server}`: authorization server refused: an endpoint is not a valid URL"
            ))
        })?;
        client.vet(&url).await.map_err(|e| {
            McpError::Config(format!(
                "server `{server}`: authorization server refused: {e}"
            ))
        })?;
    }
    Ok(())
}

/// What the authorization server sent back on the redirect.
#[derive(Clone, PartialEq, Eq)]
pub struct Callback {
    /// The authorization code.
    pub code: String,
    /// The `iss` parameter (RFC 9207), when the server sent one.
    pub issuer: Option<String>,
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callback")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

/// One-shot redirect endpoint on the loopback interface.
#[derive(Debug)]
pub struct CallbackListener {
    listener: tokio::net::TcpListener,
    port: u16,
}

fn flow_error(server: &str, message: &str) -> McpError {
    McpError::Transport {
        server: server.to_owned(),
        message: format!("OAuth sign-in failed: {message}"),
    }
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = raw.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Query parameter `key` of the request line `GET /callback?... HTTP/1.1`.
fn query_param(target: &str, key: &str) -> Option<String> {
    let (_, query) = target.split_once('?')?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| percent_decode(v))
}

/// Check a redirect target (`/callback?...`) against the expected `state`.
fn parse_callback(
    target: &str,
    expected_state: &str,
) -> std::result::Result<Callback, &'static str> {
    if !(target == "/callback" || target.starts_with("/callback?")) {
        return Err("the redirect listener received an unexpected request");
    }
    // Compare the state before anything else is looked at.
    if query_param(target, "state").as_deref() != Some(expected_state) {
        Err("the redirect carried a wrong `state`")
    } else if query_param(target, "error").is_some() {
        Err("the authorization server refused the request")
    } else {
        match query_param(target, "code").filter(|c| !c.is_empty()) {
            Some(code) => Ok(Callback {
                code,
                issuer: query_param(target, "iss"),
            }),
            None => Err("the redirect carried no authorization code"),
        }
    }
}

/// A redirect URL the user pasted: it must be exactly our `redirect_uri`
/// (scheme, loopback host, port, path) plus the query.
fn pasted_callback(
    server: &str,
    redirect_uri: &str,
    pasted: &str,
    expected_state: &str,
) -> Result<Callback> {
    let pasted = pasted.trim();
    let target = pasted
        .strip_prefix(redirect_uri)
        .filter(|rest| rest.is_empty() || rest.starts_with('?'))
        .map(|rest| format!("/callback{rest}"))
        .ok_or_else(|| flow_error(server, "the pasted address is not this sign-in's redirect"))?;
    parse_callback(&target, expected_state).map_err(|message| flow_error(server, message))
}

impl CallbackListener {
    /// Bind a random port on `127.0.0.1` (never another interface).
    ///
    /// # Errors
    /// The socket cannot be bound.
    pub async fn bind(server: &str) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| flow_error(server, &format!("cannot open the redirect listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| flow_error(server, &e.to_string()))?
            .port();
        Ok(Self { listener, port })
    }

    /// The `redirect_uri` to register and to send in the authorization request.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait for the redirect. The first request decides: it must be
    /// `GET /callback` with the expected `state` and a `code`. The listener is
    /// consumed, so a second redirect finds the port closed.
    ///
    /// # Errors
    /// Timeout, a request that is not the expected redirect, a `state`
    /// mismatch, or an `error` answer from the authorization server.
    pub async fn wait(
        self,
        server: &str,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<Callback> {
        let Self { listener, .. } = self;
        let received = tokio::time::timeout(timeout, async {
            let (mut socket, peer) = listener.accept().await?;
            // Stop listening before the request is even read.
            drop(listener);
            if !peer.ip().is_loopback() {
                return Ok::<_, std::io::Error>((socket, None));
            }
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST_BYTES {
                let n = socket.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf);
            let target = head
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("GET "))
                .and_then(|rest| rest.split(' ').next())
                .map(str::to_owned);
            Ok((socket, target))
        })
        .await
        .map_err(|_| flow_error(server, "no redirect arrived before the time limit"))?
        .map_err(|e| flow_error(server, &e.to_string()))?;
        let (mut socket, target) = received;

        let outcome = match target.as_deref() {
            Some(target) => parse_callback(target, expected_state),
            None => Err("the redirect listener received an unexpected request"),
        };
        let (status, body) = match &outcome {
            Ok(_) => ("200 OK", "Signed in. You can close this window."),
            Err(_) => (
                "400 Bad Request",
                "Sign-in failed. Return to the app and try again.",
            ),
        };
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
                     Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.shutdown().await;
        outcome.map_err(|message| flow_error(server, message))
    }
}

#[cfg(test)]
mod paste_tests {
    use super::*;

    const REDIRECT: &str = "http://127.0.0.1:4711/callback";

    #[test]
    fn a_pasted_redirect_passes_the_same_checks_as_the_listener() {
        let ok = pasted_callback(
            "s",
            REDIRECT,
            "  http://127.0.0.1:4711/callback?code=abc&state=st&iss=https%3A%2F%2Fa.example \n",
            "st",
        )
        .unwrap();
        assert_eq!(ok.code, "abc");
        assert_eq!(ok.issuer.as_deref(), Some("https://a.example"));
        for (url, why) in [
            (
                "http://127.0.0.1:4711/callback?code=abc&state=other",
                "wrong `state`",
            ),
            (
                "http://127.0.0.1:4711/callback?state=st",
                "no authorization code",
            ),
            (
                "http://127.0.0.1:4711/callback?error=access_denied&state=st",
                "refused",
            ),
            (
                "http://127.0.0.1:4712/callback?code=abc&state=st",
                "not this sign-in",
            ),
            (
                "http://evil.example/callback?code=abc&state=st",
                "not this sign-in",
            ),
            (
                "http://127.0.0.1:4711/callbackx?code=abc&state=st",
                "not this sign-in",
            ),
            (
                "https://127.0.0.1:4711/callback?code=abc&state=st",
                "not this sign-in",
            ),
            ("code=abc&state=st", "not this sign-in"),
        ] {
            let err = pasted_callback("s", REDIRECT, url, "st")
                .unwrap_err()
                .to_string();
            assert!(err.contains(why), "{url}: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use parking_lot::Mutex;

    use super::*;

    #[derive(Default)]
    struct MemoryStore(Mutex<BTreeMap<String, String>>);

    impl SecretStore for MemoryStore {
        fn get(
            &self,
            name: &str,
        ) -> std::result::Result<Option<secret_store::SecretValue>, secret_store::SecretError>
        {
            Ok(self.0.lock().get(name).map(secret_store::SecretValue::new))
        }
        fn set(
            &self,
            name: &str,
            value: &str,
        ) -> std::result::Result<(), secret_store::SecretError> {
            self.0.lock().insert(name.to_owned(), value.to_owned());
            Ok(())
        }
        fn delete(&self, name: &str) -> std::result::Result<bool, secret_store::SecretError> {
            Ok(self.0.lock().remove(name).is_some())
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            Ok(self.0.lock().keys().cloned().collect())
        }
    }

    fn credentials(access: &str, refresh: &str) -> StoredCredentials {
        let token = serde_json::from_value(serde_json::json!({
            "access_token": access,
            "token_type": "bearer",
            "refresh_token": refresh,
            "expires_in": 3600
        }))
        .unwrap();
        StoredCredentials::new("client-1".into(), Some(token), vec!["read".into()], Some(1))
            .with_issuer(Some("https://as.example".into()))
    }

    #[tokio::test]
    async fn credentials_live_only_in_the_secret_store_and_are_registered_for_redaction() {
        let backend = Arc::new(MemoryStore::default());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let registrar: SecretRegistrar = {
            let seen = Arc::clone(&seen);
            Arc::new(move |value: &str| seen.lock().push(value.to_owned()))
        };
        let store = SecretCredentialStore::new(
            backend.clone(),
            "notion",
            "https://MCP.notion.example/mcp#frag",
            Some(registrar.clone()),
        )
        .unwrap();
        assert!(store.load().await.unwrap().is_none());

        store
            .save(credentials("access-sentinel-1", "refresh-sentinel-1"))
            .await
            .unwrap();
        assert_eq!(
            backend.list_names().unwrap(),
            vec!["mcp/notion/oauth".to_owned()]
        );
        assert_eq!(
            *seen.lock(),
            vec![
                "access-sentinel-1".to_owned(),
                "refresh-sentinel-1".to_owned()
            ]
        );
        assert!(!format!("{store:?}").contains("sentinel"));

        // A fresh process registers the stored tokens again when it loads them.
        seen.lock().clear();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.client_id, "client-1");
        assert_eq!(loaded.issuer.as_deref(), Some("https://as.example"));
        assert!(!format!("{loaded:?}").contains("sentinel"));
        assert_eq!(seen.lock().len(), 2);

        // Another server's credentials are a different secret.
        let other =
            SecretCredentialStore::new(backend.clone(), "linear", "https://l.example/mcp", None)
                .unwrap();
        assert!(other.load().await.unwrap().is_none());

        // Same server name, another URL: the token is not handed out and is
        // removed, but still registered for redaction on the way.
        seen.lock().clear();
        let moved = SecretCredentialStore::new(
            backend.clone(),
            "notion",
            "https://attacker.example/mcp",
            Some(registrar),
        )
        .unwrap();
        assert!(moved.load().await.unwrap().is_none());
        assert_eq!(seen.lock().len(), 2);
        assert_eq!(backend.list_names().unwrap(), Vec::<String>::new());

        // Review finding 11: the same gateway, another query-selected
        // resource, is another binding.
        let tenant_a = SecretCredentialStore::new(
            backend.clone(),
            "gateway",
            "https://gateway.example/mcp?tenant=A",
            None,
        )
        .unwrap();
        tenant_a
            .save(credentials("tenant-a-access", "tenant-a-refresh"))
            .await
            .unwrap();
        assert!(tenant_a.load().await.unwrap().is_some());
        let tenant_b = SecretCredentialStore::new(
            backend.clone(),
            "gateway",
            "https://gateway.example/mcp?tenant=B",
            None,
        )
        .unwrap();
        assert!(tenant_b.load().await.unwrap().is_none());
        assert!(
            tenant_a.load().await.unwrap().is_none(),
            "A's token was kept after B looked"
        );
        assert!(store.load().await.unwrap().is_none());
        store
            .save(credentials("access-sentinel-1", "refresh-sentinel-1"))
            .await
            .unwrap();

        store.clear().await.unwrap();
        assert_eq!(backend.list_names().unwrap(), Vec::<String>::new());
        assert!(store.load().await.unwrap().is_none());
    }

    struct Failing;

    impl SecretStore for Failing {
        fn get(
            &self,
            _: &str,
        ) -> std::result::Result<Option<secret_store::SecretValue>, secret_store::SecretError>
        {
            Err(secret_store::SecretError::Keyring(
                "backend-sentinel".into(),
            ))
        }
        fn set(&self, _: &str, value: &str) -> std::result::Result<(), secret_store::SecretError> {
            Err(secret_store::SecretError::Keyring(value.to_owned()))
        }
        fn delete(&self, _: &str) -> std::result::Result<bool, secret_store::SecretError> {
            Err(secret_store::SecretError::Keyring(
                "backend-sentinel".into(),
            ))
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn backend_errors_never_carry_token_or_backend_text() {
        let store =
            SecretCredentialStore::new(Arc::new(Failing), "s", "https://s.example/mcp", None)
                .unwrap();
        let save = store
            .save(credentials("access-sentinel-2", "refresh-sentinel-2"))
            .await
            .unwrap_err()
            .to_string();
        let load = store.load().await.unwrap_err().to_string();
        for text in [save, load] {
            assert!(!text.contains("sentinel"), "{text}");
            assert!(text.contains("secret store"), "{text}");
        }
    }

    #[test]
    fn resources_are_canonical_and_metadata_is_checked() {
        assert_eq!(
            canonical_resource("HTTPS://Mcp.Example.com:8443/mcp?x=1#f").unwrap(),
            "https://mcp.example.com:8443/mcp?x=1"
        );
        assert_eq!(
            canonical_resource("https://mcp.example.com/mcp#f").unwrap(),
            "https://mcp.example.com/mcp"
        );
        for bad in [
            "mcp.example.com/mcp",
            "ftp://x/y",
            "https://user:pw@x/y",
            "",
        ] {
            assert!(canonical_resource(bad).is_err(), "{bad}");
        }

        let metadata = |authorize: &str, token: &str, methods: Option<&[&str]>| {
            let mut m = AuthorizationMetadata::default();
            m.authorization_endpoint = authorize.into();
            m.token_endpoint = token.into();
            m.code_challenge_methods_supported =
                methods.map(|list| list.iter().map(|s| (*s).to_owned()).collect());
            m
        };
        let s256: Option<&[&str]> = Some(&["S256"]);
        let ok = metadata(
            "https://as.example/authorize",
            "https://as.example/token",
            s256,
        );
        check_metadata("s", &ok, false).unwrap();
        // The spec requires refusing a server that does not advertise PKCE.
        for methods in [None, Some(&["plain"][..])] {
            let m = metadata(
                "https://as.example/authorize",
                "https://as.example/token",
                methods,
            );
            let error = check_metadata("s", &m, true).unwrap_err().to_string();
            assert!(error.contains("S256"), "{error}");
        }
        for (authorize, token) in [
            ("http://as.example/authorize", "https://as.example/token"),
            (
                "https://as.example/authorize",
                "http://169.254.169.254/token",
            ),
            ("https://as.example/authorize", "http://10.0.0.5/token"),
            ("file:///etc/passwd", "https://as.example/token"),
        ] {
            let m = metadata(authorize, token, s256);
            assert!(
                check_metadata("s", &m, true).is_err(),
                "{authorize} {token}"
            );
        }
        let local = metadata(
            "http://127.0.0.1:9/authorize",
            "http://localhost:9/token",
            s256,
        );
        assert!(check_metadata("s", &local, false).is_err());
        check_metadata("s", &local, true).unwrap();
    }

    struct TableResolver;

    #[async_trait::async_trait]
    impl EndpointResolver for TableResolver {
        async fn lookup(&self, host: &str, _: u16) -> Option<Vec<std::net::IpAddr>> {
            let answers: &[&str] = match host {
                "public.test" => &["93.184.216.34"],
                "imds.attacker.test" => &["169.254.169.254"],
                "internal.attacker.test" => &["10.0.0.5"],
                "mixed.attacker.test" => &["93.184.216.34", "10.0.0.5"],
                "mixed-reversed.attacker.test" => &["10.0.0.5", "93.184.216.34"],
                "mapped.attacker.test" => &["::ffff:169.254.169.254"],
                "empty.test" => &[],
                _ => return None,
            };
            Some(answers.iter().map(|ip| ip.parse().unwrap()).collect())
        }
    }

    /// Review finding 4: authorization, token and registration endpoints all
    /// pass the transport's policy (literals, aliases, mixed answers).
    #[tokio::test]
    async fn advertised_endpoints_are_vetted_like_every_request() {
        let client =
            crate::oauth_net::PolicyHttpClient::with_resolver(false, Arc::new(TableResolver));
        let good = "https://public.test/oauth";
        for bad in [
            "https://169.254.169.254/latest/meta-data/",
            "https://[::ffff:169.254.169.254]/token",
            "https://[fe80::1]/token",
            "https://0.0.0.0/token",
            "https://10.0.0.5/token",
            "https://100.64.0.1/token",
            "https://127.0.0.1/token",
            "https://[fd00::1]/token",
            "https://imds.attacker.test/oauth",
            "https://internal.attacker.test/oauth",
            "https://mixed.attacker.test/oauth",
            "https://mixed-reversed.attacker.test/oauth",
            "https://mapped.attacker.test/oauth",
            "https://empty.test/oauth",
            "https://missing.test/oauth",
            "http://public.test/oauth",
        ] {
            for slot in 0..3 {
                let mut m = AuthorizationMetadata::default();
                m.authorization_endpoint = if slot == 0 { bad } else { good }.into();
                m.token_endpoint = if slot == 1 { bad } else { good }.into();
                m.registration_endpoint = Some(if slot == 2 { bad } else { good }.into());
                assert!(
                    check_endpoint_addresses("s", &client, &m).await.is_err(),
                    "{bad} in slot {slot}"
                );
            }
        }
        let mut m = AuthorizationMetadata::default();
        m.authorization_endpoint = good.into();
        m.token_endpoint = good.into();
        check_endpoint_addresses("s", &client, &m).await.unwrap();
        // The loopback opt-in admits loopback http, nothing private.
        let dev = crate::oauth_net::PolicyHttpClient::with_resolver(true, Arc::new(TableResolver));
        m.token_endpoint = "http://127.0.0.1:9/token".into();
        check_endpoint_addresses("s", &dev, &m).await.unwrap();
        m.token_endpoint = "https://10.0.0.5/token".into();
        assert!(check_endpoint_addresses("s", &dev, &m).await.is_err());
    }

    async fn get(port: u16, target: &str) -> std::io::Result<String> {
        let mut socket = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
        socket
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await?;
        let mut out = String::new();
        socket.read_to_string(&mut out).await?;
        Ok(out)
    }

    fn port_of(listener: &CallbackListener) -> u16 {
        let uri = listener.redirect_uri();
        assert!(uri.starts_with("http://127.0.0.1:"), "{uri}");
        assert!(uri.ends_with("/callback"), "{uri}");
        assert!(
            listener.listener.local_addr().unwrap().ip().is_loopback(),
            "the redirect listener must only bind to loopback"
        );
        listener.port
    }

    #[tokio::test]
    async fn the_redirect_is_accepted_once_with_the_expected_state() {
        let listener = CallbackListener::bind("s").await.unwrap();
        let port = port_of(&listener);
        let waiting = tokio::spawn(listener.wait("s", "state-123", Duration::from_secs(10)));
        let page = get(
            port,
            "/callback?code=abc%2Fdef&state=state-123&iss=https%3A%2F%2Fas.example",
        )
        .await
        .unwrap();
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        let callback = waiting.await.unwrap().unwrap();
        assert_eq!(callback.code, "abc/def");
        assert_eq!(callback.issuer.as_deref(), Some("https://as.example"));
        assert!(!format!("{callback:?}").contains("abc"));
        // The listener is gone: a replayed redirect cannot be delivered.
        assert!(
            get(port, "/callback?code=again&state=state-123")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_wrong_state_ends_the_flow_and_closes_the_listener() {
        for target in [
            "/callback?code=stolen&state=other",
            "/callback?code=stolen",
            "/callback?state=state-123",
            "/callback?state=state-123&error=access_denied&code=x",
            "/elsewhere?code=x&state=state-123",
        ] {
            let listener = CallbackListener::bind("s").await.unwrap();
            let port = port_of(&listener);
            let waiting = tokio::spawn(listener.wait("s", "state-123", Duration::from_secs(10)));
            let page = get(port, target).await.unwrap();
            assert!(page.starts_with("HTTP/1.1 400"), "{target}: {page}");
            let error = waiting.await.unwrap().unwrap_err().to_string();
            assert!(error.contains("OAuth sign-in failed"), "{target}: {error}");
            assert!(!error.contains("stolen"), "{error}");
            // No second chance for the correct state either.
            assert!(
                get(port, "/callback?code=abc&state=state-123")
                    .await
                    .is_err(),
                "{target}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_listener_times_out() {
        let listener = CallbackListener::bind("s").await.unwrap();
        let error = listener
            .wait("s", "state-123", CALLBACK_TIMEOUT)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("time limit"), "{error}");
    }
}

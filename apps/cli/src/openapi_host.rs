//! REST APIs described by an `OpenAPI` document, for `bot serve`: plugs the
//! `openapi-tools` crate into the daemon's `/v1/openapi-apps/*` routes
//! ([`daemon_api::OpenApiAppsControl`]) and the tool registry.
//!
//! * `<data dir>/openapi-apps.toml` holds the apps (base URL, credential kind,
//!   enabled operations and the bots that may use each); no secret value.
//! * `<data dir>/openapi-specs/<id>.spec` is the saved document.
//! * The credential lives in the secret store as `openapi/<id>/credential`
//!   and is read only when a call is made.
//!
//! Only enabled operations are registered, as `api_<id>_<operation>` tools,
//! and each is visible to and callable by the listed bots only.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use daemon_api::openapi_apps::{
    OpenApiAppDto, OpenApiAppInput, OpenApiAppUpdate, OpenApiAuthDto, OpenApiAuthInput,
    OpenApiAuthKind, OpenApiEffect, OpenApiEnabledOperation, OpenApiOperationDto,
    OpenApiSkippedDto,
};
use daemon_api::{OpenApiAppsControl, OpenApiAppsError};
use openapi_tools::{AccessGate, ApiSpec, Auth, ImportOptions, OpenApiConfig, OpenApiError};
use parking_lot::RwLock;
use secret_store::{SecretRef, SecretStore};
use serde::{Deserialize, Serialize};
use tool_registry::{Effect, ToolRegistry};

/// Config file in the data directory.
pub const OPENAPI_APPS_FILE: &str = "openapi-apps.toml";
const SPEC_DIR: &str = "openapi-specs";
const FILE_HEADER: &str = "# REST APIs from OpenAPI documents. Managed by Praxi-Bot (Apps); credentials are in the secret store.\n";

/// Most operations that can be enabled at once for one API: every enabled
/// operation is a tool the model has to read.
const MAX_ENABLED: usize = 50;
/// Largest spec document kept (the importer's own limit).
const MAX_SPEC_BYTES: usize = 8 * 1024 * 1024;
/// Most operations listed for one document.
const MAX_OPERATIONS: usize = 2000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ID_LEN: usize = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    app: Vec<StoredApp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredApp {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spec_url: Option<String>,
    base_url: String,
    auth: StoredAuth,
    #[serde(default)]
    enabled: Vec<StoredEnabled>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAuth {
    kind: OpenApiAuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEnabled {
    name: String,
    /// `None` (absent) = every bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bots: Option<Vec<String>>,
}

/// An app with its parsed document.
#[derive(Clone)]
struct Loaded {
    stored: StoredApp,
    spec: Option<ApiSpec>,
    /// Why the saved document or its settings could not be used.
    problem: Option<String>,
    /// A credential is in the secret store.
    has_secret: bool,
}

#[derive(Default)]
struct State {
    apps: Vec<Loaded>,
    /// Tool names registered per app id.
    registered: HashMap<String, Vec<String>>,
}

/// The REST-API manager behind the daemon API.
pub struct OpenApiHost {
    data_dir: PathBuf,
    registry: ToolRegistry,
    secrets: Arc<dyn SecretStore>,
    /// Teaches the live redactor a credential so logs and events mask it.
    register_secret: Arc<dyn Fn(&str) + Send + Sync>,
    state: RwLock<State>,
    /// Serializes changes (they download, write files and swap tools).
    write: tokio::sync::Mutex<()>,
    allow_private_hosts: bool,
}

fn secret_name(id: &str) -> String {
    format!("openapi/{id}/credential")
}

fn prefix(id: &str) -> String {
    format!("api_{id}")
}

fn invalid(m: impl Into<String>) -> OpenApiAppsError {
    OpenApiAppsError::Invalid(m.into())
}

fn internal(m: impl std::fmt::Display) -> OpenApiAppsError {
    OpenApiAppsError::Internal(m.to_string())
}

fn spec_error(e: &OpenApiError) -> OpenApiAppsError {
    match e {
        OpenApiError::Fetch(m) => OpenApiAppsError::Upstream(m.clone()),
        other => invalid(other.to_string()),
    }
}

/// `[a-z0-9_]` id from a display name.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    let out: String = out.chars().take(MAX_ID_LEN).collect();
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() { "api".into() } else { out }
}

/// Options for reading a document. `base_url` is the owner's choice and the
/// only server ever used; without it the document's own `servers` count only
/// when they sit on `spec_url`'s host.
fn import_options(id: &str, base_url: Option<&str>, spec_url: Option<&str>) -> ImportOptions {
    ImportOptions {
        name_prefix: prefix(id),
        base_url: base_url.map(str::to_owned),
        server_host: spec_url
            .and_then(|u| reqwest::Url::parse(u.trim()).ok())
            .and_then(|u| u.host_str().map(str::to_owned)),
        max_operations: MAX_OPERATIONS,
        max_spec_bytes: MAX_SPEC_BYTES,
        ..ImportOptions::default()
    }
}

impl OpenApiHost {
    /// Load `openapi-apps.toml` and register the enabled operations. Does no
    /// network access.
    pub fn start(
        data_dir: &Path,
        registry: ToolRegistry,
        secrets: Arc<dyn SecretStore>,
        register_secret: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Arc<Self> {
        let host = Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            registry,
            secrets,
            register_secret,
            state: RwLock::new(State::default()),
            write: tokio::sync::Mutex::new(()),
            allow_private_hosts: false,
        });
        host.load();
        host
    }

    /// Tests only: talk to a fake server on loopback.
    #[cfg(test)]
    fn allowing_private_hosts(mut self: Arc<Self>) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().allow_private_hosts = true;
        self.load();
        self
    }

    fn config_path(&self) -> PathBuf {
        self.data_dir.join(OPENAPI_APPS_FILE)
    }

    fn spec_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(SPEC_DIR).join(format!("{id}.spec"))
    }

    fn load(&self) {
        let path = self.config_path();
        let file: File = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {}: {e}; REST APIs are off", path.display());
                File::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: cannot read {}: {e}", path.display());
                File::default()
            }
        };
        let apps: Vec<Loaded> = file
            .app
            .into_iter()
            .filter(|a| !a.id.is_empty() && slug(&a.id) == a.id)
            .map(|stored| self.load_spec(stored))
            .collect();
        let ids: Vec<String> = apps.iter().map(|a| a.stored.id.clone()).collect();
        self.state.write().apps = apps;
        for id in ids {
            self.apply(&id);
        }
    }

    fn load_spec(&self, stored: StoredApp) -> Loaded {
        let parsed = std::fs::read_to_string(self.spec_path(&stored.id))
            .map_err(|e| format!("cannot read the saved document: {e}"))
            .and_then(|text| {
                ApiSpec::parse(
                    &text,
                    &import_options(&stored.id, Some(&stored.base_url), None),
                )
                .map_err(|e| e.to_string())
            });
        let has_secret = self.secret_present(&stored.id);
        match parsed {
            Ok(spec) => Loaded {
                stored,
                spec: Some(spec),
                problem: None,
                has_secret,
            },
            Err(problem) => Loaded {
                stored,
                spec: None,
                problem: Some(problem),
                has_secret,
            },
        }
    }

    fn save(&self, apps: &[Loaded]) -> Result<(), OpenApiAppsError> {
        let file = File {
            app: apps.iter().map(|a| a.stored.clone()).collect(),
        };
        let body = toml::to_string_pretty(&file).map_err(internal)?;
        let path = self.config_path();
        let tmp = path.with_extension("toml.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(FILE_HEADER.as_bytes())?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            internal(format!("cannot save {}: {e}", path.display()))
        })
    }

    fn write_spec(&self, id: &str, text: &str) -> Result<(), OpenApiAppsError> {
        let dir = self.data_dir.join(SPEC_DIR);
        std::fs::create_dir_all(&dir).map_err(internal)?;
        let path = self.spec_path(id);
        let tmp = path.with_extension("spec.tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                internal(format!("cannot save the document: {e}"))
            })
    }

    /// The auth part of the runtime config; `Err` when the stored settings are unusable.
    fn auth_of(stored: &StoredApp) -> Result<Auth, String> {
        let secret = || SecretRef::new(secret_name(&stored.id)).map_err(|e| e.to_string());
        let name = || stored.auth.name.clone().unwrap_or_default();
        Ok(match stored.auth.kind {
            OpenApiAuthKind::None => Auth::None,
            OpenApiAuthKind::ApiKeyHeader => Auth::ApiKeyHeader {
                header: name(),
                secret: secret()?,
            },
            OpenApiAuthKind::ApiKeyQuery => Auth::ApiKeyQuery {
                param: name(),
                secret: secret()?,
            },
            OpenApiAuthKind::Bearer => Auth::Bearer { secret: secret()? },
            OpenApiAuthKind::Basic => Auth::Basic {
                username: stored.auth.username.clone().unwrap_or_default(),
                password_secret: secret()?,
            },
        })
    }

    fn runtime_config(
        &self,
        stored: &StoredApp,
        gate: Option<AccessGate>,
    ) -> Result<OpenApiConfig, String> {
        let mut cfg = OpenApiConfig::new(Arc::clone(&self.secrets));
        cfg.auth = Self::auth_of(stored)?;
        cfg.allow_private_hosts = self.allow_private_hosts;
        cfg.gate = gate;
        Ok(cfg)
    }

    /// Replace the registered tools of `id` with the enabled operations of its
    /// current state. A problem leaves the app without tools (and says why).
    fn apply(&self, id: &str) {
        let mut guard = self.state.write();
        let state = &mut *guard;
        for name in state.registered.remove(id).unwrap_or_default() {
            self.registry.unregister(&name);
        }
        let Some(app) = state.apps.iter_mut().find(|a| a.stored.id == id) else {
            return;
        };
        let Some(mut spec) = app.spec.clone() else {
            return;
        };
        let access: HashMap<String, Option<Vec<String>>> = app
            .stored
            .enabled
            .iter()
            .map(|e| (e.name.clone(), e.bots.clone()))
            .collect();
        if access.is_empty() {
            app.problem = None;
            return;
        }
        spec.operations.retain(|o| access.contains_key(&o.name));
        let gate: AccessGate = Arc::new(move |tool, bot| match access.get(tool) {
            None => false,
            Some(None) => true,
            Some(Some(list)) => bot.is_some_and(|b| list.iter().any(|x| x == b)),
        });
        self.preload_secret(&app.stored);
        let built = self
            .runtime_config(&app.stored, Some(gate))
            .and_then(|cfg| openapi_tools::tools(&spec, &cfg).map_err(|e| e.to_string()));
        match built {
            Ok(tools) => {
                let total = tools.len();
                let names: Vec<String> = tools
                    .into_iter()
                    .filter_map(|t| {
                        let name = t.spec().name.clone();
                        self.registry.register(t).then_some(name)
                    })
                    .collect();
                // A name another tool already has (two apps whose ids and operation names
                // join into the same name) is not registered: say so instead of hiding it.
                app.problem = (names.len() < total).then(|| {
                    format!(
                        "{} operation(s) share a tool name with another tool and are off",
                        total - names.len()
                    )
                });
                state.registered.insert(id.to_owned(), names);
            }
            Err(problem) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: REST API `{id}`: {problem}; its tools are off");
                app.problem = Some(problem);
            }
        }
    }

    /// Let the live redactor know the credential before any tool can echo it.
    fn preload_secret(&self, stored: &StoredApp) {
        if stored.auth.kind == OpenApiAuthKind::None {
            return;
        }
        if let Ok(Some(value)) = self.secrets.get(&secret_name(&stored.id)) {
            (self.register_secret)(value.expose());
        }
    }

    fn secret_present(&self, id: &str) -> bool {
        matches!(self.secrets.get(&secret_name(id)), Ok(Some(_)))
    }

    fn dto(app: &Loaded) -> OpenApiAppDto {
        let stored = &app.stored;
        let access: HashMap<&str, &Option<Vec<String>>> = stored
            .enabled
            .iter()
            .map(|e| (e.name.as_str(), &e.bots))
            .collect();
        let spec = app.spec.as_ref();
        let operations: Vec<OpenApiOperationDto> = spec
            .map(|s| {
                s.operations
                    .iter()
                    .map(|op| {
                        let risk = openapi_tools::risk_of(op);
                        let bots = access.get(op.name.as_str()).copied();
                        OpenApiOperationDto {
                            name: op.name.clone(),
                            method: op.method.as_str().to_owned(),
                            path: op.path.clone(),
                            summary: first_line(&op.description),
                            tags: op.tags.clone(),
                            effect: match risk.effect {
                                Effect::Read => OpenApiEffect::Read,
                                Effect::Write => OpenApiEffect::Write,
                                Effect::Destructive => OpenApiEffect::Destructive,
                            },
                            always_ask: risk.always_ask,
                            enabled: bots.is_some(),
                            bots: bots.cloned().flatten(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut warnings = spec.map(|s| s.warnings.clone()).unwrap_or_default();
        if let Some(p) = &app.problem {
            warnings.insert(0, p.clone());
        }
        OpenApiAppDto {
            id: stored.id.clone(),
            name: stored.name.clone(),
            title: spec.map(|s| s.title.clone()).unwrap_or_default(),
            version: spec.map(|s| s.version.clone()).unwrap_or_default(),
            base_url: stored.base_url.clone(),
            spec_url: stored.spec_url.clone(),
            auth: OpenApiAuthDto {
                kind: stored.auth.kind,
                name: stored.auth.name.clone(),
                username: stored.auth.username.clone(),
                secret_set: app.has_secret,
            },
            skipped: spec
                .map(|s| {
                    s.skipped
                        .iter()
                        .map(|k| OpenApiSkippedDto {
                            method: k.method.clone(),
                            path: k.path.clone(),
                            reason: k.reason.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            warnings,
            security_hints: spec
                .map(|s| {
                    s.security_schemes
                        .iter()
                        .map(|x| format!("{}: {}", x.name, x.kind))
                        .collect()
                })
                .unwrap_or_default(),
            enabled_count: u32::try_from(operations.iter().filter(|o| o.enabled).count())
                .unwrap_or(u32::MAX),
            max_enabled: u32::try_from(MAX_ENABLED).unwrap_or(u32::MAX),
            operations,
        }
    }
}

fn first_line(description: &str) -> String {
    let line = description.lines().next().unwrap_or_default().trim();
    if line.chars().count() > 160 {
        let cut: String = line.chars().take(160).collect();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

fn check_auth_shape(
    kind: OpenApiAuthKind,
    name: Option<&str>,
    username: Option<&str>,
) -> Result<StoredAuth, OpenApiAppsError> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let username = username.map(str::trim).filter(|n| !n.is_empty());
    match kind {
        OpenApiAuthKind::None | OpenApiAuthKind::Bearer => Ok(StoredAuth {
            kind,
            name: None,
            username: None,
        }),
        OpenApiAuthKind::ApiKeyHeader => {
            let n = name.ok_or_else(|| invalid("the API key needs a header name"))?;
            reqwest::header::HeaderName::from_bytes(n.as_bytes())
                .map_err(|_| invalid(format!("`{n}` is not a valid header name")))?;
            Ok(StoredAuth {
                kind,
                name: Some(n.to_owned()),
                username: None,
            })
        }
        OpenApiAuthKind::ApiKeyQuery => {
            let n = name.ok_or_else(|| invalid("the API key needs a query parameter name"))?;
            if n.len() > 64
                || n.chars()
                    .any(|c| c.is_control() || matches!(c, '&' | '=' | '#' | ' '))
            {
                return Err(invalid(format!(
                    "`{n}` is not a valid query parameter name"
                )));
            }
            Ok(StoredAuth {
                kind,
                name: Some(n.to_owned()),
                username: None,
            })
        }
        OpenApiAuthKind::Basic => {
            let u = username.ok_or_else(|| invalid("basic authentication needs a user name"))?;
            if u.contains(':') || u.chars().any(char::is_control) || u.len() > 256 {
                return Err(invalid(
                    "the user name must not contain a colon or control characters",
                ));
            }
            Ok(StoredAuth {
                kind,
                name: None,
                username: Some(u.to_owned()),
            })
        }
    }
}

/// Bots of one enabled operation: trimmed, de-duplicated, no empty ids.
fn clean_bots(bots: Option<Vec<String>>) -> Option<Vec<String>> {
    bots.map(|list| {
        let mut seen = HashSet::new();
        list.into_iter()
            .map(|b| b.trim().to_owned())
            .filter(|b| !b.is_empty() && seen.insert(b.clone()))
            .collect()
    })
}

impl OpenApiHost {
    async fn store_secret(&self, id: &str, value: String) -> Result<(), OpenApiAppsError> {
        (self.register_secret)(&value);
        let store = Arc::clone(&self.secrets);
        let name = secret_name(id);
        tokio::task::spawn_blocking(move || store.set(&name, &value))
            .await
            .map_err(internal)?
            .map_err(|e| internal(format!("cannot store the credential: {e}")))
    }

    async fn delete_secret(&self, id: &str) {
        let store = Arc::clone(&self.secrets);
        let name = secret_name(id);
        let _ = tokio::task::spawn_blocking(move || store.delete(&name)).await;
    }

    /// Validate the base URL and the credential settings the way a call would
    /// (private addresses refused, header name well formed) without any operation.
    fn probe(&self, spec: &ApiSpec, stored: &StoredApp) -> Result<(), OpenApiAppsError> {
        let cfg = self.runtime_config(stored, None).map_err(invalid)?;
        let mut empty = spec.clone();
        empty.operations.clear();
        openapi_tools::tools(&empty, &cfg)
            .map(|_| ())
            .map_err(|e| invalid(e.to_string()))
    }

    fn app(&self, id: &str) -> Result<Loaded, OpenApiAppsError> {
        self.state
            .read()
            .apps
            .iter()
            .find(|a| a.stored.id == id)
            .cloned()
            .ok_or_else(|| OpenApiAppsError::NotFound(format!("app `{id}`")))
    }

    /// Store `app` (replacing the one with the same id, or appending), persist
    /// and re-register its tools.
    fn commit(&self, app: Loaded) -> Result<OpenApiAppDto, OpenApiAppsError> {
        let id = app.stored.id.clone();
        let mut apps = self.state.read().apps.clone();
        match apps.iter_mut().find(|a| a.stored.id == id) {
            Some(slot) => *slot = app,
            None => apps.push(app),
        }
        self.save(&apps)?;
        self.state.write().apps = apps;
        self.apply(&id);
        self.app(&id).map(|a| Self::dto(&a))
    }

    /// The document text of an add: downloaded or given.
    async fn spec_text(&self, input: &OpenApiAppInput) -> Result<String, OpenApiAppsError> {
        match (&input.spec_url, &input.spec_text) {
            (Some(url), None) => self.download(url).await,
            (None, Some(text)) => Ok(text.clone()),
            _ => Err(invalid(
                "give exactly one of the document URL and the document text",
            )),
        }
    }

    async fn download(&self, url: &str) -> Result<String, OpenApiAppsError> {
        let fetched = if self.allow_private_hosts {
            openapi_tools::fetch_spec_allowing_private(url, MAX_SPEC_BYTES, FETCH_TIMEOUT).await
        } else {
            openapi_tools::fetch_spec(url, MAX_SPEC_BYTES, FETCH_TIMEOUT).await
        };
        fetched.map_err(|e| spec_error(&e))
    }

    fn validate_enabled(
        spec: &ApiSpec,
        wanted: Vec<OpenApiEnabledOperation>,
    ) -> Result<Vec<StoredEnabled>, OpenApiAppsError> {
        let known: HashSet<&str> = spec.operations.iter().map(|o| o.name.as_str()).collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for op in wanted {
            if !known.contains(op.name.as_str()) {
                return Err(invalid(format!(
                    "the document has no operation `{}`",
                    op.name
                )));
            }
            if !seen.insert(op.name.clone()) {
                continue;
            }
            out.push(StoredEnabled {
                name: op.name,
                bots: clean_bots(op.bots),
            });
        }
        if out.len() > MAX_ENABLED {
            return Err(invalid(format!(
                "at most {MAX_ENABLED} operations can be enabled at once"
            )));
        }
        Ok(out)
    }
}

#[async_trait]
impl OpenApiAppsControl for OpenApiHost {
    fn list(&self) -> Vec<OpenApiAppDto> {
        let mut apps: Vec<OpenApiAppDto> = self.state.read().apps.iter().map(Self::dto).collect();
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps
    }

    fn get(&self, id: &str) -> Result<OpenApiAppDto, OpenApiAppsError> {
        self.app(id).map(|a| Self::dto(&a))
    }

    async fn add(&self, input: OpenApiAppInput) -> Result<OpenApiAppDto, OpenApiAppsError> {
        let _guard = self.write.lock().await;
        let name = input.name.trim().to_owned();
        let text = self.spec_text(&input).await?;
        let taken: HashSet<String> = self
            .state
            .read()
            .apps
            .iter()
            .map(|a| a.stored.id.clone())
            .collect();
        let base = slug(&name);
        let mut id = base.clone();
        let mut n = 2;
        while taken.contains(&id) {
            id = format!("{base}_{n}");
            n += 1;
        }
        let base_url = input
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty());
        let spec = ApiSpec::parse(
            &text,
            &import_options(&id, base_url, input.spec_url.as_deref()),
        )
        .map_err(|e| spec_error(&e))?;
        let auth_input = input.auth.as_ref();
        let kind = auth_input.map_or(OpenApiAuthKind::None, |a| a.kind);
        let auth = check_auth_shape(
            kind,
            auth_input.and_then(|a| a.name.as_deref()),
            auth_input.and_then(|a| a.username.as_deref()),
        )?;
        let secret = auth_input.and_then(|a| a.secret.clone());
        if kind != OpenApiAuthKind::None && secret.is_none() {
            return Err(invalid(
                "a credential is needed for this kind of authentication",
            ));
        }
        let stored = StoredApp {
            id: id.clone(),
            name,
            spec_url: input.spec_url.map(|u| u.trim().to_owned()),
            base_url: spec.base_url.clone(),
            auth,
            enabled: Vec::new(),
        };
        self.probe(&spec, &stored)?;
        self.write_spec(&id, &text)?;
        let has_secret = match secret {
            Some(value) if kind != OpenApiAuthKind::None => {
                if let Err(e) = self.store_secret(&id, value).await {
                    let _ = std::fs::remove_file(self.spec_path(&id));
                    return Err(e);
                }
                true
            }
            _ => false,
        };
        let loaded = Loaded {
            stored,
            spec: Some(spec),
            problem: None,
            has_secret,
        };
        self.commit(loaded).inspect_err(|_| {
            let _ = std::fs::remove_file(self.spec_path(&id));
        })
    }

    async fn update(
        &self,
        id: &str,
        input: OpenApiAppUpdate,
    ) -> Result<OpenApiAppDto, OpenApiAppsError> {
        let _guard = self.write.lock().await;
        let mut app = self.app(id)?;
        if let Some(name) = input.name.as_deref().map(str::trim) {
            if name.is_empty() || name.chars().count() > 80 {
                return Err(invalid("the name must be 1 to 80 characters"));
            }
            app.stored.name = name.to_owned();
        }
        if let Some(url) = input.base_url.as_deref().map(str::trim) {
            let text = std::fs::read_to_string(self.spec_path(id)).map_err(internal)?;
            let spec = ApiSpec::parse(&text, &import_options(id, Some(url), None))
                .map_err(|e| spec_error(&e))?;
            app.stored.base_url.clone_from(&spec.base_url);
            app.spec = Some(spec);
        }
        let mut new_secret = None;
        if let Some(auth) = input.auth {
            let OpenApiAuthInput {
                kind,
                name,
                username,
                secret,
            } = auth;
            app.stored.auth = check_auth_shape(kind, name.as_deref(), username.as_deref())?;
            if kind == OpenApiAuthKind::None {
                app.has_secret = false;
            } else if let Some(value) = secret {
                new_secret = Some(value);
            } else if !app.has_secret {
                return Err(invalid(
                    "a credential is needed for this kind of authentication",
                ));
            }
        }
        if let Some(wanted) = input.operations {
            let spec = app.spec.as_ref().ok_or_else(|| {
                invalid("the saved document is unusable; refresh or remove the app")
            })?;
            app.stored.enabled = Self::validate_enabled(spec, wanted)?;
        }
        if let Some(spec) = &app.spec {
            self.probe(spec, &app.stored)?;
        }
        if let Some(value) = new_secret {
            self.store_secret(id, value).await?;
            app.has_secret = true;
        }
        let removed_secret =
            app.stored.auth.kind == OpenApiAuthKind::None && self.secret_present(id);
        let dto = self.commit(app)?;
        if removed_secret {
            self.delete_secret(id).await;
        }
        Ok(dto)
    }

    async fn refresh(&self, id: &str) -> Result<OpenApiAppDto, OpenApiAppsError> {
        let _guard = self.write.lock().await;
        let mut app = self.app(id)?;
        let url = app.stored.spec_url.clone().ok_or_else(|| {
            invalid("this API was added from a file; add the document again to update it")
        })?;
        let text = self.download(&url).await?;
        let spec = ApiSpec::parse(&text, &import_options(id, Some(&app.stored.base_url), None))
            .map_err(|e| spec_error(&e))?;
        // Keep what still exists; an operation that vanished is switched off.
        let known: HashSet<&str> = spec.operations.iter().map(|o| o.name.as_str()).collect();
        app.stored
            .enabled
            .retain(|e| known.contains(e.name.as_str()));
        app.spec = Some(spec);
        app.problem = None;
        self.write_spec(id, &text)?;
        self.commit(app)
    }

    async fn remove(&self, id: &str) -> Result<(), OpenApiAppsError> {
        let _guard = self.write.lock().await;
        self.app(id)?;
        let apps: Vec<Loaded> = self
            .state
            .read()
            .apps
            .iter()
            .filter(|a| a.stored.id != id)
            .cloned()
            .collect();
        self.save(&apps)?;
        self.state.write().apps = apps;
        self.apply(id);
        self.delete_secret(id).await;
        let _ = std::fs::remove_file(self.spec_path(id));
        Ok(())
    }

    async fn forget_bot(&self, bot_id: &str) -> Result<usize, OpenApiAppsError> {
        let _guard = self.write.lock().await;
        let mut apps = self.state.read().apps.clone();
        let mut changed = Vec::new();
        for app in &mut apps {
            let mut touched = false;
            for e in &mut app.stored.enabled {
                if let Some(list) = &mut e.bots {
                    let before = list.len();
                    list.retain(|b| b != bot_id);
                    touched |= list.len() != before;
                }
            }
            if touched {
                changed.push(app.stored.id.clone());
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        self.save(&apps)?;
        self.state.write().apps = apps;
        for id in &changed {
            self.apply(id);
        }
        Ok(changed.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;
    use serde_json::json;
    use tool_registry::{ToolContext, ToolError};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SHOP: &str = include_str!("../../../crates/openapi-tools/tests/fixtures/shop.yaml");
    const TOKEN: &str = "sk-live-0123456789abcdef";

    struct Rig {
        host: Arc<OpenApiHost>,
        registry: ToolRegistry,
        secrets: Arc<MemoryStore>,
        learned: Arc<parking_lot::Mutex<Vec<String>>>,
        dir: tempfile::TempDir,
    }

    fn start(
        dir: &Path,
        secrets: &Arc<MemoryStore>,
        registry: &ToolRegistry,
        learned: &Arc<parking_lot::Mutex<Vec<String>>>,
        private: bool,
    ) -> Arc<OpenApiHost> {
        let learned = Arc::clone(learned);
        let host = OpenApiHost::start(
            dir,
            registry.clone(),
            Arc::clone(secrets) as Arc<dyn SecretStore>,
            Arc::new(move |v| learned.lock().push(v.to_owned())),
        );
        if private {
            host.allowing_private_hosts()
        } else {
            host
        }
    }

    fn rig(private: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemoryStore::new());
        let registry = ToolRegistry::new();
        let learned = Arc::default();
        let host = start(dir.path(), &secrets, &registry, &learned, private);
        Rig {
            host,
            registry,
            secrets,
            learned,
            dir,
        }
    }

    fn stored(r: &Rig) -> Option<String> {
        r.secrets
            .get("openapi/shop/credential")
            .unwrap()
            .map(|v| v.expose().to_owned())
    }

    fn input(base: &str) -> OpenApiAppInput {
        serde_json::from_value(json!({
            "name": "Shop",
            "spec_text": SHOP,
            "base_url": format!("{base}/api"),
            "auth": { "kind": "bearer", "secret": TOKEN },
        }))
        .unwrap()
    }

    fn enable(names: &[(&str, Option<&[&str]>)]) -> OpenApiAppUpdate {
        OpenApiAppUpdate {
            name: None,
            base_url: None,
            auth: None,
            operations: Some(
                names
                    .iter()
                    .map(|(n, b)| OpenApiEnabledOperation {
                        name: format!("api_shop_{n}"),
                        bots: b.map(|l| l.iter().map(|s| (*s).to_owned()).collect()),
                    })
                    .collect(),
            ),
        }
    }

    fn ctx(bot: &str) -> ToolContext {
        ToolContext {
            bot_id: Some(bot.into()),
            ..ToolContext::default()
        }
    }

    #[tokio::test]
    async fn adding_an_api_lists_its_operations_off_and_keeps_the_credential_in_the_store_only() {
        let server = MockServer::start().await;
        let r = rig(true);
        let app = r.host.add(input(&server.uri())).await.unwrap();
        assert_eq!(app.id, "shop");
        assert_eq!(app.base_url, format!("{}/api", server.uri()));
        assert!(app.auth.secret_set);
        assert_eq!(app.enabled_count, 0);
        assert!(app.operations.len() >= 6 && app.operations.iter().all(|o| !o.enabled));
        // Nothing is a tool until the owner turns it on.
        assert!(
            r.registry
                .list()
                .iter()
                .all(|s| !s.name.starts_with("api_"))
        );
        // The credential is in the secret store, taught to the redactor, and nowhere else.
        assert_eq!(stored(&r).as_deref(), Some(TOKEN));
        assert!(r.learned.lock().iter().any(|v| v == TOKEN));
        assert!(
            !serde_json::to_string(&r.host.list())
                .unwrap()
                .contains(TOKEN)
        );
        let toml = std::fs::read_to_string(r.dir.path().join(OPENAPI_APPS_FILE)).unwrap();
        assert!(!toml.contains(TOKEN) && toml.contains("bearer"), "{toml}");
        let saved = std::fs::read_to_string(r.dir.path().join("openapi-specs/shop.spec")).unwrap();
        assert!(!saved.contains(TOKEN));
    }

    #[tokio::test]
    async fn enabled_operations_become_tools_only_the_chosen_bots_can_see_and_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/products"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": 1}])))
            .mount(&server)
            .await;
        let r = rig(true);
        r.host.add(input(&server.uri())).await.unwrap();
        let app = r
            .host
            .update(
                "shop",
                enable(&[("listProducts", Some(&["bot-a"])), ("createSession", None)]),
            )
            .await
            .unwrap();
        assert_eq!(app.enabled_count, 2);

        let list = r.registry.get("api_shop_listProducts").expect("registered");
        assert!(list.visible_to(Some("bot-a")));
        assert!(!list.visible_to(Some("bot-b")));
        assert!(!list.visible_to(None));
        // Only the two enabled operations exist.
        assert!(r.registry.get("api_shop_createProduct").is_none());

        let denied = list.call(&ctx("bot-b"), json!({})).await.unwrap_err();
        assert!(matches!(denied, ToolError::PermissionDenied(_)), "{denied}");
        assert!(server.received_requests().await.unwrap().is_empty());
        let out = list.call(&ctx("bot-a"), json!({})).await.unwrap();
        assert!(format!("{:?}", out.content).contains("\"id\""));
        assert!(!format!("{:?}", out.content).contains(TOKEN));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);

        // Open to every bot, and a money-like operation: always asks, in the Money category.
        let pay = r.registry.get("api_shop_createSession").unwrap();
        assert!(pay.visible_to(Some("bot-b")) && pay.visible_to(Some("anyone")));
        assert!(pay.always_ask());
        assert_eq!(pay.spec().category, protocol::PermissionCategory::Money);
        assert!(!list.always_ask());
        assert_eq!(list.spec().effect, Effect::Read);
    }

    #[tokio::test]
    async fn changes_apply_at_once_and_survive_a_restart() {
        let server = MockServer::start().await;
        let r = rig(true);
        r.host.add(input(&server.uri())).await.unwrap();
        r.host
            .update(
                "shop",
                enable(&[("listProducts", None), ("deleteProduct", Some(&["a"]))]),
            )
            .await
            .unwrap();
        assert!(r.registry.get("api_shop_deleteProduct").is_some());
        // Turning one off removes its tool.
        r.host
            .update("shop", enable(&[("listProducts", None)]))
            .await
            .unwrap();
        assert!(r.registry.get("api_shop_deleteProduct").is_none());
        assert!(r.registry.get("api_shop_listProducts").is_some());

        // A new daemon over the same files registers the same tools again.
        let registry = ToolRegistry::new();
        let again = start(r.dir.path(), &r.secrets, &registry, &Arc::default(), true);
        assert!(registry.get("api_shop_listProducts").is_some());
        assert_eq!(again.list()[0].enabled_count, 1);
        assert!(again.list()[0].auth.secret_set);
    }

    #[tokio::test]
    async fn a_deleted_bot_leaves_the_lists_and_an_empty_list_never_means_everyone() {
        let server = MockServer::start().await;
        let r = rig(true);
        r.host.add(input(&server.uri())).await.unwrap();
        r.host
            .update(
                "shop",
                enable(&[
                    ("listProducts", Some(&["a", "b"])),
                    ("getFile", Some(&["a"])),
                    ("settle", None),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(r.host.forget_bot("a").await.unwrap(), 1);
        let app = r.host.get("shop").unwrap();
        let by = |n: &str| {
            app.operations
                .iter()
                .find(|o| o.name == format!("api_shop_{n}"))
                .unwrap()
                .clone()
        };
        assert_eq!(by("listProducts").bots, Some(vec!["b".to_owned()]));
        let file = by("getFile");
        assert!(file.enabled && file.bots == Some(vec![]));
        assert!(by("settle").enabled && by("settle").bots.is_none());
        let tool = r.registry.get("api_shop_getFile").unwrap();
        assert!(
            !tool.visible_to(Some("a")) && !tool.visible_to(Some("b")) && !tool.visible_to(None)
        );
        assert_eq!(r.host.forget_bot("a").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn remove_deletes_the_tools_the_document_and_the_credential() {
        let server = MockServer::start().await;
        let r = rig(true);
        r.host.add(input(&server.uri())).await.unwrap();
        r.host
            .update("shop", enable(&[("listProducts", None)]))
            .await
            .unwrap();
        r.host.remove("shop").await.unwrap();
        assert!(r.registry.get("api_shop_listProducts").is_none());
        assert!(stored(&r).is_none());
        assert!(!r.dir.path().join("openapi-specs/shop.spec").exists());
        assert!(r.host.list().is_empty(), "no app was saved");
        assert!(matches!(
            r.host.remove("shop").await,
            Err(OpenApiAppsError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn bad_input_is_refused_with_a_reason() {
        let server = MockServer::start().await;
        let r = rig(true);
        let bad = |v: serde_json::Value| serde_json::from_value::<OpenApiAppInput>(v).unwrap();
        // A credential kind without a credential, a bad header name, junk and an unusable base URL.
        for body in [
            json!({"name": "a", "spec_text": SHOP, "base_url": server.uri(), "auth": {"kind": "bearer"}}),
            json!({"name": "a", "spec_text": SHOP, "base_url": server.uri(), "auth": {"kind": "api_key_header", "name": "bad header", "secret": "k"}}),
            json!({"name": "a", "spec_text": SHOP, "base_url": server.uri(), "auth": {"kind": "basic", "secret": "k"}}),
            json!({"name": "a", "spec_text": "not an openapi document", "base_url": server.uri()}),
            json!({"name": "a", "spec_text": SHOP, "base_url": "ftp://x.example/api"}),
            // A relative `servers` entry and no address of ours.
            json!({"name": "a", "spec_text": SHOP.replace("http://placeholder.invalid/api", "/api")}),
        ] {
            let err = r.host.add(bad(body.clone())).await.unwrap_err();
            assert!(
                matches!(err, OpenApiAppsError::Invalid(_)),
                "{body}: {err:?}"
            );
        }
        assert!(r.host.list().is_empty(), "no app was saved");
        assert!(
            r.secrets.list_names().unwrap().is_empty(),
            "no secret was stored"
        );

        r.host.add(input(&server.uri())).await.unwrap();
        let unknown = r
            .host
            .update("shop", enable(&[("nope", None)]))
            .await
            .unwrap_err();
        assert!(matches!(unknown, OpenApiAppsError::Invalid(_)));
        assert!(matches!(
            r.host.get("missing"),
            Err(OpenApiAppsError::NotFound(_))
        ));
        // No document URL to refresh from.
        assert!(matches!(
            r.host.refresh("shop").await,
            Err(OpenApiAppsError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn the_documents_servers_are_used_only_from_its_own_host_and_never_over_the_owners_address()
     {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = MockServer::start().await;
        let serve = |servers_url: String| {
            let text = SHOP.replace("http://placeholder.invalid/api", &servers_url);
            Mock::given(method("GET"))
                .and(path("/spec.yaml"))
                .respond_with(ResponseTemplate::new(200).set_body_string(text))
        };
        let from_url = || {
            serde_json::from_value::<OpenApiAppInput>(
                json!({"name": "Shop", "spec_url": format!("{}/spec.yaml", server.uri())}),
            )
            .unwrap()
        };
        let r = rig(true);

        // Pasted text has no host to compare with: the owner must give the address.
        let text_only = json!({"name": "a", "spec_text": SHOP});
        let err = r
            .host
            .add(serde_json::from_value(text_only).unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, OpenApiAppsError::Invalid(m) if m.contains("base URL")),
            "{err:?}"
        );

        // A server on another host than the document's is not taken.
        serve("https://api.elsewhere.example/api".into())
            .mount(&server)
            .await;
        let err = r.host.add(from_url()).await.unwrap_err();
        assert!(matches!(err, OpenApiAppsError::Invalid(_)), "{err:?}");
        assert!(r.host.list().is_empty(), "no app was saved");
        server.reset().await;

        // The same host is fine, and so is the owner's own address over any server.
        serve(format!("{}/api", server.uri())).mount(&server).await;
        let app = r.host.add(from_url()).await.unwrap();
        assert_eq!(app.base_url, format!("{}/api", server.uri()));
        server.reset().await;
        serve("https://api.elsewhere.example/api".into())
            .mount(&server)
            .await;
        let mut own = from_url();
        own.name = "Mine".into();
        own.base_url = Some(format!("{}/mine", server.uri()));
        let app = r.host.add(own).await.unwrap();
        assert_eq!(app.base_url, format!("{}/mine", server.uri()));
        // A refresh keeps the owner's address although the document names another.
        let app = r.host.refresh(&app.id).await.unwrap();
        assert_eq!(app.base_url, format!("{}/mine", server.uri()));
    }

    #[tokio::test]
    async fn private_addresses_are_refused_for_the_api_and_for_the_document_by_default() {
        let r = rig(false);
        let local = |extra: serde_json::Value| {
            let mut v = json!({"name": "a", "base_url": "http://127.0.0.1:9/api"});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<OpenApiAppInput>(v).unwrap()
        };
        let err = r
            .host
            .add(local(json!({"spec_text": SHOP})))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, OpenApiAppsError::Invalid(m) if m.contains("private")),
            "{err:?}"
        );
        let err = r
            .host
            .add(local(
                json!({"spec_url": "http://169.254.169.254/latest/meta-data/"}),
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, OpenApiAppsError::Invalid(m) if m.contains("private")),
            "{err:?}"
        );
        assert!(r.host.list().is_empty(), "no app was saved");
    }

    #[tokio::test]
    async fn at_most_fifty_operations_can_be_on_and_the_secret_can_be_replaced_or_dropped() {
        let server = MockServer::start().await;
        let r = rig(true);
        r.host.add(input(&server.uri())).await.unwrap();
        // A new token replaces the stored one; leaving it out keeps it.
        let mut up = enable(&[]);
        up.operations = None;
        up.auth = Some(
            serde_json::from_value(json!({"kind": "bearer", "secret": "second-token-value"}))
                .unwrap(),
        );
        r.host.update("shop", up).await.unwrap();
        assert_eq!(stored(&r).as_deref(), Some("second-token-value"));
        let mut keep = enable(&[]);
        keep.operations = None;
        keep.auth = Some(serde_json::from_value(json!({"kind": "bearer"})).unwrap());
        r.host.update("shop", keep).await.unwrap();
        assert_eq!(stored(&r).as_deref(), Some("second-token-value"));
        // No sign-in any more: the stored secret goes.
        let mut none = enable(&[]);
        none.operations = None;
        none.auth = Some(serde_json::from_value(json!({"kind": "none"})).unwrap());
        let app = r.host.update("shop", none).await.unwrap();
        assert!(!app.auth.secret_set);
        assert!(stored(&r).is_none());
    }

    #[test]
    fn ids_are_short_lower_case_and_never_empty() {
        assert_eq!(slug("GitHub REST API"), "github_rest_api");
        assert_eq!(slug("  --  "), "api");
        assert!(slug("Çok Uzun Bir Servis Adı Burada Yazıyor").len() <= MAX_ID_LEN);
        assert!(!slug("x".repeat(100).as_str()).ends_with('_'));
    }
}

//! `POST /v1/import/scan` and `POST /v1/import/run`: bring the setups of other assistants along.
//!
//! The scan reads the files of the machine this daemon runs on (so a remote server imports from
//! its own files) and writes nothing. The run scans again, takes only the items the owner ticked
//! and writes them:
//! - an MCP server is created **disabled**; a value of its `env` / `headers` is stored in the
//!   secret store only when its secret name is in `confirmed_secrets`, otherwise it is left out
//!   and reported. The values never leave the daemon: the response carries names only;
//! - a skill is installed through the skill hub (scan, fingerprint, no code run);
//! - instruction files are returned as text for the caller to add to a bot through the normal
//!   bot edit route, which keeps every check that route has.
//!
//! Owner only (the local owner, or the administrator desktop of a host). Audit rows carry item
//! names and counts, never a config body or a value.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use assistant_import::{Item, Kind, Plan, Roots, ScanOptions, build_plan};
use axum::extract::State;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use skills::hub::{HubPaths, InstallSource, RiskLevel, SkillHub};
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorBody};
use crate::mcp::McpServerInput;
use crate::mcp_routes::{MaybeActor, audited};
use crate::routes::ApiJson;
use crate::skill_hub::install_with;
use crate::skill_management::local_owner;
use crate::{AppState, PeerIdentity};

/// Most items one run may carry.
const MAX_IDS: usize = 500;
/// Longest folder path accepted.
const MAX_FOLDER_LEN: usize = 1024;

/// What to look at.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportScanInput {
    /// Only this app: `claude`, `cursor`, `vscode`, `codex`, `openclaw`, `hermes` or `folder`.
    #[serde(default)]
    pub only: Option<String>,
    /// A folder (on the machine of this daemon) of skill and prompt markdown. Read only when set.
    #[serde(default)]
    pub folder: Option<String>,
}

/// Import the ticked items.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportRunInput {
    /// Same as in the scan.
    #[serde(default)]
    pub only: Option<String>,
    /// Same as in the scan.
    #[serde(default)]
    pub folder: Option<String>,
    /// `id` of every ticked item.
    pub ids: Vec<String>,
    /// `name` of every secret the owner agreed to move into the secret store.
    #[serde(default)]
    pub confirmed_secrets: Vec<String>,
}

/// A value that moves to the secret store when the owner confirms it. Never the value.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportSecretDto {
    /// The secret's name in the store.
    pub name: String,
    /// The environment variable or header it belongs to.
    pub key: String,
    /// `env` or `header`.
    pub place: String,
}

/// One line of the preview.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportItemDto {
    /// Stable between a scan and a run.
    pub id: String,
    /// `claude`, `cursor`, `vscode`, `codex`, `openclaw`, `hermes` or `folder`.
    pub app: String,
    /// `mcp_server`, `skill`, `instructions` or `other`.
    pub kind: String,
    /// Server or skill name, or a file name.
    pub name: String,
    /// The file it came from (`~/.cursor/mcp.json`).
    pub source: String,
    /// Can be imported.
    pub import: bool,
    /// Ticked by default.
    pub default_on: bool,
    /// Outcome as a code (`ready`, `already_have`, `unpinned`, ...).
    pub reason: String,
    /// Outcome in English.
    pub note: String,
    /// A safe one-line summary (never a value).
    pub detail: Option<String>,
    /// Values that go to the secret store (names only).
    pub secrets: Vec<ImportSecretDto>,
    /// Names whose value was only a variable reference and is not copied.
    pub dropped: Vec<String>,
}

/// The preview.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportScanDto {
    /// Everything found, importable or not.
    pub items: Vec<ImportItemDto>,
}

/// What happened to one item.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportResultDto {
    /// The item's `id`.
    pub id: String,
    /// `imported`, `skipped` or `failed`.
    pub status: String,
    /// Outcome code when it was skipped or failed.
    pub reason: Option<String>,
    /// Outcome in English.
    pub message: String,
    /// Secret names stored.
    pub secrets_stored: Vec<String>,
    /// Environment variables / headers left out (not confirmed, or not storable).
    pub secrets_left_out: Vec<String>,
    /// Instruction items: the text to add to a bot.
    pub text: Option<String>,
}

/// The outcome of a run.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportRunDto {
    /// One entry per requested id, in order.
    pub results: Vec<ImportResultDto>,
}

fn options(only: Option<String>, folder: Option<String>) -> Result<ScanOptions, ApiError> {
    if let Some(only) = &only
        && assistant_import::App::from_key(only).is_none()
    {
        return Err(ApiError::bad_request("unknown app in `only`"));
    }
    let folder = match folder.filter(|f| !f.trim().is_empty()) {
        None => None,
        Some(f) => {
            let path = PathBuf::from(f.trim());
            if f.len() > MAX_FOLDER_LEN || f.contains('\0') || !path.is_absolute() {
                return Err(ApiError::bad_request(
                    "`folder` must be a full path on the machine of this daemon",
                ));
            }
            Some(path)
        }
    };
    if only.as_deref() == Some("folder") && folder.is_none() {
        return Err(ApiError::bad_request("`folder` is needed to scan a folder"));
    }
    Ok(ScanOptions { only, folder })
}

fn roots(s: &AppState) -> Result<Roots, ApiError> {
    s.import_roots()
        .or_else(Roots::from_env)
        .ok_or_else(|| ApiError::unavailable("cannot tell where the home folder is"))
}

fn taken_servers(s: &AppState) -> BTreeSet<String> {
    s.mcp()
        .map(|m| {
            m.list()
                .into_iter()
                .map(|server| server.name.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Build the plan on a blocking thread (it reads files and scans skills).
async fn plan_of(s: &AppState, opts: ScanOptions) -> Result<Plan, ApiError> {
    let roots = roots(s)?;
    let data_dir = s.data_dir();
    let taken = taken_servers(s);
    tokio::task::spawn_blocking(move || build_plan(&roots, &data_dir, &taken, &opts))
        .await
        .map_err(|_| ApiError::internal("import worker failed"))
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::McpServer => "mcp_server",
        Kind::Skill => "skill",
        Kind::Instructions => "instructions",
        Kind::Other => "other",
    }
}

fn code<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn item_dto(s: &AppState, item: &Item) -> ImportItemDto {
    let redact = |text: &str| s.agent().redact_external_text(text.to_owned());
    ImportItemDto {
        id: item.id.clone(),
        app: item.app.key().to_owned(),
        kind: kind_label(item.kind).to_owned(),
        name: redact(&item.name),
        source: redact(&item.source),
        import: item.import,
        default_on: item.default_on,
        reason: code(&item.reason),
        note: redact(&item.note),
        detail: item.detail.as_deref().map(redact),
        secrets: item
            .secrets
            .iter()
            .map(|slot| ImportSecretDto {
                name: slot.name.clone(),
                key: slot.key.clone(),
                place: slot.place.to_owned(),
            })
            .collect(),
        dropped: item.dropped.clone(),
    }
}

/// Look for the setups of other assistants on this machine. Reads files; writes nothing.
#[utoipa::path(post, path = "/v1/import/scan", tag = "import", security(("bearer" = [])),
    request_body = ImportScanInput,
    responses((status = 200, body = ImportScanDto), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody)))]
pub(crate) async fn scan(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    ApiJson(input): ApiJson<ImportScanInput>,
) -> Result<Json<ImportScanDto>, ApiError> {
    local_owner(&s, peer.as_ref())?;
    let opts = options(input.only, input.folder)?;
    let plan = plan_of(&s, opts).await?;
    Ok(Json(ImportScanDto {
        items: plan.items.iter().map(|i| item_dto(&s, i)).collect(),
    }))
}

fn result(
    id: &str,
    status: &str,
    reason: Option<&str>,
    message: impl Into<String>,
) -> ImportResultDto {
    ImportResultDto {
        id: id.to_owned(),
        status: status.to_owned(),
        reason: reason.map(str::to_owned),
        message: message.into(),
        secrets_stored: Vec::new(),
        secrets_left_out: Vec::new(),
        text: None,
    }
}

/// Import what the owner ticked. Scans again first: what is written is what is on disk now.
#[utoipa::path(post, path = "/v1/import/run", tag = "import", security(("bearer" = [])),
    request_body = ImportRunInput,
    responses((status = 200, body = ImportRunDto), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody)))]
pub(crate) async fn run(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    actor: MaybeActor,
    ApiJson(input): ApiJson<ImportRunInput>,
) -> Result<Json<ImportRunDto>, ApiError> {
    local_owner(&s, peer.as_ref())?;
    if input.ids.len() > MAX_IDS || input.confirmed_secrets.len() > MAX_IDS * 8 {
        return Err(ApiError::bad_request("too many items"));
    }
    let opts = options(input.only, input.folder)?;
    let plan = plan_of(&s, opts).await?;
    let confirmed: BTreeSet<&str> = input.confirmed_secrets.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut results = Vec::new();
    for id in &input.ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let Some(item) = plan.item(id) else {
            results.push(result(
                id,
                "failed",
                Some("not_found"),
                "not found any more: look again",
            ));
            continue;
        };
        if !item.import {
            let mut r = result(id, "skipped", Some(&code(&item.reason)), item.note.clone());
            r.message = s.agent().redact_external_text(r.message);
            results.push(r);
            continue;
        }
        results.push(match item.kind {
            Kind::McpServer => import_server(&s, &actor, &plan, item, &confirmed).await,
            Kind::Skill => import_skill(&s, &plan, item).await,
            Kind::Instructions => import_instructions(&plan, item),
            Kind::Other => result(id, "skipped", Some(&code(&item.reason)), item.note.clone()),
        });
    }
    let imported = results.iter().filter(|r| r.status == "imported").count();
    let _ = s.storage().append_audit(
        s.user_id(),
        None,
        "local_owner",
        "import.run",
        &serde_json::json!({ "requested": input.ids.len(), "imported": imported }),
    );
    Ok(Json(ImportRunDto { results }))
}

/// Same name rules as `POST /v1/secrets/{name}`: the generic sink refuses these.
fn storable(name: &str) -> bool {
    secret_store::validate_name(name).is_ok()
        && !secret_store::is_host_credential(name)
        && !secret_store::is_reserved_name(name)
        && !secret_store::is_fill_name(name)
        && !secret_store::is_managed_oauth_name(name)
}

async fn import_server(
    s: &AppState,
    actor: &MaybeActor,
    plan: &Plan,
    item: &Item,
    confirmed: &BTreeSet<&str>,
) -> ImportResultDto {
    let fail = |reason: &str, why: &str| result(&item.id, "failed", Some(reason), why);
    let Some(entry) = plan.servers.iter().find(|e| e.item_id == item.id) else {
        return fail("not_found", "not found any more: look again");
    };
    let Some(host) = s.mcp() else {
        return fail(
            "failed",
            "server management is not available in this daemon",
        );
    };
    let mut config = entry.parsed.config.clone();
    config.enabled = false;
    // Only values the owner confirmed (one by one) are moved; the rest is left out.
    let mut to_store: BTreeMap<String, String> = BTreeMap::new();
    let values: BTreeMap<&str, &str> = entry
        .parsed
        .secrets
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let mut left_out = Vec::new();
    config.secret_env.retain(|key, name| {
        let keep = confirmed.contains(name.as_str()) && storable(name);
        if keep {
            if let Some(v) = values.get(name.as_str()) {
                to_store.insert(name.clone(), (*v).to_owned());
            }
        } else {
            left_out.push(key.clone());
        }
        keep
    });
    config.secret_headers.retain(|key, name| {
        let keep = confirmed.contains(name.as_str()) && storable(name);
        if keep {
            if let Some(v) = values.get(name.as_str()) {
                to_store.insert(name.clone(), (*v).to_owned());
            }
        } else {
            left_out.push(key.clone());
        }
        keep
    });
    let input: McpServerInput = match serde_json::from_value(assistant_import::server_body(&config))
    {
        Ok(i) => i,
        Err(_) => return fail("invalid", "the server settings are not valid"),
    };
    let name = config.id.clone();
    // The server first (disabled, so a secret it points to is not read yet), then its secrets;
    // a secret that cannot be stored removes the new server again.
    let created = audited(s, actor, "mcp.server_create", &name, async {
        Ok(host.add(input).await?)
    })
    .await;
    if let Err(e) = created {
        return match e.status().as_u16() {
            409 => fail("already_have", "a server with this name already exists"),
            _ => fail("failed", "the server could not be created"),
        };
    }
    let Some(store) = s.secret_store() else {
        if to_store.is_empty() {
            return done_server(item, Vec::new(), left_out);
        }
        let _ = host.remove(&name).await;
        return fail(
            "secret_failed",
            "this daemon has no secret store: nothing was imported",
        );
    };
    let mut stored = Vec::new();
    for (secret, value) in to_store {
        let store = store.clone();
        let label = secret.clone();
        let written = audited(s, actor, "secret.set", &label, async move {
            tokio::task::spawn_blocking(move || store.set(&secret, &value))
                .await
                .map_err(ApiError::internal)?
                .map_err(|_| ApiError::internal("cannot store the secret"))
        })
        .await;
        if written.is_err() {
            let _ = host.remove(&name).await;
            return fail(
                "secret_failed",
                "a secret could not be stored: the server was not imported",
            );
        }
        stored.push(label);
    }
    done_server(item, stored, left_out)
}

fn done_server(item: &Item, stored: Vec<String>, left_out: Vec<String>) -> ImportResultDto {
    let mut r = result(&item.id, "imported", None, "created and switched off");
    r.secrets_stored = stored;
    r.secrets_left_out = left_out;
    r
}

async fn import_skill(s: &AppState, plan: &Plan, item: &Item) -> ImportResultDto {
    let Some(entry) = plan.skills.iter().find(|e| e.item_id == item.id) else {
        return result(
            &item.id,
            "failed",
            Some("not_found"),
            "not found any more: look again",
        );
    };
    let dir = entry.dir.clone();
    let state = s.clone();
    // The error is `(reason, message)`.
    let installed = tokio::task::spawn_blocking(move || {
        let hub = SkillHub::new(HubPaths::from_data_dir(&state.data_dir()));
        let source = InstallSource::Local(dir);
        let preview = hub
            .preview(&source)
            .map_err(|_| ("failed", "the skill cannot be read any more"))?;
        // The scan is repeated at install time: only a clean skill goes in.
        if preview.report.risk() != RiskLevel::Low {
            return Err((
                "review_first",
                "the safety scan result changed since the preview; read it first",
            ));
        }
        let _guard = state.0.skill_mutations.lock();
        install_with(&state, &hub, &source, &preview.report.fingerprint)
            .map(|_| ())
            .map_err(|_| {
                (
                    "failed",
                    "the skill could not be installed (is the name already used?)",
                )
            })
    })
    .await;
    match installed {
        Ok(Ok(())) => result(&item.id, "imported", None, "installed"),
        Ok(Err((reason, why))) => result(&item.id, "failed", Some(reason), why),
        Err(_) => result(&item.id, "failed", Some("failed"), "import worker failed"),
    }
}

fn import_instructions(plan: &Plan, item: &Item) -> ImportResultDto {
    let Some(entry) = plan.instructions.iter().find(|e| e.item_id == item.id) else {
        return result(
            &item.id,
            "failed",
            Some("not_found"),
            "not found any more: look again",
        );
    };
    let mut r = result(&item.id, "imported", None, "ready to add to a bot");
    r.text = Some(entry.text.clone());
    r
}

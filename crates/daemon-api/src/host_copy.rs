//! "Copy my setup to this server": the owner's computer sends its provider
//! keys, bots and default model to a paired host it administers.
//!
//! The LOCAL daemon does the work, never the web view: the keys are read from
//! this computer's secret store at the moment the owner confirms, only for the
//! providers that were ticked, and travel straight into the paired peer link
//! (end-to-end encrypted) to the host's administration routes
//! (`host_admin.rs`). The UI sees names and per-item results, never a key.
//!
//! Local owner only: the route lives under `/v1/hosts`, which the peer router
//! refuses to every paired role and the scoped MCP token filter never lists.
//! Nothing here is a tool, and no bot can reach it.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::HeaderMap,
};
use provider_catalog::{ProviderConfig, config_path};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    ApiError, AppState,
    host_links::{HostLinkError, HostLinks},
    routes::ApiJson,
};

/// Most items one copy may carry (providers + bots).
const MAX_ITEMS: usize = 64;

static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CopyProviderRow {
    pub id: String,
    pub has_key: bool,
    pub model: Option<String>,
    pub custom: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CopyBotRow {
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    /// The bot cannot be copied (it runs on a CLI engine).
    pub blocked: Option<String>,
    /// Schedules of this bot on this computer (270-B).
    #[serde(default)]
    pub schedules: u32,
    /// What the server says about this bot: `mapped` (already linked), `name_taken` (a bot of
    /// that name that this flow did not create exists there: the owner must choose), `free`,
    /// `separate` (the owner chose to keep them apart) or absent (the server was not asked).
    #[serde(default)]
    pub host_state: Option<String>,
    /// Chats of this bot on this computer, and their messages.
    #[serde(default)]
    pub chats: u32,
    #[serde(default)]
    pub messages: u32,
}

/// What a copy could send. Names only; no secret value.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CopyPlanDto {
    pub providers: Vec<CopyProviderRow>,
    pub bots: Vec<CopyBotRow>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    /// What the host says about this computer: `true` when it may receive.
    pub host_accepts: Option<bool>,
    /// Stable code from the host when it would refuse (`host_admin_required`, `secrets_unavailable`).
    pub host_code: Option<String>,
    /// Command for the owner to run on the host, when one is needed.
    pub host_command: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // independent tick boxes of one dialog
pub struct CopyRequest {
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub bots: Vec<String>,
    /// Also set the host's default provider and model to this computer's.
    #[serde(default)]
    pub default_model: bool,
    /// Record the host as the bots' home on both sides when something moved.
    /// Nothing is deleted on this computer: it stays a working fallback.
    #[serde(default)]
    pub set_home: bool,
    /// Also move the schedules and triggers of the moved bots (the local copies are then
    /// disabled and marked "moved to <server>").
    #[serde(default)]
    pub schedules: bool,
    /// Also move the chat history of the moved bots (messages and run metadata; files and
    /// attachments named in them are not copied).
    #[serde(default)]
    pub chats: bool,
    /// Local bot ids the owner explicitly wants bound to the server bot that already has the
    /// same name (otherwise a name collision is refused).
    #[serde(default)]
    pub adopt: Vec<String>,
    /// Local bot ids the owner chose to keep separate from the server's same-named bot: they
    /// are skipped, with everything that belongs to them, and the answer is remembered.
    #[serde(default)]
    pub separate: Vec<String>,
}

/// `GET /v1/hosts/home`: the host adopted as the bots' home, if any.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HomeDto {
    pub host_id: Option<String>,
}

const LOCAL_HOME_FILE: &str = "home.json";

fn read_home(s: &AppState) -> Option<String> {
    std::fs::read(s.data_dir().join(LOCAL_HOME_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["host_id"].as_str().map(str::to_owned))
}

#[utoipa::path(get, operation_id = "hosts_home", path = "/v1/hosts/home", tag = "host", security(("bearer" = [])),
    responses((status = 200, body = HomeDto)))]
pub(crate) async fn home(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
) -> Result<Json<HomeDto>, ApiError> {
    local_only(peer.as_ref())?;
    Ok(Json(HomeDto {
        host_id: read_home(&s),
    }))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CopyItemResult {
    /// `provider`, `bot` or `default`.
    pub kind: String,
    pub id: String,
    pub name: String,
    /// `done`, `skipped` or `failed`.
    pub status: String,
    pub code: Option<String>,
    /// For `schedules` / `chats`: how many items moved now (messages for chats) ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// ... were already on the server ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<u32>,
    /// ... were left behind (webhook secret, too long, bot not moved) ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u32>,
    /// ... or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CopyReport {
    pub items: Vec<CopyItemResult>,
}

pub(crate) fn local_only(peer: Option<&Extension<crate::PeerIdentity>>) -> Result<(), ApiError> {
    if peer.is_some() {
        return Err(ApiError::forbidden("host links are local only"));
    }
    Ok(())
}

pub(crate) fn links(s: &AppState) -> Result<std::sync::Arc<dyn HostLinks>, ApiError> {
    s.host_links()
        .ok_or_else(|| ApiError::unavailable("host links are not available in this daemon"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

pub(crate) fn audit(
    s: &AppState,
    action: &str,
    detail: &serde_json::Value,
) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, "local-ui", action, detail)
        .map(|_| ())
        .map_err(ApiError::internal)
}

/// Provider ids that are plain HTTP providers (a `cli:` engine is a login of
/// the vendor's tool on THIS computer and never leaves it).
fn http_providers(
    cfg: &ProviderConfig,
) -> impl Iterator<Item = &provider_catalog::ProviderSettings> {
    cfg.providers.iter().filter(|p| !p.id.starts_with("cli:"))
}

fn bot_executor(config: &serde_json::Value) -> Option<String> {
    config
        .pointer("/models/executor")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The profile as the host should get it: what only makes sense on this
/// computer (folders, knowledge sources, an external engine) is left behind.
fn portable_config(config: &serde_json::Value) -> serde_json::Value {
    let mut config = config.clone();
    if let Some(obj) = config.as_object_mut() {
        for key in ["files", "knowledge", "engine"] {
            obj.remove(key);
        }
    }
    config
}

#[utoipa::path(get, operation_id = "hosts_copy_plan", path = "/v1/hosts/{id}/copy-setup", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path)), responses((status = 200, body = CopyPlanDto)))]
pub(crate) async fn plan(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    Path(id): Path<String>,
) -> Result<Json<CopyPlanDto>, ApiError> {
    local_only(peer.as_ref())?;
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    let cfg = ProviderConfig::load(&config_path(&s.data_dir()))
        .map_err(|e| ApiError::internal(format!("providers.toml: {e}")))?;
    let providers = http_providers(&cfg)
        .map(|p| CopyProviderRow {
            id: p.id.clone(),
            has_key: p.key.is_some(),
            model: p.default_model.clone(),
            custom: p.custom.is_some(),
        })
        .collect();
    let mut schedule_counts: std::collections::HashMap<uuid::Uuid, u32> =
        std::collections::HashMap::new();
    if let Ok(all) = s.schedule_store().and_then(|st| st.list()) {
        for sc in all {
            *schedule_counts.entry(sc.bot_id.0).or_default() += 1;
        }
    }
    let mut chat_counts: std::collections::HashMap<uuid::Uuid, (u32, u32)> =
        std::collections::HashMap::new();
    for chat in s.storage().list_chats(s.user_id())? {
        let n = u32::try_from(s.storage().list_messages(chat.id).map_or(0, |m| m.len()))
            .unwrap_or(u32::MAX);
        let e = chat_counts.entry(chat.bot_id.0).or_default();
        e.0 += 1;
        e.1 = e.1.saturating_add(n);
    }
    let bots = s
        .storage()
        .list_bots(s.user_id())?
        .into_iter()
        .map(|bot| {
            let executor = bot_executor(&bot.config);
            let (chats, messages) = chat_counts.get(&bot.id.0).copied().unwrap_or_default();
            CopyBotRow {
                host_state: None,
                schedules: schedule_counts.get(&bot.id.0).copied().unwrap_or_default(),
                chats,
                messages,
                id: bot.id.0.to_string(),
                name: bot.name,
                blocked: executor
                    .as_deref()
                    .is_some_and(|e| e.starts_with("cli:"))
                    .then(|| "cli_engine".to_owned()),
                model: executor,
            }
        })
        .collect();
    let default = cfg
        .effective_default()
        .filter(|d| !d.id.starts_with("cli:"));
    let mut plan = CopyPlanDto {
        providers,
        bots,
        default_provider: default.map(|d| d.id.clone()),
        default_model: default.and_then(|d| d.default_model.clone()),
        host_accepts: None,
        host_code: None,
        host_command: None,
    };
    // Best effort: ask the host whether it would accept, so the dialog can
    // say what to do before anything is ticked.
    if let Ok(call) = links(&s)?.call(&id, "GET", "/v1/admin/state", None).await {
        if call.status == 200 {
            {
                let granted = call.body["granted"].as_bool().unwrap_or(false);
                let secrets = call.body["secrets_available"].as_bool().unwrap_or(true);
                plan.host_accepts = Some(granted && secrets);
                if !granted {
                    plan.host_code = Some(crate::host_admin::codes::REQUIRED.into());
                    plan.host_command = call.body["confirm_command"].as_str().map(str::to_owned);
                } else if !secrets {
                    plan.host_code = Some(crate::host_admin::codes::NO_SECRETS.into());
                    plan.host_command = call.body["secrets_hint"].as_str().map(str::to_owned);
                }
            }
        } else {
            plan.host_accepts = Some(false);
            plan.host_code = call.body["error"]["code"].as_str().map(str::to_owned);
        }
    }
    // Ask the server which bots would collide by name, so the dialog can put the question
    // before anything is sent. A bot the owner already kept separate is not asked again.
    if plan.host_accepts == Some(true) {
        fill_host_states(&s, &id, &mut plan).await?;
    }
    Ok(Json(plan))
}

/// Ask the server which bots would collide by name (see `CopyBotRow::host_state`).
async fn fill_host_states(s: &AppState, id: &str, plan: &mut CopyPlanDto) -> Result<(), ApiError> {
    let probe: Vec<_> = plan
        .bots
        .iter()
        .filter(|b| b.blocked.is_none())
        .filter_map(|b| {
            Some(serde_json::json!({"source_bot_id": b.id.parse::<uuid::Uuid>().ok()?, "name": b.name}))
        })
        .collect();
    let states: std::collections::HashMap<String, String> = links(s)?
        .call(
            id,
            "POST",
            "/v1/admin/import/bots/check",
            Some(serde_json::json!({"bots": probe})),
        )
        .await
        .ok()
        .filter(|c| c.status == 200)
        .and_then(|c| c.body["items"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| {
            Some((
                i["source_bot_id"].as_str()?.to_owned(),
                i["state"].as_str()?.to_owned(),
            ))
        })
        .collect();
    for b in &mut plan.bots {
        b.host_state = match (states.get(&b.id), b.id.parse::<uuid::Uuid>()) {
            (Some(st), Ok(bid))
                if st == "name_taken"
                    && crate::host_import::kept_separate(&s.data_dir(), id, bid) =>
            {
                Some("separate".to_owned())
            }
            (Some(st), _) => Some(st.clone()),
            _ => None,
        };
    }
    Ok(())
}

fn item(kind: &str, id: &str, name: &str, status: &str, code: Option<&str>) -> CopyItemResult {
    CopyItemResult {
        kind: kind.into(),
        id: id.into(),
        name: name.into(),
        status: status.into(),
        code: code.map(str::to_owned),
        count: None,
        existing: None,
        skipped: None,
        failed: None,
    }
}

fn failure(e: &HostLinkError) -> String {
    match e {
        HostLinkError::NotFound => "host_not_found".into(),
        HostLinkError::Invalid(_) | HostLinkError::ProtocolMismatch => "invalid".into(),
        HostLinkError::Failed(_) => "host_unreachable".into(),
    }
}

#[utoipa::path(post, operation_id = "hosts_copy_setup", path = "/v1/hosts/{id}/copy-setup", tag = "host", security(("bearer" = [])),
    params(("id" = String, Path)), request_body = CopyRequest, responses((status = 200, body = CopyReport)))]
#[allow(clippy::too_many_lines)] // one linear send loop; each item reports on its own
pub(crate) async fn copy(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<CopyRequest>,
) -> Result<Json<CopyReport>, ApiError> {
    local_only(peer.as_ref())?;
    // Sending keys needs the owner key, not just the general token.
    s.require_owner(&headers)?;
    if !valid_id(&id) {
        return Err(ApiError::bad_request("invalid host id"));
    }
    if input.providers.len() + input.bots.len() > MAX_ITEMS {
        return Err(ApiError::bad_request("too many items in one copy"));
    }
    let links = links(&s)?;
    // Only the paired host the owner chose; the items are exactly the ids in
    // this request (nothing else is ever read from the secret store).
    if !links.list().iter().any(|h| h.id == id) {
        return Err(ApiError::not_found("host"));
    }
    let Ok(_running) = RUNNING.try_lock() else {
        return Err(ApiError::conflict("a copy is already running"));
    };
    // Names only: never a key, a profile or a ticket.
    audit(
        &s,
        "host_link.copy_setup",
        &serde_json::json!({"host": id, "providers": input.providers, "bots": input.bots,
                            "default_model": input.default_model, "schedules": input.schedules,
                            "chats": input.chats, "phase": "requested"}),
    )?;
    let cfg = ProviderConfig::load(&config_path(&s.data_dir()))
        .map_err(|e| ApiError::internal(format!("providers.toml: {e}")))?;
    let store = s.secret_store();
    let mut items = Vec::new();
    // A refusal that no later item can fix (no capability, no secret store)
    // stops the run; the remaining items are reported as skipped.
    let mut stop: Option<String> = None;

    for pid in &input.providers {
        if let Some(code) = &stop {
            items.push(item("provider", pid, pid, "skipped", Some(code)));
            continue;
        }
        let Some(settings) = http_providers(&cfg).find(|p| &p.id == pid) else {
            items.push(item("provider", pid, pid, "skipped", Some("not_copyable")));
            continue;
        };
        // The key is read here, for this one ticked provider, and dropped
        // after the request.
        let key = match (&settings.key, &store) {
            (Some(name), Some(store)) => store
                .get(name.name())
                .ok()
                .flatten()
                .map(|v| zeroize::Zeroizing::new(v.expose().to_owned())),
            _ => None,
        };
        if settings.key.is_some() && key.is_none() {
            items.push(item("provider", pid, pid, "failed", Some("key_unreadable")));
            continue;
        }
        let mut body = serde_json::json!({"model": settings.default_model});
        if let Some(k) = key {
            // The JSON value is the one remaining copy; the link scrubs it
            // after serialising (see `CliHostLinks::call`).
            body["api_key"] = k.as_str().into();
        }
        if let Some(custom) = &settings.custom {
            body["base_url"] = custom.base_url.clone().into();
        }
        match links
            .call(
                &id,
                "PUT",
                &format!("/v1/admin/providers/{pid}"),
                Some(body),
            )
            .await
        {
            Ok(call) if (200..300).contains(&call.status) => {
                items.push(item("provider", pid, pid, "done", None));
            }
            Ok(call) => {
                let code = call.body["error"]["code"]
                    .as_str()
                    .unwrap_or("rejected")
                    .to_owned();
                if matches!(
                    code.as_str(),
                    "host_admin_required" | "host_admin_forbidden" | "secrets_unavailable"
                ) {
                    stop = Some(code.clone());
                }
                items.push(item("provider", pid, pid, "failed", Some(&code)));
            }
            Err(e) => {
                stop = Some(failure(&e));
                items.push(item("provider", pid, pid, "failed", Some(&failure(&e))));
            }
        }
    }

    let local_bots = s.storage().list_bots(s.user_id())?;
    // Local bot id -> the bot on the host that THIS flow created or the owner adopted.
    let mut moved_bots: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
    for bid in &input.bots {
        let Some(bot) = local_bots.iter().find(|b| &b.id.0.to_string() == bid) else {
            items.push(item("bot", bid, bid, "skipped", Some("not_found")));
            continue;
        };
        let name = bot.name.clone();
        if let Some(code) = &stop {
            items.push(item("bot", bid, &name, "skipped", Some(code)));
            continue;
        }
        if input.separate.contains(bid) {
            // The owner said these are two different bots: remember it, send nothing of it.
            let kept = crate::host_import::remember_separate(&s.data_dir(), &id, bot.id.0).is_ok();
            items.push(item(
                "bot",
                bid,
                &name,
                "skipped",
                Some(if kept {
                    "kept_separate"
                } else {
                    "choice_not_saved"
                }),
            ));
            continue;
        }
        if bot_executor(&bot.config).is_some_and(|e| e.starts_with("cli:")) {
            items.push(item("bot", bid, &name, "skipped", Some("cli_engine")));
            continue;
        }
        // The host maps (this computer, this bot id) -> its own bot: created by this flow, or
        // adopted when the owner asked for it. A bot that merely has the same NAME is refused.
        let body = serde_json::json!({"source_bot_id": bot.id.0, "name": name,
            "config": portable_config(&bot.config), "adopt": input.adopt.contains(bid)});
        match links
            .call(&id, "POST", "/v1/admin/import/bots", Some(body))
            .await
        {
            Ok(call) if (200..300).contains(&call.status) => {
                match (call.body["status"].as_str(), call.body["code"].as_str()) {
                    (Some("done" | "adopted"), _) => {
                        moved_bots.insert(bot.id.0);
                        items.push(item("bot", bid, &name, "done", None));
                    }
                    (Some("exists"), _) => {
                        moved_bots.insert(bot.id.0);
                        items.push(item("bot", bid, &name, "skipped", Some("exists")));
                    }
                    (_, code) => {
                        items.push(item(
                            "bot",
                            bid,
                            &name,
                            "failed",
                            Some(code.unwrap_or("rejected")),
                        ));
                    }
                }
            }
            Ok(call) => {
                let code = call.body["error"]["code"]
                    .as_str()
                    .unwrap_or("rejected")
                    .to_owned();
                if matches!(
                    code.as_str(),
                    "host_admin_required" | "host_admin_forbidden"
                ) {
                    stop = Some(code.clone());
                }
                items.push(item("bot", bid, &name, "failed", Some(&code)));
            }
            Err(e) => {
                stop = Some(failure(&e));
                items.push(item("bot", bid, &name, "failed", Some(&failure(&e))));
            }
        }
    }

    if (input.schedules || input.chats) && stop.is_none() {
        let host_name = links
            .list()
            .into_iter()
            .find(|h| h.id == id)
            .map_or_else(|| id.clone(), |h| h.name);
        let mut chats_on_host = std::collections::HashSet::new();
        if input.chats {
            let (r, on_host) = crate::host_import::send_chats(&s, &*links, &id, &moved_bots).await;
            chats_on_host = on_host;
            items.push(r);
        }
        if input.schedules {
            items.push(
                crate::host_import::send_schedules(
                    &s,
                    &*links,
                    &id,
                    &host_name,
                    &moved_bots,
                    &chats_on_host,
                )
                .await,
            );
        }
    }
    if input.default_model {
        let default = cfg
            .effective_default()
            .filter(|d| !d.id.starts_with("cli:"));
        match (default, &stop) {
            (_, Some(code)) => {
                items.push(item("default", "default", "default", "skipped", Some(code)));
            }
            (None, None) => items.push(item(
                "default",
                "default",
                "default",
                "skipped",
                Some("no_default"),
            )),
            (Some(d), None) => {
                let body = serde_json::json!({"id": d.id, "model": d.default_model});
                let result = links
                    .call(&id, "PUT", "/v1/admin/default-provider", Some(body))
                    .await;
                items.push(match result {
                    Ok(call) if (200..300).contains(&call.status) => {
                        item("default", "default", &d.id, "done", None)
                    }
                    Ok(call) => item(
                        "default",
                        "default",
                        &d.id,
                        "failed",
                        Some(call.body["error"]["code"].as_str().unwrap_or("rejected")),
                    ),
                    Err(e) => item("default", "default", &d.id, "failed", Some(&failure(&e))),
                });
            }
        }
    }
    let moved = items.iter().any(|i| i.status == "done");
    if input.set_home && moved && stop.is_none() {
        let host_ok = links
            .call(
                &id,
                "PUT",
                "/v1/admin/home",
                Some(serde_json::json!({"adopted": true})),
            )
            .await
            .is_ok_and(|c| (200..300).contains(&c.status));
        let local_ok = std::fs::write(
            s.data_dir().join(LOCAL_HOME_FILE),
            serde_json::json!({"host_id": id}).to_string(),
        )
        .is_ok();
        items.push(item(
            "home",
            "home",
            "home",
            if host_ok && local_ok {
                "done"
            } else {
                "failed"
            },
            (!(host_ok && local_ok)).then_some("home_not_recorded"),
        ));
    }
    audit(
        &s,
        "host_link.copy_setup",
        &serde_json::json!({"host": id, "phase": "done",
            "done": items.iter().filter(|i| i.status == "done").count(),
            "failed": items.iter().filter(|i| i.status == "failed").count()}),
    )?;
    Ok(Json(CopyReport { items }))
}

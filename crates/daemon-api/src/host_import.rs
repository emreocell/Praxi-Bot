//! Moving bots' schedules and chat history to a host (card 270-B).
//!
//! The owner's computer (the LOCAL daemon, owner key required, see `host_copy.rs`) sends bots,
//! schedules, triggers, chats, run metadata and messages to the host's administration routes
//! below. They need exactly what the rest of the copy flow needs: a paired desktop that holds
//! host administration (`host_admin::require_admin`).
//!
//! Identity, not names. Everything that arrives is bound to the IMPORTING DEVICE (the peer key
//! of the caller) in a durable registry (`host-imports.json` in the host's data directory):
//! * a bot is mapped `(device, source bot id) -> host bot id` only when this flow CREATED the
//!   host bot or the owner explicitly asked to ADOPT the one with that name; a name collision
//!   is refused (`name_taken`), never silently matched;
//! * schedules, chats, runs and messages are bound to that mapping: a chat id the host already
//!   has is only accepted when this device imported it for the same bot, and every run and
//!   message must belong to a chat bound to this device.
//!
//! Two timers never run at once. A schedule is handed over in stages that each survive a lost
//! response or a restart, and the next sync finishes whatever was interrupted:
//! 1. the host imports it DISABLED (`staged`);
//! 2. the desktop FREEZES its local copy durably (`ScheduleStore::freeze`; an error stops here);
//! 3. the desktop asks the host to ACTIVATE it (`active`), which re-enables it only if the owner
//!    had it enabled.
//!
//! What travels, and what does not: schedules keep id, paused state and time zone (part of the
//! cron spec; no zone means UTC on every computer); next runs are recomputed on the host's
//! clock; webhook secrets never travel. Chats keep ids, roles, timestamps and exposure labels;
//! the text is redacted first; run rows are metadata only; every imported row carries an
//! `imported:` source so it is never "typed by the owner here" (typed-URL waiver); a reply of a
//! bot that did not move keeps its author's name and stays quoted foreign data. Files and
//! attachments named in messages are NOT copied. Memory has no export path and is not moved.
//!
//! Logs and audit rows carry counts and ids, never text.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use axum::{
    Extension, Json,
    extract::{Path as UrlPath, State},
    http::HeaderMap,
};
use protocol::{BotId, ChatId, RunId, TaskId};
use serde::{Deserialize, Serialize};
use storage::{Chat, Message, Run};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState, PeerIdentity,
    dto::{BotInput, ScheduleInput},
    host_admin::{Gate, audit, require_admin},
    host_copy::CopyItemResult,
    host_links::HostLinks,
    routes::ApiJson,
};

const MAX_SCHEDULES: usize = 4;
const MAX_CHATS: usize = 8;
const MAX_RUNS: usize = 32;
const MAX_MESSAGES: usize = 48;
/// Longest message text that is moved; a longer one is counted as skipped.
pub(crate) const MAX_CONTENT: usize = 24 * 1024;
/// Serialised size one page may reach (the admin body limit is 64 KiB).
const PAGE_BYTES: usize = 40 * 1024;
/// Most messages one move sends (the admin API allows 60 calls a minute).
const MAX_MESSAGES_TOTAL: usize = 4000;
const MAX_SCHEDULES_TOTAL: usize = 200;
/// Source label of every imported message (never `owner`).
const IMPORTED: &str = "imported";
/// Source label of an imported reply whose author bot does not exist here.
pub(crate) const IMPORTED_FOREIGN: &str = "imported_foreign:";

// ------------------------------------------------------------------ host registry

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatBind {
    device: String,
    host_bot: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Staged,
    Active,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchedBind {
    device: String,
    stage: Stage,
    want_enabled: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    /// `"<device>/<source bot id>"` -> host bot id.
    bots: BTreeMap<String, Uuid>,
    chats: BTreeMap<String, ChatBind>,
    /// run id -> device
    runs: BTreeMap<String, String>,
    schedules: BTreeMap<String, SchedBind>,
    /// The one-time "moved bots run on their own computer" normalisation ran
    /// (see [`AppState::normalise_moved_bots`]).
    #[serde(default)]
    computer_normalised: bool,
}

const REGISTRY_FILE: &str = "host-imports.json";
static REGISTRY_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn load(data: &Path) -> Registry {
    std::fs::read(data.join(REGISTRY_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write `bytes` to `path` so that a crash leaves the old or the new file, never a torn one:
/// write a temporary file, flush it to disk (`fsync`), then rename it over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn save(data: &Path, reg: &Registry) -> Result<(), ApiError> {
    serde_json::to_vec(reg)
        .map_err(ApiError::internal)
        .and_then(|b| write_atomic(&data.join(REGISTRY_FILE), &b).map_err(ApiError::internal))
}

fn bot_key(device: &str, source: Uuid) -> String {
    format!("{device}/{source}")
}

fn device_of(peer: Option<&Extension<PeerIdentity>>) -> String {
    peer.map(|p| p.0.key.clone()).unwrap_or_default()
}

fn plain(text: &str, max: usize) -> bool {
    text.len() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

// ------------------------------------------------------------------ host side: bots

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportBotInput {
    /// The bot's id on the sending computer.
    pub source_bot_id: Uuid,
    pub name: String,
    pub config: serde_json::Value,
    /// The owner asked to bind this bot to the host bot that already has its name.
    #[serde(default)]
    pub adopt: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportBotResult {
    /// `done` (created), `exists` (already mapped), `adopted` or `failed`.
    pub status: String,
    /// `name_taken`: a different bot with this name already exists on the server.
    pub code: Option<String>,
    pub host_bot_id: Option<Uuid>,
}

fn bot_result(status: &str, code: Option<&str>, id: Option<Uuid>) -> Json<ImportBotResult> {
    Json(ImportBotResult {
        status: status.into(),
        code: code.map(str::to_owned),
        host_bot_id: id,
    })
}

#[utoipa::path(post, operation_id = "admin_import_bot", path = "/v1/admin/import/bots", tag = "admin", security(("bearer" = [])),
    request_body = ImportBotInput,
    responses((status = 200, body = ImportBotResult), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn import_bot(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<ImportBotInput>,
) -> Result<Json<ImportBotResult>, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let device = device_of(peer.as_ref());
    let key = bot_key(&device, input.source_bot_id);
    let data = s.data_dir();
    let mapped = {
        let _g = REGISTRY_LOCK.lock();
        load(&data).bots.get(&key).copied()
    };
    if let Some(h) = mapped
        && s.bot_of_user(BotId(h)).is_ok()
    {
        return Ok(bot_result("exists", None, Some(h)));
    }
    let same_name: Vec<_> = s
        .storage()
        .list_bots(s.user_id())?
        .into_iter()
        .filter(|b| b.name == input.name)
        .collect();
    let host_bot = if let Some(b) = same_name.first() {
        // A bot with this name that this flow did not create is somebody else's: it may hold
        // other permissions. Only an explicit adoption binds to it.
        if !(input.adopt && same_name.len() == 1) {
            return Ok(bot_result("failed", Some("name_taken"), None));
        }
        audit(
            &s,
            &admin,
            "host.admin.import_bot",
            &serde_json::json!({"phase": "adopted"}),
        )?;
        (b.id.0, "adopted")
    } else {
        let mut config = input.config;
        normalise_computer(&mut config);
        let body = BotInput {
            expected_version: None,
            name: Some(input.name.clone()),
            config: Some(config),
            profile_toml: None,
            runtime: None,
        };
        // The ordinary creation path: every authority guard of `POST /v1/bots` applies.
        match crate::routes::create_bot(
            State(s.clone()),
            peer.clone(),
            HeaderMap::new(),
            ApiJson(body),
        )
        .await
        {
            Ok((_, Json(dto))) => (dto.id, "done"),
            Err(e) => return Ok(bot_result("failed", Some(&e.code_str()), None)),
        }
    };
    {
        let _g = REGISTRY_LOCK.lock();
        let mut reg = load(&data);
        reg.bots.insert(key, host_bot.0);
        save(&data, &reg)?;
    }
    Ok(bot_result(host_bot.1, None, Some(host_bot.0)))
}

/// `runtime.computer` values that meant "the sending desktop" (`pc`/`local`) or
/// one of ITS paired nodes. Once a bot lives on the host its computer is the
/// host's own (`auto`); the owner may choose "my computer" for it again later,
/// and that explicit choice is never rewritten.
fn foreign_computer(value: &str) -> bool {
    matches!(value, "pc" | "local")
        || value.starts_with("node:")
        || value.starts_with("peer:")
        || (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Set a moved bot's `runtime.computer` to `auto` when it names the sending
/// desktop. Returns the value that was replaced.
fn normalise_computer(config: &mut serde_json::Value) -> Option<String> {
    let slot = config.get_mut("runtime")?.get_mut("computer")?;
    let old = slot.as_str()?.to_owned();
    if !foreign_computer(&old) {
        return None;
    }
    *slot = serde_json::Value::String("auto".into());
    Some(old)
}

impl AppState {
    /// One-time, logged fix for bots that were moved to this host before the import mapped their
    /// computer: a stored `runtime.computer = "pc"` (the owner's desktop) becomes `auto` (this
    /// host's own computer). Runs once per installation (a flag in the import registry);
    /// returns how many bots changed.
    pub fn normalise_moved_bots(&self) -> usize {
        let data = self.data_dir();
        let _g = REGISTRY_LOCK.lock();
        let mut reg = load(&data);
        if reg.computer_normalised {
            return 0;
        }
        // Also with nothing imported yet: the flag is what makes this one-time, so a later
        // deliberate "my computer" choice for an imported bot is never rewritten.
        let mut changed = 0;
        for id in reg.bots.values().copied().collect::<Vec<_>>() {
            let mut from = None;
            let patched = self.storage().modify_bot(BotId(id), |name, stored| {
                let mut next = stored.clone();
                from = normalise_computer(&mut next);
                Ok((name.to_owned(), next))
            });
            let Some(old) = from else { continue };
            if let Ok(storage::BotPatch::Applied { bot, .. }) = patched {
                self.refresh_profile(&bot);
                changed += 1;
                let _ = self.storage().append_audit(
                    self.user_id(),
                    None,
                    "host",
                    "host.import.computer_normalised",
                    &serde_json::json!({"bot": id, "from": old, "to": "auto"}),
                );
                eprintln!(
                    "migration: moved bot {id} now runs on this host's own computer (was {old:?})"
                );
            }
        }
        reg.computer_normalised = true;
        let _ = save(&data, &reg);
        changed
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckBotItem {
    pub source_bot_id: Uuid,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckBotsInput {
    pub bots: Vec<CheckBotItem>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CheckBotResult {
    pub source_bot_id: Uuid,
    /// `mapped` (linked to a server bot already), `name_taken` (a bot with this name that this
    /// flow did not create exists: ask the owner) or `free`.
    pub state: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CheckBotsReport {
    pub items: Vec<CheckBotResult>,
}

/// What would happen to each bot, so the dialog can ask before anything is sent.
#[utoipa::path(post, operation_id = "admin_check_bots", path = "/v1/admin/import/bots/check", tag = "admin", security(("bearer" = [])),
    request_body = CheckBotsInput,
    responses((status = 200, body = CheckBotsReport), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn check_bots(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<CheckBotsInput>,
) -> Result<Json<CheckBotsReport>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    if input.bots.len() > 64 {
        return Err(ApiError::bad_request("too many bots"));
    }
    let device = device_of(peer.as_ref());
    let reg = {
        let _g = REGISTRY_LOCK.lock();
        load(&s.data_dir())
    };
    let names: HashSet<String> = s
        .storage()
        .list_bots(s.user_id())?
        .into_iter()
        .map(|b| b.name)
        .collect();
    let items = input
        .bots
        .into_iter()
        .map(|b| {
            let mapped = reg
                .bots
                .get(&bot_key(&device, b.source_bot_id))
                .is_some_and(|h| s.bot_of_user(BotId(*h)).is_ok());
            let state = if mapped {
                "mapped"
            } else if names.contains(&b.name) {
                "name_taken"
            } else {
                "free"
            };
            CheckBotResult {
                source_bot_id: b.source_bot_id,
                state: state.into(),
            }
        })
        .collect();
    Ok(Json(CheckBotsReport { items }))
}

// ------------------------------------------------------------------ host side: schedules

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportScheduleItem {
    /// Preserved: the same id on the host means "already moved".
    pub id: Uuid,
    /// The bot's id on the SENDING computer; the host maps it (see the module docs).
    pub source_bot_id: Uuid,
    /// A chat that was moved too (host id = source id); dropped unless bound to this bot.
    #[serde(default)]
    pub chat_id: Option<Uuid>,
    pub name: String,
    /// `{"cron": "0 9 * * 1-5 Europe/Istanbul"}`: the zone travels inside the spec.
    pub spec: serde_json::Value,
    pub payload: serde_json::Value,
    /// What the owner had; the schedule is imported DISABLED and activated later.
    pub enabled: bool,
    #[serde(default)]
    pub misfire_policy: Option<String>,
    #[serde(default)]
    pub precheck: Option<serde_json::Value>,
    /// File-watch or e-mail trigger; never a webhook (its secret stays behind).
    #[serde(default)]
    pub trigger: Option<serde_json::Value>,
    pub created_at_ms: i64,
    /// `user` or `bot`.
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportSchedulesInput {
    pub schedules: Vec<ImportScheduleItem>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportScheduleResult {
    pub id: Uuid,
    /// `done` (imported, disabled), `exists`, `active`, `paused`, `released` or `failed`.
    pub status: String,
    pub code: Option<String>,
    /// `staged`, `active` or `released`.
    pub stage: Option<String>,
    /// Next firing, recomputed on the host's clock (only once active).
    pub next_run_at_ms: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportSchedulesReport {
    pub items: Vec<ImportScheduleResult>,
}

fn result(
    id: Uuid,
    status: &str,
    code: Option<&str>,
    stage: Option<Stage>,
) -> ImportScheduleResult {
    ImportScheduleResult {
        id,
        status: status.into(),
        code: code.map(str::to_owned),
        stage: stage.map(|s| {
            match s {
                Stage::Staged => "staged",
                Stage::Active => "active",
                Stage::Released => "released",
            }
            .to_owned()
        }),
        next_run_at_ms: None,
    }
}

#[utoipa::path(post, operation_id = "admin_import_schedules", path = "/v1/admin/import/schedules", tag = "admin", security(("bearer" = [])),
    request_body = ImportSchedulesInput,
    responses((status = 200, body = ImportSchedulesReport), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn import_schedules(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<ImportSchedulesInput>,
) -> Result<Json<ImportSchedulesReport>, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    if input.schedules.len() > MAX_SCHEDULES {
        return Err(ApiError::bad_request("too many schedules in one call"));
    }
    let device = device_of(peer.as_ref());
    let data = s.data_dir();
    let store = s.schedule_store()?;
    let mut items = Vec::new();
    for item in input.schedules {
        let id = TaskId(item.id);
        let reg = {
            let _g = REGISTRY_LOCK.lock();
            load(&data)
        };
        if store.get(id)?.is_some() {
            items.push(match reg.schedules.get(&item.id.to_string()) {
                Some(b) if b.device == device => result(item.id, "exists", None, Some(b.stage)),
                _ => result(item.id, "failed", Some("id_in_use"), None),
            });
            continue;
        }
        // The host bot comes from the mapping this flow made, never from a name.
        let Some(host_bot) = reg.bots.get(&bot_key(&device, item.source_bot_id)).copied() else {
            items.push(result(item.id, "failed", Some("bot_not_moved"), None));
            continue;
        };
        if s.bot_of_user(BotId(host_bot)).is_err() {
            items.push(result(item.id, "failed", Some("bot_not_moved"), None));
            continue;
        }
        let chat_ok = |c: &Uuid| {
            reg.chats
                .get(&c.to_string())
                .is_some_and(|b| b.device == device && b.host_bot == host_bot)
                && s.chat_of_user(ChatId(*c)).is_ok()
        };
        let origin = item
            .origin
            .as_deref()
            .and_then(scheduler::ScheduleOrigin::parse)
            .unwrap_or_default();
        let wire = ScheduleInput {
            bot_id: host_bot,
            chat_id: item.chat_id.filter(chat_ok),
            name: item.name,
            spec: item.spec,
            payload: item.payload,
            // Stage 1: never enabled here until the sending computer froze its copy.
            enabled: Some(false),
            misfire_policy: item.misfire_policy,
            webhook_secret: None,
            trigger: item.trigger,
            precheck: item.precheck,
        };
        match crate::routes::import_schedule(&s, &wire, id, item.created_at_ms, origin).await {
            Ok(_) => {
                let _g = REGISTRY_LOCK.lock();
                let mut reg = load(&data);
                reg.schedules.insert(
                    item.id.to_string(),
                    SchedBind {
                        device: device.clone(),
                        stage: Stage::Staged,
                        want_enabled: item.enabled,
                    },
                );
                save(&data, &reg)?;
                items.push(result(item.id, "done", None, Some(Stage::Staged)));
            }
            Err(e) => items.push(result(item.id, "failed", Some(&e.code_str()), None)),
        }
    }
    audit(
        &s,
        &admin,
        "host.admin.import_schedules",
        &serde_json::json!({
            "done": items.iter().filter(|i| i.status == "done").count(),
            "exists": items.iter().filter(|i| i.status == "exists").count(),
            "failed": items.iter().filter(|i| i.status == "failed").count(),
        }),
    )?;
    Ok(Json(ImportSchedulesReport { items }))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleIdsInput {
    pub ids: Vec<Uuid>,
}

/// Move imported schedules of this device between stages. `activate`: staged -> active (and
/// enabled when the owner had it enabled); `release`: any -> released (disabled; the owner
/// brought it back to the other computer).
fn restage(
    s: &AppState,
    peer: Option<&Extension<PeerIdentity>>,
    input: ScheduleIdsInput,
    activate: bool,
) -> Result<Json<ImportSchedulesReport>, ApiError> {
    let admin = require_admin(s, peer.map(|p| &p.0))?;
    if input.ids.len() > MAX_SCHEDULES * 4 {
        return Err(ApiError::bad_request("too many schedules in one call"));
    }
    let device = device_of(peer);
    let data = s.data_dir();
    let store = s.schedule_store()?;
    let mut items = Vec::new();
    for id in input.ids {
        let _g = REGISTRY_LOCK.lock();
        let mut reg = load(&data);
        let Some(bind) = reg
            .schedules
            .get_mut(&id.to_string())
            .filter(|b| b.device == device)
        else {
            items.push(result(id, "failed", Some("not_imported"), None));
            continue;
        };
        let outcome = if activate {
            match bind.stage {
                Stage::Released => result(id, "released", None, Some(Stage::Released)),
                Stage::Active => result(id, "active", None, Some(Stage::Active)),
                Stage::Staged => {
                    if bind.want_enabled {
                        store.set_enabled(TaskId(id), true, None)?;
                    }
                    bind.stage = Stage::Active;
                    let mut r = result(
                        id,
                        if bind.want_enabled {
                            "active"
                        } else {
                            "paused"
                        },
                        None,
                        Some(Stage::Active),
                    );
                    r.next_run_at_ms = store.get(TaskId(id))?.and_then(|sc| sc.next_run_at);
                    r
                }
            }
        } else {
            store.set_enabled(TaskId(id), false, None)?;
            bind.stage = Stage::Released;
            result(id, "released", None, Some(Stage::Released))
        };
        save(&data, &reg)?;
        s.publish_schedule_changed(id, crate::dto::ScheduleChangeKind::Toggled);
        items.push(outcome);
    }
    audit(
        s,
        &admin,
        if activate {
            "host.admin.activate_schedules"
        } else {
            "host.admin.release_schedules"
        },
        &serde_json::json!({"count": items.len()}),
    )?;
    Ok(Json(ImportSchedulesReport { items }))
}

#[utoipa::path(post, operation_id = "admin_activate_schedules", path = "/v1/admin/import/schedules/activate", tag = "admin", security(("bearer" = [])),
    request_body = ScheduleIdsInput,
    responses((status = 200, body = ImportSchedulesReport), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn activate_schedules(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<ScheduleIdsInput>,
) -> Result<Json<ImportSchedulesReport>, ApiError> {
    restage(&s, peer.as_ref(), input, true)
}

#[utoipa::path(post, operation_id = "admin_release_schedules", path = "/v1/admin/import/schedules/release", tag = "admin", security(("bearer" = [])),
    request_body = ScheduleIdsInput,
    responses((status = 200, body = ImportSchedulesReport), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn release_schedules(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<ScheduleIdsInput>,
) -> Result<Json<ImportSchedulesReport>, ApiError> {
    restage(&s, peer.as_ref(), input, false)
}

// ------------------------------------------------------------------ host side: chats

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportChat {
    pub id: Uuid,
    /// The bot's id on the SENDING computer.
    pub source_bot_id: Uuid,
    pub title: String,
    pub created_at_ms: i64,
}

/// Run metadata only: no error text, no spawn parameters, no computer identity.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportRun {
    pub id: Uuid,
    pub source_bot_id: Uuid,
    pub chat_id: Option<Uuid>,
    pub kind: String,
    /// `succeeded`, `failed` or `cancelled`; anything else arrives as `cancelled`.
    pub state: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportMessage {
    pub id: Uuid,
    pub chat_id: Uuid,
    #[serde(default)]
    pub run_id: Option<Uuid>,
    /// The author: `user`, `assistant`, `system`, `tool`, ...
    pub role: String,
    pub content: String,
    pub created_at_ms: i64,
    #[serde(default)]
    pub exposure_untrusted: bool,
    #[serde(default)]
    pub exposure_private: bool,
    #[serde(default)]
    pub exposure_sources: Vec<String>,
    /// For a reply of a bot that did NOT move: its name. The reply stays foreign, quoted data.
    #[serde(default)]
    pub foreign_author: Option<String>,
}

/// One page of a chat move. Chats first, then runs, then messages: send the pages in that order.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportChatsInput {
    #[serde(default)]
    pub chats: Vec<ImportChat>,
    #[serde(default)]
    pub runs: Vec<ImportRun>,
    #[serde(default)]
    pub messages: Vec<ImportMessage>,
}

/// What a page did. Counts only.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct ImportChatsReport {
    pub chats: u32,
    pub runs: u32,
    pub messages: u32,
    /// Items the host already had from this device (an earlier or interrupted sync).
    pub existing: u32,
    /// Items refused: unmapped bot, an id that belongs to something else, malformed, too long.
    pub rejected: u32,
}

fn author_label(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .take(40)
        .collect();
    if name.trim().is_empty() {
        "another bot".into()
    } else {
        name
    }
}

#[allow(clippy::too_many_lines)] // three short insert loops over one report
fn process_chats(
    s: &AppState,
    device: &str,
    input: ImportChatsInput,
) -> Result<ImportChatsReport, ApiError> {
    let storage = s.storage();
    let user = s.user_id();
    let data = s.data_dir();
    let _g = REGISTRY_LOCK.lock();
    let mut reg = load(&data);
    let mut report = ImportChatsReport::default();
    let host_bot = |reg: &Registry, source: Uuid| -> Option<Uuid> {
        reg.bots
            .get(&bot_key(device, source))
            .copied()
            .filter(|b| s.bot_of_user(BotId(*b)).is_ok())
    };
    for c in input.chats {
        let Some(hb) = host_bot(&reg, c.source_bot_id) else {
            report.rejected += 1;
            continue;
        };
        if !plain(&c.title, 512) {
            report.rejected += 1;
        } else if storage.get_chat(ChatId(c.id))?.is_some() {
            // Only a chat this device imported for this very bot is "existing"; any other
            // chat with this id is somebody else's and is never extended.
            if reg
                .chats
                .get(&c.id.to_string())
                .is_some_and(|b| b.device == device && b.host_bot == hb)
            {
                report.existing += 1;
            } else {
                report.rejected += 1;
            }
        } else {
            storage.insert_chat(&Chat {
                id: ChatId(c.id),
                user_id: user,
                bot_id: BotId(hb),
                title: c.title,
                created_at_ms: c.created_at_ms,
            })?;
            reg.chats.insert(
                c.id.to_string(),
                ChatBind {
                    device: device.to_owned(),
                    host_bot: hb,
                },
            );
            report.chats += 1;
        }
    }
    for r in input.runs {
        let bound = |reg: &Registry, chat: Uuid, hb: Uuid| {
            reg.chats
                .get(&chat.to_string())
                .is_some_and(|b| b.device == device && b.host_bot == hb)
        };
        let Some(hb) = host_bot(&reg, r.source_bot_id) else {
            report.rejected += 1;
            continue;
        };
        // A run belongs to a chat of its own bot that this device imported.
        let Some(chat) = r.chat_id.filter(|c| bound(&reg, *c, hb)) else {
            report.rejected += 1;
            continue;
        };
        if !plain(&r.kind, 64) {
            report.rejected += 1;
        } else if storage.get_run(RunId(r.id))?.is_some() {
            if reg.runs.get(&r.id.to_string()).is_some_and(|d| d == device) {
                report.existing += 1;
            } else {
                report.rejected += 1;
            }
        } else {
            let state = match r.state.as_str() {
                "succeeded" | "failed" | "cancelled" => r.state,
                _ => "cancelled".to_owned(),
            };
            storage.insert_run(&Run {
                id: RunId(r.id),
                user_id: user,
                bot_id: BotId(hb),
                chat_id: Some(ChatId(chat)),
                kind: r.kind,
                state,
                error: None,
                created_at_ms: r.created_at_ms,
                updated_at_ms: r.updated_at_ms,
                parent_run_id: None,
                spec: None,
                computer_target: None,
                computer_node: None,
                computer_node_name: None,
                paused_at_ms: None,
                pause_reason: None,
            })?;
            reg.runs.insert(r.id.to_string(), device.to_owned());
            report.runs += 1;
        }
    }
    for m in input.messages {
        let chat_bound = reg
            .chats
            .get(&m.chat_id.to_string())
            .filter(|b| b.device == device)
            .map(|b| b.host_bot);
        if chat_bound.is_none()
            || m.content.len() > MAX_CONTENT
            || !plain(&m.role, 32)
            || m.exposure_sources.len() > 16
            || m.exposure_sources.iter().any(|x| !plain(x, 256))
        {
            report.rejected += 1;
            continue;
        }
        if let Some(old) = storage.get_message(m.id)? {
            if old.chat_id.0 == m.chat_id {
                report.existing += 1;
            } else {
                report.rejected += 1;
            }
            continue;
        }
        // A run link only counts when this device imported that run into this very chat.
        let run_id = m
            .run_id
            .filter(|r| reg.runs.get(&r.to_string()).is_some_and(|d| d == device))
            .map(RunId)
            .filter(|r| {
                storage
                    .get_run(*r)
                    .ok()
                    .flatten()
                    .is_some_and(|run| run.chat_id == Some(ChatId(m.chat_id)))
            });
        // An assistant reply that is not attached to an imported run of a moved bot was written
        // by somebody who is not a bot here: it stays foreign, quoted data with its author's name.
        let foreign = m.role == "assistant" && (run_id.is_none() || m.foreign_author.is_some());
        let (run_id, source) = if foreign {
            (
                None,
                format!(
                    "{IMPORTED_FOREIGN}{}",
                    author_label(m.foreign_author.as_deref().unwrap_or_default())
                ),
            )
        } else {
            (run_id, IMPORTED.to_owned())
        };
        storage.insert_message(&Message {
            id: m.id,
            user_id: user,
            chat_id: ChatId(m.chat_id),
            run_id,
            role: m.role,
            // The host's own redactor, on top of the sender's.
            content: s.0.agent.redact_host_text(m.content),
            created_at_ms: m.created_at_ms,
            exposure_untrusted: m.exposure_untrusted,
            exposure_private: m.exposure_private,
            exposure_sources: m.exposure_sources,
        })?;
        // Never `owner`: an imported row is not input typed on this computer.
        storage.set_message_source(m.id, &source)?;
        report.messages += 1;
    }
    save(&data, &reg)?;
    Ok(report)
}

#[utoipa::path(post, operation_id = "admin_import_chats", path = "/v1/admin/import/chats", tag = "admin", security(("bearer" = [])),
    request_body = ImportChatsInput,
    responses((status = 200, body = ImportChatsReport), (status = 403, body = crate::error::ErrorBody)))]
pub(crate) async fn import_chats(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    ApiJson(input): ApiJson<ImportChatsInput>,
) -> Result<Json<ImportChatsReport>, ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    if input.chats.len() > MAX_CHATS
        || input.runs.len() > MAX_RUNS
        || input.messages.len() > MAX_MESSAGES
    {
        return Err(ApiError::bad_request("page too large"));
    }
    let report = process_chats(&s, &device_of(peer.as_ref()), input)?;
    audit(
        &s,
        &admin,
        "host.admin.import_chats",
        &serde_json::json!({"chats": report.chats, "runs": report.runs,
            "messages": report.messages, "existing": report.existing, "rejected": report.rejected}),
    )?;
    Ok(Json(report))
}

// ----------------------------------------------------------------- local side

const MOVED_FILE: &str = "moved-schedules.json";

/// Where a schedule is in its handoff, as this computer remembers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Handoff {
    /// The local copy is frozen; the host copy may still be staged (disabled). Resumable.
    Frozen,
    /// The host runs it.
    Active,
    /// The owner brought it back here: never synced again.
    Reclaimed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Moved {
    host_id: String,
    host_name: String,
    was_enabled: bool,
    handoff: Handoff,
}

fn read_moved(data: &Path) -> BTreeMap<String, Moved> {
    std::fs::read(data.join(MOVED_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_moved(data: &Path, moved: &BTreeMap<String, Moved>) -> std::io::Result<()> {
    write_atomic(&data.join(MOVED_FILE), &serde_json::to_vec(moved)?)
}

const CHOICES_FILE: &str = "bot-link-choices.json";

/// The owner's answer to "a bot with this name already exists on the server", remembered per
/// (server, bot): `separate` means "keep them separate". (A link answer lives in the host's
/// mapping.) The question is asked once.
fn choice_key(host: &str, bot: Uuid) -> String {
    format!("{host}/{bot}")
}

pub(crate) fn kept_separate(data: &Path, host: &str, bot: Uuid) -> bool {
    std::fs::read(data.join(CHOICES_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<BTreeMap<String, String>>(&b).ok())
        .is_some_and(|m| {
            m.get(&choice_key(host, bot))
                .is_some_and(|v| v == "separate")
        })
}

pub(crate) fn remember_separate(data: &Path, host: &str, bot: Uuid) -> std::io::Result<()> {
    let mut m: BTreeMap<String, String> = std::fs::read(data.join(CHOICES_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    m.insert(choice_key(host, bot), "separate".into());
    write_atomic(&data.join(CHOICES_FILE), &serde_json::to_vec(&m)?)
}

/// `POST` with a pause and a retry when the host's administration budget is used up.
async fn post(
    links: &dyn HostLinks,
    host: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    for attempt in 0..4 {
        let call = links
            .call(host, "POST", path, Some(body.clone()))
            .await
            .map_err(|_| "host_unreachable".to_owned())?;
        if (200..300).contains(&call.status) {
            return Ok(call.body);
        }
        let code = call.body["error"]["code"]
            .as_str()
            .unwrap_or("rejected")
            .to_owned();
        if code == "admin_rate_limit" && attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_secs(61)).await;
            continue;
        }
        return Err(code);
    }
    Err("admin_rate_limit".into())
}

fn counts(kind: &str, done: u32, existing: u32, skipped: u32, failed: u32) -> CopyItemResult {
    CopyItemResult {
        kind: kind.into(),
        id: "all".into(),
        name: "all".into(),
        status: if failed > 0 { "failed" } else { "done" }.into(),
        code: None,
        count: Some(done),
        existing: Some(existing),
        skipped: Some(skipped),
        failed: Some(failed),
    }
}

fn failed_item(kind: &str, code: &str, done: u32, existing: u32, skipped: u32) -> CopyItemResult {
    let mut r = counts(kind, done, existing, skipped, 1);
    r.code = Some(code.to_owned());
    r
}

/// Send the chats (with run metadata and messages) of the moved bots. Idempotent and
/// resumable: a host that already has an item counts it as existing, so an interrupted move
/// is finished by running it again. Returns the report line and the chats the host now has.
#[allow(clippy::too_many_lines)] // one ordered export: chats, runs, then message pages
pub(crate) async fn send_chats(
    s: &AppState,
    links: &dyn HostLinks,
    host: &str,
    moved: &HashSet<Uuid>,
) -> (CopyItemResult, HashSet<Uuid>) {
    let (mut done, mut existing, mut skipped) = (0u32, 0u32, 0u32);
    let mut chats_on_host = HashSet::new();
    let storage = s.storage();
    let all_runs = storage.list_runs(s.user_id()).unwrap_or_default();
    let Ok(chats) = storage.list_chats(s.user_id()) else {
        return (failed_item("chats", "unreadable", 0, 0, 0), chats_on_host);
    };
    let mut budget = MAX_MESSAGES_TOTAL;
    for chat in chats.into_iter().filter(|c| moved.contains(&c.bot_id.0)) {
        let runs: Vec<_> = all_runs
            .iter()
            .filter(|r| r.chat_id == Some(chat.id) && moved.contains(&r.bot_id.0))
            .collect();
        let mut pages = vec![ImportChatsInput {
            chats: vec![ImportChat {
                id: chat.id.0,
                source_bot_id: chat.bot_id.0,
                title: s.0.agent.redact_host_text(chat.title.clone()),
                created_at_ms: chat.created_at_ms,
            }],
            ..ImportChatsInput::default()
        }];
        for part in runs.chunks(MAX_RUNS) {
            pages.push(ImportChatsInput {
                runs: part
                    .iter()
                    .map(|r| ImportRun {
                        id: r.id.0,
                        source_bot_id: r.bot_id.0,
                        chat_id: Some(chat.id.0),
                        kind: r.kind.clone(),
                        state: r.state.clone(),
                        created_at_ms: r.created_at_ms,
                        updated_at_ms: r.updated_at_ms,
                    })
                    .collect(),
                ..ImportChatsInput::default()
            });
        }
        let run_ids: HashSet<Uuid> = runs.iter().map(|r| r.id.0).collect();
        let mut page = ImportChatsInput::default();
        let mut bytes = 0usize;
        for m in storage.list_messages(chat.id).unwrap_or_default() {
            if m.content.len() > MAX_CONTENT || budget == 0 {
                skipped += 1;
                continue;
            }
            budget -= 1;
            // Whose reply is it? A reply of a bot that did not move keeps its author's name.
            let foreign_author = (m.role == "assistant"
                && !m.run_id.is_some_and(|r| run_ids.contains(&r.0)))
            .then(|| {
                m.run_id
                    .and_then(|r| all_runs.iter().find(|x| x.id == r))
                    .and_then(|r| storage.get_bot(r.bot_id).ok().flatten())
                    .map(|b| b.name)
                    .unwrap_or_default()
            });
            let item = ImportMessage {
                id: m.id,
                chat_id: chat.id.0,
                run_id: m.run_id.map(|r| r.0).filter(|r| run_ids.contains(r)),
                role: m.role,
                content: s.0.agent.redact_host_text(m.content),
                created_at_ms: m.created_at_ms,
                exposure_untrusted: m.exposure_untrusted,
                exposure_private: m.exposure_private,
                exposure_sources: m.exposure_sources,
                foreign_author,
            };
            let size = item.content.len() + 400;
            if !page.messages.is_empty()
                && (bytes + size > PAGE_BYTES || page.messages.len() >= MAX_MESSAGES)
            {
                pages.push(std::mem::take(&mut page));
                bytes = 0;
            }
            bytes += size;
            page.messages.push(item);
        }
        if !page.messages.is_empty() {
            pages.push(page);
        }
        for page in pages {
            let Ok(body) = serde_json::to_value(&page) else {
                continue;
            };
            match post(links, host, "/v1/admin/import/chats", body).await {
                Ok(v) => {
                    let n = |k: &str| u32::try_from(v[k].as_u64().unwrap_or(0)).unwrap_or(0);
                    done += n("messages");
                    existing += n("existing");
                    skipped += n("rejected");
                }
                Err(code) => {
                    return (
                        failed_item("chats", &code, done, existing, skipped),
                        chats_on_host,
                    );
                }
            }
        }
        chats_on_host.insert(chat.id.0);
    }
    (counts("chats", done, existing, skipped, 0), chats_on_host)
}

/// Hand the schedules of the moved bots over in stages (see the module docs): import disabled,
/// freeze the local copy durably, activate. Nothing here can leave both enabled, and a step
/// that was interrupted is finished by the next sync.
#[allow(clippy::too_many_lines)] // one ordered pass per schedule: import, freeze, activate
pub(crate) async fn send_schedules(
    s: &AppState,
    links: &dyn HostLinks,
    host: &str,
    host_name: &str,
    moved_bots: &HashSet<Uuid>,
    chats_on_host: &HashSet<Uuid>,
) -> CopyItemResult {
    let Ok(store) = s.schedule_store() else {
        return failed_item("schedules", "unreadable", 0, 0, 0);
    };
    let Ok(all) = store.list() else {
        return failed_item("schedules", "unreadable", 0, 0, 0);
    };
    let data = s.data_dir();
    let mut moved = read_moved(&data);
    let (mut done, mut existing, mut skipped, mut failed) = (0u32, 0u32, 0u32, 0u32);
    let mut stop: Option<String> = None;
    for sc in all
        .into_iter()
        .filter(|sc| moved_bots.contains(&sc.bot_id.0))
        .take(MAX_SCHEDULES_TOTAL)
    {
        let key = sc.id.0.to_string();
        let entry = moved.get(&key).cloned();
        if matches!(&entry, Some(e) if e.handoff == Handoff::Reclaimed) {
            skipped += 1;
            continue;
        }
        if matches!(&entry, Some(e) if e.handoff == Handoff::Active) {
            existing += 1;
            continue;
        }
        let trigger = store.get_trigger(sc.id).ok().flatten();
        if matches!(trigger, Some(scheduler::Trigger::Webhook { .. })) {
            // The shared secret is not copied; the owner sets a new one on the server.
            skipped += 1;
            continue;
        }
        if stop.is_some() {
            failed += 1;
            continue;
        }
        let was_enabled = entry.as_ref().map_or(sc.enabled, |e| e.was_enabled);
        let item = ImportScheduleItem {
            id: sc.id.0,
            source_bot_id: sc.bot_id.0,
            chat_id: sc
                .chat_id
                .map(|c| c.0)
                .filter(|c| chats_on_host.contains(c)),
            name: sc.name.clone(),
            spec: serde_json::to_value(&sc.spec).unwrap_or_default(),
            payload: serde_json::to_value(&sc.payload).unwrap_or_default(),
            enabled: was_enabled,
            misfire_policy: Some(sc.misfire_policy.as_str().to_owned()),
            precheck: sc
                .precheck
                .as_ref()
                .and_then(|p| serde_json::to_value(p).ok()),
            trigger: trigger.and_then(|t| serde_json::to_value(t).ok()),
            created_at_ms: sc.created_at,
            origin: Some(sc.origin.as_str().to_owned()),
        };
        // Stage 1: the host imports it DISABLED.
        let first = match post(
            links,
            host,
            "/v1/admin/import/schedules",
            serde_json::json!({"schedules": [item]}),
        )
        .await
        {
            Ok(v) => v["items"][0].clone(),
            Err(code) => {
                stop = Some(code);
                failed += 1;
                continue;
            }
        };
        let status = first["status"].as_str().unwrap_or("failed");
        if !matches!(status, "done" | "exists") {
            failed += 1;
            stop = first["code"].as_str().map(str::to_owned).or(stop);
            continue;
        }
        if first["stage"] == "released" {
            // The owner reclaimed it on another occasion: leave both sides as they are.
            skipped += 1;
            continue;
        }
        // Stage 2: freeze the local copy DURABLY. The record first (so an interrupted run
        // resumes), then the database freeze; any error stops here and nothing is activated.
        moved.insert(
            key.clone(),
            Moved {
                host_id: host.to_owned(),
                host_name: host_name.to_owned(),
                was_enabled,
                handoff: Handoff::Frozen,
            },
        );
        if write_moved(&data, &moved).is_err()
            || !matches!(store.freeze(sc.id, host_name), Ok(true))
        {
            stop = Some("freeze_failed".into());
            failed += 1;
            continue;
        }
        s.publish_schedule_changed(sc.id.0, crate::dto::ScheduleChangeKind::Toggled);
        // Stage 3: activate on the host.
        if let Err(code) = post(
            links,
            host,
            "/v1/admin/import/schedules/activate",
            serde_json::json!({"ids": [sc.id.0]}),
        )
        .await
        {
            // Frozen here, still staged there: nothing runs; the next sync finishes it.
            stop = Some(code);
            failed += 1;
            continue;
        }
        if let Some(e) = moved.get_mut(&key) {
            e.handoff = Handoff::Active;
        }
        if write_moved(&data, &moved).is_err() {
            // Harmless: the next sync re-sends and the host answers "active".
            failed += 1;
            continue;
        }
        if status == "done" {
            done += 1;
        } else {
            existing += 1;
        }
    }
    let mut r = counts("schedules", done, existing, skipped, failed);
    r.code = stop;
    r
}

// --------------------------------------------------------------------- reclaim

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReclaimInput {
    /// The server cannot be reached: bring the schedule back anyway. The copy there may still
    /// be active and then both computers run it.
    #[serde(default)]
    pub confirm_host_unreachable: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReclaimDto {
    pub schedule_id: Uuid,
    /// The server confirmed that its copy is switched off.
    pub host_released: bool,
    /// The server was not asked or did not answer: its copy may still be active.
    pub host_copy_may_be_active: bool,
    /// The local copy runs again (it was enabled before the move).
    pub enabled: bool,
}

/// `POST /v1/schedules/{id}/reclaim`: "bring this schedule back to this computer". Owner key
/// only. The server's copy is switched off FIRST; when it cannot be reached the owner must
/// confirm explicitly and is told that the copy there may still be active.
#[utoipa::path(post, operation_id = "schedule_reclaim", path = "/v1/schedules/{id}/reclaim", tag = "schedules", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Schedule id")), request_body = ReclaimInput,
    responses((status = 200, body = ReclaimDto), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn reclaim(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    headers: HeaderMap,
    UrlPath(sid): UrlPath<Uuid>,
    ApiJson(input): ApiJson<ReclaimInput>,
) -> Result<Json<ReclaimDto>, ApiError> {
    crate::host_copy::local_only(peer.as_ref())?;
    s.require_owner(&headers)?;
    let store = s.schedule_store()?;
    let id = TaskId(sid);
    let sc = store
        .get(id)?
        .filter(|sc| s.bot_of_user(sc.bot_id).is_ok())
        .ok_or_else(|| ApiError::not_found("schedule"))?;
    if store.frozen_for(sc.id)?.is_none() {
        return Err(ApiError::conflict("this schedule did not move to a server"));
    }
    let data = s.data_dir();
    let mut moved = read_moved(&data);
    let key = sid.to_string();
    let entry = moved.get(&key).cloned();
    // Ask the server to switch its copy off. No record of the server (a lost file) is the same
    // as an unreachable one: the owner has to say so.
    let released = match &entry {
        Some(e) => match crate::host_copy::links(&s) {
            Ok(links) => post(
                &*links,
                &e.host_id,
                "/v1/admin/import/schedules/release",
                serde_json::json!({"ids": [sid]}),
            )
            .await
            .is_ok_and(|v| {
                matches!(v["items"][0]["status"].as_str(), Some("released"))
                    || v["items"][0]["code"] == "not_imported"
            }),
            Err(_) => false,
        },
        None => false,
    };
    if !released && !input.confirm_host_unreachable {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "host_unreachable",
            "the server could not be asked to switch its copy off; it may still be active. \
             Repeat with confirm_host_unreachable to bring the schedule back anyway",
        ));
    }
    let was_enabled = entry.as_ref().is_some_and(|e| e.was_enabled);
    let mut kept = entry.unwrap_or(Moved {
        host_id: String::new(),
        host_name: String::new(),
        was_enabled,
        handoff: Handoff::Reclaimed,
    });
    kept.handoff = Handoff::Reclaimed;
    moved.insert(key, kept);
    write_moved(&data, &moved).map_err(ApiError::internal)?;
    store.unfreeze(id)?;
    if was_enabled {
        store.set_enabled(id, true, None)?;
    }
    s.publish_schedule_changed(sid, crate::dto::ScheduleChangeKind::Toggled);
    crate::host_copy::audit(
        &s,
        "schedule.reclaim",
        &serde_json::json!({"schedule_id": sid.to_string(), "host_released": released}),
    )?;
    Ok(Json(ReclaimDto {
        schedule_id: sid,
        host_released: released,
        host_copy_may_be_active: !released,
        enabled: was_enabled,
    }))
}

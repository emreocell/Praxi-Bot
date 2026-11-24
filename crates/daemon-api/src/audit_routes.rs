//! Audit log viewer (`/v1/audit`, owner-only): a filtered, paged, redacted
//! read of the append-only `audit_log`, a redacted CSV/JSON export and an
//! integrity check. Reads only: nothing here can change the log.
//!
//! Every detail value is redacted before it leaves the daemon (secret-looking
//! text, values under secret-looking keys, and whatever the host's own
//! redactor knows). Detail text is data written by tools and bots: it is
//! only ever returned as JSON/CSV cells, never interpreted.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Query, State},
    http::header,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::AuditEntry;
use utoipa::{IntoParams, ToSchema};

use crate::{ApiError, AppState, error::ErrorBody, usage::csv_escape};

/// Largest page the viewer may ask for.
const MAX_PAGE: usize = 200;
/// Most rows one export contains (newest first); `truncated` tells when cut.
const MAX_EXPORT: usize = 10_000;

/// Kinds a user can filter by. `other` is everything not listed.
const KINDS: &[&str] = &[
    "approval",
    "permission",
    "takeover",
    "import",
    "update",
    "key_use",
    "other",
];

/// Plain category of an audit action (the UI shows a translated name for
/// the action itself and for the category).
#[must_use]
pub fn kind_of(action: &str) -> &'static str {
    let has = |parts: &[&str]| parts.iter().any(|p| action.contains(p));
    if action.starts_with("takeover") || action.starts_with("control") {
        "takeover"
    } else if has(&["import"]) {
        "import"
    } else if has(&["update", "install", "upgrade", "restore"])
        && !action.starts_with("bot.")
        && !action.starts_with("engine.")
    {
        "update"
    } else if has(&["approv", "grant", "decision", "question", "authorized"]) {
        "approval"
    } else if action.starts_with("composio.")
        || has(&["key_", "token_", "llm.call", "usage"])
        || action == "mcp.tool_called"
    {
        "key_use"
    } else if has(&[
        "bot.update",
        "bot.create",
        "bot.delete",
        "engine.",
        "trusted_folder",
        "settings",
        "host_link",
        "host.invite",
        "cli_enable",
        "revoke",
        "fill_secret",
        "policy",
    ]) {
        "permission"
    } else {
        "other"
    }
}

/// Filters shared by the list and the export.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    /// Only events at or after this time (Unix ms).
    pub since_ms: Option<i64>,
    /// Only events at or before this time (Unix ms).
    pub until_ms: Option<i64>,
    /// Only events by this actor (`user`, `bot`, `system`, ...).
    pub actor: Option<String>,
    /// Only this kind: approval, permission, takeover, import, update, `key_use`, other.
    pub kind: Option<String>,
    /// Only events of this bot (id).
    pub bot: Option<String>,
    /// Case-insensitive text search over action, actor and redacted details.
    pub q: Option<String>,
    /// Rows to skip (newest first).
    pub offset: Option<usize>,
    /// Page size, 1 to 200 (default 50).
    pub limit: Option<usize>,
    /// Export format: `csv` (default) or `json`.
    pub format: Option<String>,
}

/// One audit event, details redacted.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEventDto {
    /// Stable, increasing id.
    pub id: i64,
    /// When it happened (Unix ms).
    pub ts_ms: i64,
    /// Who acted.
    pub actor: String,
    /// Machine name of the action (for example `grant.always`).
    pub action: String,
    /// Plain category: approval, permission, takeover, import, update, `key_use`, other.
    pub kind: String,
    /// Run it belongs to, if any.
    pub run_id: Option<String>,
    /// Bot it belongs to, if known.
    pub bot_id: Option<String>,
    /// Name of that bot, if it still exists.
    pub bot_name: Option<String>,
    /// Details with secrets removed.
    pub detail: Value,
}

/// A page of events.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditPage {
    /// Newest first.
    pub items: Vec<AuditEventDto>,
    /// Events matching the filters.
    pub total: usize,
    /// Rows skipped.
    pub offset: usize,
    /// Page size used.
    pub limit: usize,
    /// Distinct actors in the time window (for the filter list).
    pub actors: Vec<String>,
}

/// Result of the integrity check.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditVerify {
    /// Whether the log is hash-chained. It is not: protection is the
    /// database's own append-only rules plus the gap check below.
    pub hash_chain: bool,
    /// Events stored.
    pub entries: i64,
    /// First event id.
    pub first_id: Option<i64>,
    /// Last event id.
    pub last_id: Option<i64>,
    /// Ids missing between first and last (rows removed behind the rules).
    pub missing: i64,
    /// Whether both append-only rules are in place.
    pub append_only_rules: bool,
    /// `ok` or `problem`.
    pub status: String,
}

fn secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "authorization",
        "api_key",
        "apikey",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|p| k.contains(p))
}

fn scrub(v: &Value, redact: &dyn Fn(&str) -> String) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(|x| scrub(x, redact)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| {
                    let nv = if secret_key(k) && !x.is_null() {
                        Value::String("[REDACTED]".into())
                    } else {
                        scrub(x, redact)
                    };
                    (k.clone(), nv)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

struct Matched {
    rows: Vec<AuditEventDto>,
    actors: Vec<String>,
}

fn matching(s: &AppState, q: &AuditQuery) -> Result<Matched, ApiError> {
    if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty())
        && !KINDS.contains(&k)
    {
        return Err(ApiError::bad_request(
            "kind must be approval, permission, takeover, import, update, key_use or other",
        ));
    }
    let st = s.storage();
    let user = s.user_id();
    let bots: HashMap<String, String> = st
        .list_bots(user)?
        .into_iter()
        .map(|b| (b.id.0.to_string(), b.name))
        .collect();
    let run_bot: HashMap<String, String> = st
        .list_runs(user)?
        .into_iter()
        .map(|r| (r.id.to_string(), r.bot_id.0.to_string()))
        .collect();
    let host_redactor = s.doctor_host().and_then(|h| h.redactor());
    let redact = |t: &str| {
        let t = memory_store::redact(t);
        match &host_redactor {
            Some(r) => r.redact(&t),
            None => t,
        }
    };
    let entries = st.list_audit_between(user, q.since_ms, q.until_ms)?;
    let mut actors: Vec<String> = entries.iter().map(|e| e.actor.clone()).collect();
    actors.sort();
    actors.dedup();
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_lowercase);
    let rows = entries
        .into_iter()
        .filter(|e| {
            q.actor
                .as_deref()
                .is_none_or(|a| a.is_empty() || e.actor == a)
                && q.kind
                    .as_deref()
                    .is_none_or(|k| k.is_empty() || kind_of(&e.action) == k)
        })
        .map(|e| to_dto(&e, &bots, &run_bot, &redact))
        .filter(|d| {
            q.bot
                .as_deref()
                .is_none_or(|b| b.is_empty() || d.bot_id.as_deref() == Some(b))
        })
        .filter(|d| {
            needle.as_deref().is_none_or(|n| {
                d.action.to_lowercase().contains(n)
                    || d.actor.to_lowercase().contains(n)
                    || d.detail.to_string().to_lowercase().contains(n)
            })
        })
        .collect();
    Ok(Matched { rows, actors })
}

fn to_dto(
    e: &AuditEntry,
    bots: &HashMap<String, String>,
    run_bot: &HashMap<String, String>,
    redact: &dyn Fn(&str) -> String,
) -> AuditEventDto {
    let run_id = e.run_id.map(|r| r.to_string());
    let bot_id = e
        .detail
        .get("bot_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| run_id.as_ref().and_then(|r| run_bot.get(r).cloned()));
    AuditEventDto {
        id: e.id,
        ts_ms: e.ts_ms,
        actor: redact(&e.actor),
        action: redact(&e.action),
        kind: kind_of(&e.action).to_owned(),
        run_id,
        bot_name: bot_id.as_ref().and_then(|b| bots.get(b).cloned()),
        bot_id,
        detail: scrub(&e.detail, redact),
    }
}

/// Audit events, newest first, with filters and paging (owner-only).
#[utoipa::path(get, operation_id = "audit_list", path = "/v1/audit", tag = "audit", security(("bearer" = [])),
    params(AuditQuery),
    responses((status = 200, body = AuditPage), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody)))]
pub(crate) async fn list(
    State(s): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<AuditPage>, ApiError> {
    let m = matching(&s, &q)?;
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let offset = q.offset.unwrap_or(0);
    let total = m.rows.len();
    let items = m.rows.into_iter().skip(offset).take(limit).collect();
    Ok(Json(AuditPage {
        items,
        total,
        offset,
        limit,
        actors: m.actors,
    }))
}

/// Redacted CSV or JSON export of the filtered events (owner-only).
#[utoipa::path(get, operation_id = "audit_export", path = "/v1/audit/export", tag = "audit", security(("bearer" = [])),
    params(AuditQuery),
    responses((status = 200, description = "Redacted events as CSV or JSON", content_type = "text/csv", body = String),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)))]
pub(crate) async fn export(
    State(s): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let json = match q.format.as_deref() {
        None | Some("" | "csv") => false,
        Some("json") => true,
        Some(_) => return Err(ApiError::bad_request("format must be csv or json")),
    };
    let mut rows = matching(&s, &q)?.rows;
    let truncated = rows.len() > MAX_EXPORT;
    rows.truncate(MAX_EXPORT);
    let (ctype, name, body) = if json {
        let body = serde_json::to_string_pretty(&serde_json::json!({
            "truncated": truncated,
            "events": rows,
        }))
        .map_err(|_| ApiError::internal("export formatting failed"))?;
        (
            "application/json; charset=utf-8",
            "attachment; filename=\"praxi-audit.json\"",
            body,
        )
    } else {
        let mut out = String::from("id,time_utc_ms,actor,kind,action,bot,run,details\n");
        for r in &rows {
            let cells = [
                r.id.to_string(),
                r.ts_ms.to_string(),
                r.actor.clone(),
                r.kind.clone(),
                r.action.clone(),
                r.bot_name
                    .clone()
                    .or_else(|| r.bot_id.clone())
                    .unwrap_or_default(),
                r.run_id.clone().unwrap_or_default(),
                r.detail.to_string(),
            ];
            let line: Vec<String> = cells.iter().map(|c| csv_escape(c)).collect();
            out.push_str(&line.join(","));
            out.push('\n');
        }
        (
            "text/csv; charset=utf-8",
            "attachment; filename=\"praxi-audit.csv\"",
            out,
        )
    };
    Ok((
        [
            (header::CONTENT_TYPE, ctype),
            (header::CONTENT_DISPOSITION, name),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    ))
}

/// Check the log's append-only protection and look for removed rows (owner-only).
#[utoipa::path(get, operation_id = "audit_verify", path = "/v1/audit/verify", tag = "audit", security(("bearer" = [])),
    responses((status = 200, body = AuditVerify), (status = 403, body = ErrorBody)))]
pub(crate) async fn verify(State(s): State<AppState>) -> Result<Json<AuditVerify>, ApiError> {
    let (entries, first_id, last_id, triggers) = s.storage().audit_integrity()?;
    let span = match (first_id, last_id) {
        (Some(a), Some(b)) => b - a + 1,
        _ => 0,
    };
    let missing = (span - entries).max(0);
    let append_only_rules = triggers == 2;
    Ok(Json(AuditVerify {
        hash_chain: false,
        entries,
        first_id,
        last_id,
        missing,
        append_only_rules,
        status: if missing == 0 && append_only_rules {
            "ok"
        } else {
            "problem"
        }
        .into(),
    }))
}

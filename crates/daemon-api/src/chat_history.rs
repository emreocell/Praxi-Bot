//! Owner-requested history reads. Export deliberately never loads tool payloads.

use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use protocol::{BotId, ChatId};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ErrorBody},
    routes::ApiJson,
};

const MAX_EXPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_EXPORT_MESSAGES: usize = 1000;
const MAX_EXPORT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXPORT_ROWS: usize = 2000;

/// Largest import body; the same bound as an export file.
pub(crate) const MAX_IMPORT_BYTES: usize = MAX_EXPORT_BYTES;
/// Ranked pages scanned for one request when redaction omits every hit.
const MAX_SCAN_PAGES: usize = 20;

#[derive(Deserialize, IntoParams)]
pub(crate) struct SearchQuery {
    /// Literal phrase, 1–200 characters. FTS operators are treated as text.
    q: String,
    bot_id: Option<Uuid>,
    chat_id: Option<Uuid>,
    /// Ranked offset, at most 10,000.
    offset: Option<u32>,
    /// Page size, 1–100 (default 30).
    limit: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct SearchHit {
    chat_id: Uuid,
    bot_id: Uuid,
    chat_title: String,
    message_id: Uuid,
    created_at_ms: i64,
    /// Plain text, at most 320 characters. Never HTML.
    snippet: String,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct SearchPage {
    results: Vec<SearchHit>,
    next_offset: Option<u32>,
}

/// Search live, non-private user/assistant text, ranked by SQLite FTS5 BM25.
#[utoipa::path(get, path = "/v1/chats/search", tag = "chats", security(("bearer" = [])),
    params(SearchQuery), responses((status = 200, body = SearchPage),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)))]
pub(crate) async fn search(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Result<Response, ApiError> {
    s.require_owner(&headers)?;
    let page = history_work(move || search_page(&s, &q)).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(page)).into_response())
}

fn search_page(s: &AppState, q: &SearchQuery) -> Result<SearchPage, ApiError> {
    s.agent().refresh_external_redactor();
    let text = q.q.trim();
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(30);
    if text.is_empty()
        || text.chars().count() > 200
        || offset > 10_000
        || !(1..=100).contains(&limit)
    {
        return Err(ApiError::bad_request(
            "search requires a 1–200 character phrase, offset <= 10000 and limit 1–100",
        ));
    }
    if let Some(bot) = q.bot_id {
        s.bot_of_user(BotId(bot))?;
    }
    if let Some(chat) = q.chat_id {
        s.chat_of_user(ChatId(chat))?;
    }
    // A page whose hits were all omitted by redaction must not come back empty
    // with a "more" cursor: keep reading forward (bounded) until something is
    // shown or the ranked list ends.
    let mut next = offset;
    let mut results = Vec::new();
    let mut next_offset = None;
    for _ in 0..MAX_SCAN_PAGES {
        let rows = s.storage().search_messages(
            s.user_id(),
            q.bot_id.map(BotId),
            q.chat_id.map(ChatId),
            text,
            next,
            limit,
        )?;
        let full = rows.len() == limit as usize;
        for (m, snippet) in rows {
            // Redact the WHOLE message before considering snippets. Otherwise a snippet
            // boundary could split a credential and evade known-value masking. Omit a
            // changed message entirely, so a query cannot match only its secret portion.
            if safe_text(s, &m.content) != m.content || m.content.contains("[REDACTED]") {
                continue;
            }
            // The snippet gets the same pass as the checked text: the folded-match
            // fallback can pick a window that differs from what was checked.
            if safe_text(s, &snippet) != snippet || snippet.contains("[REDACTED]") {
                continue;
            }
            let chat = s.chat_of_user(m.chat_id)?;
            results.push(SearchHit {
                chat_id: chat.id.0,
                bot_id: chat.bot_id.0,
                chat_title: safe_text(s, &chat.title),
                message_id: m.id,
                created_at_ms: m.created_at_ms,
                snippet: snippet.chars().take(320).collect(),
            });
        }
        next += limit;
        next_offset = (full && next <= 10_000).then_some(next);
        if !results.is_empty() || next_offset.is_none() {
            break;
        }
    }
    Ok(SearchPage {
        results,
        next_offset,
    })
}

#[derive(Deserialize, IntoParams)]
pub(crate) struct ExportQuery {
    /// `markdown` (default) or `json`.
    format: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct ExportMessage {
    id: Uuid,
    run_id: Option<Uuid>,
    role: String,
    created_at_ms: i64,
    content: String,
    /// Recorded origin (`owner`, `telegram`, `imported`, ...), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// Display names only: no paths, URLs, extracted text or bytes.
    attachments: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct ExportStep {
    run_id: Uuid,
    name: String,
    status: String,
    created_at_ms: i64,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct ExportApproval {
    run_id: Uuid,
    outcome: String,
    created_at_ms: i64,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct ChatExport {
    format_version: u32,
    chat_id: Uuid,
    title: String,
    created_at_ms: i64,
    messages: Vec<ExportMessage>,
    steps: Vec<ExportStep>,
    approvals: Vec<ExportApproval>,
}

#[derive(Deserialize)]
pub(crate) struct ImportQuery {
    /// Bot that will own the new chat.
    bot_id: Uuid,
}

/// A version-1 export file. The chat and message ids are ignored; a new chat
/// and new message ids are created on import.
#[derive(Deserialize, ToSchema)]
pub(crate) struct ChatImport {
    format_version: u32,
    #[serde(default)]
    title: String,
    messages: Vec<ImportMessage>,
    #[serde(default)]
    steps: Vec<ImportRow>,
    #[serde(default)]
    approvals: Vec<ImportRow>,
}

/// An imported message. Usable roles are checked by the handler; `id`,
/// `run_id`, `created_at_ms` and `attachments` from an export are ignored.
#[derive(Deserialize, ToSchema)]
pub(crate) struct ImportMessage {
    role: String,
    content: String,
}

/// A tool step or approval row. Kept only for its count; it is never stored
/// in the run or approval tables.
#[derive(Deserialize, ToSchema)]
pub(crate) struct ImportRow {}

#[derive(Serialize, ToSchema)]
pub(crate) struct ChatImportReport {
    chat_id: Uuid,
    title: String,
    messages: u32,
    steps_ignored: u32,
    approvals_ignored: u32,
}

/// Import one version-1 chat export as a new chat. Every message is stored as
/// untrusted history with origin `imported`; tool steps and approvals are
/// counted but never executed or written to their tables.
#[utoipa::path(post, operation_id = "import_chat", path = "/v1/chats/import", tag = "chats", security(("bearer" = [])),
    params(("bot_id" = Uuid, Query, description = "Bot that will own the new chat.")),
    request_body = ChatImport,
    responses((status = 200, body = ChatImportReport),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub(crate) async fn import(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ImportQuery>,
    ApiJson(doc): ApiJson<ChatImport>,
) -> Result<Json<ChatImportReport>, ApiError> {
    s.require_owner(&headers)?;
    let report = tokio::task::spawn_blocking(move || import_doc(&s, q.bot_id, doc))
        .await
        .map_err(ApiError::internal)??;
    Ok(Json(report))
}

fn validate_import(doc: &ChatImport) -> Result<(), ApiError> {
    if doc.format_version != 1 {
        return Err(ApiError::bad_request(
            "unsupported chat export format_version; this build imports version 1",
        ));
    }
    if doc.messages.len() > MAX_EXPORT_MESSAGES {
        return Err(ApiError::bad_request("chat export has too many messages"));
    }
    if doc.steps.len() > MAX_EXPORT_ROWS || doc.approvals.len() > MAX_EXPORT_ROWS {
        return Err(ApiError::bad_request(
            "chat export has too many steps or approvals",
        ));
    }
    let text_bytes = doc.messages.iter().try_fold(0usize, |total, message| {
        total
            .checked_add(message.content.len())
            .ok_or_else(|| ApiError::bad_request("chat export text is too large"))
    })?;
    if text_bytes > MAX_EXPORT_TEXT_BYTES {
        return Err(ApiError::bad_request("chat export text is too large"));
    }
    if let Some(message) = doc
        .messages
        .iter()
        .find(|message| !matches!(message.role.as_str(), "user" | "assistant"))
    {
        return Err(ApiError::bad_request(format!(
            "chat export contains unsupported role {:?}; only user and assistant are imported",
            message.role
        )));
    }
    Ok(())
}

fn import_doc(s: &AppState, bot_id: Uuid, doc: ChatImport) -> Result<ChatImportReport, ApiError> {
    s.agent().refresh_external_redactor();
    let bot = s.bot_of_user(BotId(bot_id))?;
    validate_import(&doc)?;

    let now = storage::now_ms();
    let title = safe_text(s, doc.title.trim());
    let title: String = if title.is_empty() {
        "Imported chat".to_owned()
    } else {
        title.chars().take(120).collect()
    };
    let chat = storage::Chat {
        id: ChatId::new(),
        user_id: s.user_id(),
        bot_id: bot.id,
        title: title.clone(),
        created_at_ms: now,
    };
    let message_count = doc.messages.len();
    let mut rows = Vec::with_capacity(message_count + 1);
    for (index, message) in doc.messages.into_iter().enumerate() {
        let (id, source) = (Uuid::now_v7(), import_source(&message.role));
        rows.push((
            storage::Message {
                id,
                user_id: s.user_id(),
                chat_id: chat.id,
                run_id: None,
                role: message.role,
                content: safe_text(s, &message.content),
                // Keep the file's order while never placing an imported row
                // after the import instant: [now-N, now-1].
                created_at_ms: now.saturating_sub(
                    i64::try_from(message_count.saturating_sub(index)).unwrap_or(i64::MAX),
                ),
                exposure_untrusted: true,
                exposure_private: false,
                exposure_sources: vec!["imported chat".to_owned()],
            },
            source,
        ));
    }
    if !doc.steps.is_empty() || !doc.approvals.is_empty() {
        let note = format!(
            "Imported chat note: {} tool step(s) and {} approval(s) were listed in the file but were not imported or executed.",
            doc.steps.len(),
            doc.approvals.len()
        );
        rows.push((
            storage::Message {
                id: Uuid::now_v7(),
                user_id: s.user_id(),
                chat_id: chat.id,
                run_id: None,
                role: agent_core::control_note::IMPORTED_ROLE.to_owned(),
                content: note,
                created_at_ms: now,
                exposure_untrusted: true,
                exposure_private: false,
                exposure_sources: vec!["imported chat".to_owned()],
            },
            "imported",
        ));
    }
    let messages = u32::try_from(message_count).unwrap_or(u32::MAX);
    let steps_ignored = u32::try_from(doc.steps.len()).unwrap_or(u32::MAX);
    let approvals_ignored = u32::try_from(doc.approvals.len()).unwrap_or(u32::MAX);
    s.storage().insert_chat_with_messages(&chat, &rows)?;
    Ok(ChatImportReport {
        chat_id: chat.id.0,
        title,
        messages,
        steps_ignored,
        approvals_ignored,
    })
}

/// Imported replies are quoted as a foreign author's untrusted data, never as our own.
fn import_source(role: &str) -> &'static str {
    if role == "assistant" {
        "imported_foreign:imported chat"
    } else {
        "imported"
    }
}

fn safe_text(s: &AppState, text: &str) -> String {
    // The configured refresher covers vault/provider/CLI credentials; the baseline
    // also masks common credential patterns when an embedder has no redactor.
    static BASELINE: std::sync::OnceLock<secret_store::Redactor> = std::sync::OnceLock::new();
    BASELINE
        .get_or_init(secret_store::Redactor::default)
        .redact(&s.agent().redact_external_text(text.to_owned()))
}

/// Explicitly export one chat. Private messages are represented by redacted stubs.
/// Refuses rather than silently truncating chats over 1,000 messages, 2 MiB of
/// message text, 2,000 steps/approvals, or a 4 MiB serialized file.
#[utoipa::path(get, path = "/v1/chats/{id}/export", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id"), ExportQuery),
    responses((status = 200, content((ChatExport = "application/json"), (String = "text/markdown"))),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub(crate) async fn export(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(cid): Path<Uuid>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    s.require_owner(&headers)?;
    history_work(move || export_response(&s, cid, &q)).await
}

fn export_response(s: &AppState, cid: Uuid, q: &ExportQuery) -> Result<Response, ApiError> {
    s.agent().refresh_external_redactor();
    let format = q.format.as_deref().unwrap_or("markdown");
    if !matches!(format, "markdown" | "json") {
        return Err(ApiError::bad_request("format must be markdown or json"));
    }
    let chat = s.chat_of_user(ChatId(cid))?;
    // Scan a little past the exportable limits so inert rows (for example the
    // imported-chat note) do not count as exported messages.
    let (messages, omitted) = s.storage().list_messages_bounded(
        chat.id,
        None,
        MAX_EXPORT_MESSAGES + 32,
        MAX_EXPORT_TEXT_BYTES + 4096,
    )?;
    let messages: Vec<_> = messages
        .into_iter()
        .filter(|m| m.user_id == s.user_id() && matches!(m.role.as_str(), "user" | "assistant"))
        .collect();
    let text_bytes = messages.iter().try_fold(0usize, |total, message| {
        total.checked_add(message.content.len())
    });
    let steps = s.storage().chat_step_summaries(s.user_id(), chat.id)?;
    let approvals = s.storage().chat_approval_summaries(s.user_id(), chat.id)?;
    if omitted
        || messages.len() > MAX_EXPORT_MESSAGES
        || text_bytes.is_none_or(|bytes| bytes > MAX_EXPORT_TEXT_BYTES)
        || steps.len() > MAX_EXPORT_ROWS
        || approvals.len() > MAX_EXPORT_ROWS
    {
        return Err(ApiError::bad_request("chat exceeds export limits"));
    }
    let mut doc = ChatExport {
        format_version: 1,
        chat_id: cid,
        title: safe_text(s, &chat.title),
        created_at_ms: chat.created_at_ms,
        messages: Vec::new(),
        steps: steps
            .into_iter()
            .map(|(run, name, status, ts)| ExportStep {
                run_id: run.0,
                name: safe_text(s, &name),
                status: safe_text(s, &status),
                created_at_ms: ts,
            })
            .collect(),
        approvals: approvals
            .into_iter()
            .map(|(run, outcome, ts)| ExportApproval {
                run_id: run.0,
                outcome,
                created_at_ms: ts,
            })
            .collect(),
    };
    for message in messages {
        doc.messages.push(export_message(s, chat.id, message));
    }
    let (body, mime, extension) = if format == "json" {
        (
            serde_json::to_string_pretty(&doc).map_err(ApiError::internal)?,
            "application/json",
            "json",
        )
    } else {
        (markdown(&doc), "text/markdown; charset=utf-8", "md")
    };
    if body.len() > MAX_EXPORT_BYTES {
        return Err(ApiError::bad_request("chat exceeds export limits"));
    }
    Ok((
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"chat-{cid}.{extension}\""),
            ),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
        body,
    )
        .into_response())
}

fn export_message(s: &AppState, chat: ChatId, m: storage::Message) -> ExportMessage {
    let source = s.storage().message_source(m.id);
    // A failed lookup is treated as imported: never interpret untrusted text
    // as local attachment metadata.
    let imported = source
        .as_ref()
        .map_or(true, |source| source.starts_with("imported"));
    let source = source.ok().filter(|source| !source.is_empty());
    let (text, ids) = if imported {
        (m.content.as_str(), Vec::new())
    } else {
        agent_core::attachments::split_marker(&m.content)
    };
    let attachments = if m.exposure_private || imported {
        Vec::new()
    } else {
        ids.into_iter()
            .take(agent_core::attachments::MAX_PER_MESSAGE)
            .filter_map(|id| agent_core::attachments::meta(s.storage().data_dir(), chat, id).ok())
            .map(|a| agent_core::attachments::clean_name(&safe_text(s, &a.name)))
            .collect()
    };
    ExportMessage {
        id: m.id,
        run_id: m.run_id.map(|r| r.0),
        role: m.role,
        created_at_ms: m.created_at_ms,
        content: if m.exposure_private {
            "[REDACTED]".into()
        } else {
            safe_text(s, text)
        },
        source,
        attachments,
    }
}

// Keep CPU/SQLite/file reads off the async executor and bound concurrent work.
async fn history_work<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static READERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = READERS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::unavailable("history reads busy; retry shortly"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(ApiError::internal)?
}

fn timestamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map_or_else(|| ms.to_string(), |t| t.to_rfc3339())
}

fn markdown(doc: &ChatExport) -> String {
    let mut out = format!(
        "# {}\n\nChat: {}\nCreated: {}\nFormat version: 1\n",
        doc.title.replace(['\r', '\n'], " "),
        doc.chat_id,
        timestamp(doc.created_at_ms)
    );
    for m in &doc.messages {
        let _ = write!(
            out,
            "\n## {} · {}\n\n{}\n",
            m.role,
            timestamp(m.created_at_ms),
            m.content
        );
        for name in &m.attachments {
            let _ = writeln!(out, "\nAttachment: {}", name.replace(['\r', '\n'], " "));
        }
    }
    if !doc.steps.is_empty() {
        out.push_str("\n## Steps\n\n");
    }
    for step in &doc.steps {
        let _ = writeln!(
            out,
            "- {} · {} · {} (run {})",
            timestamp(step.created_at_ms),
            step.name.replace(['\r', '\n'], " "),
            step.status.replace(['\r', '\n'], " "),
            step.run_id
        );
    }
    if !doc.approvals.is_empty() {
        out.push_str("\n## Approvals\n\n");
    }
    for a in &doc.approvals {
        let _ = writeln!(
            out,
            "- {} · {} (run {})",
            timestamp(a.created_at_ms),
            a.outcome,
            a.run_id
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_formats_match_documented_golden_files() {
        let doc = ChatExport {
            format_version: 1,
            chat_id: Uuid::from_u128(1),
            title: "Project notes".into(),
            created_at_ms: 0,
            messages: vec![ExportMessage {
                id: Uuid::from_u128(2),
                run_id: None,
                role: "user".into(),
                created_at_ms: 0,
                content: "Hello [REDACTED]".into(),
                source: Some("owner".into()),
                attachments: vec!["notes.txt".into()],
            }],
            steps: vec![ExportStep {
                run_id: Uuid::from_u128(3),
                name: "read_file".into(),
                status: "succeeded".into(),
                created_at_ms: 0,
            }],
            approvals: vec![ExportApproval {
                run_id: Uuid::from_u128(3),
                outcome: "denied".into(),
                created_at_ms: 1000,
            }],
        };
        assert_eq!(
            serde_json::to_string_pretty(&doc).unwrap(),
            include_str!("tests/fixtures/chat-export.json").trim_end()
        );
        assert_eq!(
            markdown(&doc),
            include_str!("tests/fixtures/chat-export.md")
        );
    }
}

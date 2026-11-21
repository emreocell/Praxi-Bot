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
};

const MAX_EXPORT_BYTES: usize = 4 * 1024 * 1024;

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
    let rows = s.storage().search_messages(
        s.user_id(),
        q.bot_id.map(BotId),
        q.chat_id.map(ChatId),
        text,
        offset,
        limit,
    )?;
    let full = rows.len() == limit as usize;
    let mut results = Vec::new();
    for (m, snippet) in rows {
        // Redact the WHOLE message before considering snippets. Otherwise a snippet
        // boundary could split a credential and evade known-value masking. Omit a
        // changed message entirely, so a query cannot match only its secret portion.
        if safe_text(s, &m.content) != m.content || m.content.contains("[REDACTED]") {
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
    Ok(SearchPage {
        results,
        next_offset: (full && offset + limit <= 10_000).then_some(offset + limit),
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
    let (messages, omitted) =
        s.storage()
            .list_messages_bounded(chat.id, None, 1000, 2 * 1024 * 1024)?;
    let steps = s.storage().chat_step_summaries(s.user_id(), chat.id)?;
    let approvals = s.storage().chat_approval_summaries(s.user_id(), chat.id)?;
    if omitted || steps.len() > 2000 || approvals.len() > 2000 {
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
    for m in messages {
        if m.user_id != s.user_id() || !matches!(m.role.as_str(), "user" | "assistant") {
            continue;
        }
        let (text, ids) = agent_core::attachments::split_marker(&m.content);
        let attachments = if m.exposure_private {
            Vec::new()
        } else {
            ids.into_iter()
                .take(agent_core::attachments::MAX_PER_MESSAGE)
                .filter_map(|id| {
                    agent_core::attachments::meta(s.storage().data_dir(), chat.id, id).ok()
                })
                .map(|a| agent_core::attachments::clean_name(&safe_text(s, &a.name)))
                .collect()
        };
        doc.messages.push(ExportMessage {
            id: m.id,
            run_id: m.run_id.map(|r| r.0),
            role: m.role,
            created_at_ms: m.created_at_ms,
            content: if m.exposure_private {
                "[REDACTED]".into()
            } else {
                safe_text(s, text)
            },
            attachments,
        });
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

// Keep CPU/SQLite/file reads off the async executor and bound concurrent work.
async fn history_work<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static READERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = READERS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
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

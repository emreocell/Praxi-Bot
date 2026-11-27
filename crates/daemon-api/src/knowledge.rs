//! User-owned knowledge source management. No tool can create a filesystem
//! grant, enable an imported source or grant embedding egress through this API.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use axum::http::StatusCode;
use axum::{
    Json,
    extract::{Path as ApiPath, State},
};
use cap_fs_ext::DirExt;
use memory_store::knowledge::source_adapters::{
    MAX_SOURCE_BYTES, MAX_TEXT_BYTES, SelectedSource, saved_web,
};
use memory_store::knowledge::{
    DocumentFormat, DocumentInput, KnowledgeStore, NewSource, Scope, Source, SourceKind,
    SourceUpdate,
};
use protocol::{BotId, ChatId, RunId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

type SourceWorkers = parking_lot::Mutex<
    std::collections::BTreeMap<std::path::PathBuf, std::sync::Weak<tokio::sync::Semaphore>>,
>;
static WORKERS: OnceLock<SourceWorkers> = OnceLock::new();

/// JSON may encode each decoded input byte as a six-byte Unicode escape.
/// Decoded snapshots remain limited to 4 MiB and indexed text to 1 MiB.
pub(crate) const MAX_SOURCE_REQUEST_BYTES: usize = MAX_SOURCE_BYTES * 6 + 64 * 1024;

#[cfg(test)]
pub(crate) static SOURCE_WORKER_PROBE: parking_lot::Mutex<
    Option<(BotId, std::sync::mpsc::Sender<std::thread::ThreadId>)>,
> = parking_lot::Mutex::new(None);

pub(crate) fn authority_lock(s: &AppState, bot: BotId) -> Arc<tokio::sync::Mutex<()>> {
    crate::knowledge_runtime::authority_lock(s, bot)
}

pub(crate) fn try_authority(
    s: &AppState,
    bot: BotId,
) -> Result<tokio::sync::OwnedMutexGuard<()>, ApiError> {
    authority_lock(s, bot).try_lock_owned().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "knowledge_busy",
            "a knowledge authority change is already running",
        )
    })
}

/// Source kinds accepted by user management; every kind remains untrusted data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceKind {
    File,
    Folder,
    SavedWeb,
    Conversation,
    Email,
    Note,
    SharedBrain,
}

impl From<KnowledgeSourceKind> for SourceKind {
    fn from(kind: KnowledgeSourceKind) -> Self {
        match kind {
            KnowledgeSourceKind::File => Self::File,
            KnowledgeSourceKind::Folder => Self::Folder,
            KnowledgeSourceKind::SavedWeb => Self::SavedWeb,
            KnowledgeSourceKind::Conversation => Self::Conversation,
            KnowledgeSourceKind::Email => Self::Email,
            KnowledgeSourceKind::Note => Self::Note,
            KnowledgeSourceKind::SharedBrain => Self::SharedBrain,
        }
    }
}

/// A user-selected source. Body fields cannot choose an owner, trust or consent.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSourceInput {
    pub kind: KnowledgeSourceKind,
    #[serde(default)]
    pub label: String,
    /// Absolute path on the daemon host, for a file or folder only.
    pub path: Option<String>,
    /// Saved text/HTML or a note; never interpreted as a source URL to fetch.
    /// At most 4 MiB decoded UTF-8 input and 1 MiB extracted/indexed text.
    /// The JSON envelope permits up to 24 MiB + 64 KiB for escaped input.
    pub content: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub html: bool,
    pub chat_id: Option<Uuid>,
    /// Email sources use a successful recorded `email_read` in this bot's run.
    pub run_id: Option<Uuid>,
    pub tool_call_id: Option<Uuid>,
    /// Must equal the explicit account in the recorded `email_read` arguments.
    pub account: Option<String>,
}

/// Source metadata; source bodies and credentials are never listed.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeSourceDto {
    pub id: Uuid,
    pub kind: KnowledgeSourceKind,
    pub label: String,
    pub locator: String,
    pub enabled: bool,
    pub requires_path_selection: bool,
    pub revision: u64,
    pub remote_profile_fingerprint: Option<String>,
    pub documents: usize,
    pub chunks: usize,
    pub updated_ms: i64,
}

/// A bounded indexing page. Replay starts from hashes already committed.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct KnowledgeIndexStatus {
    pub processed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub next_cursor: Option<String>,
    pub failures: Vec<KnowledgeFileFailure>,
    /// What the folder scan left out (secret-looking files, oversized files, ...).
    pub skipped: crate::knowledge_documents::KnowledgeSkipped,
}

/// A skipped file never prevents the page cursor from advancing.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeFileFailure {
    pub locator: String,
    pub code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeMutation {
    pub source: KnowledgeSourceDto,
    pub indexing: KnowledgeIndexStatus,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeEmbeddingStatus {
    Progress,
    Complete,
    AwaitingConsent,
    Deferred,
    Cancelled,
    Failed,
}

/// Progress of one bounded embedding batch. The queue resumes from hashes and
/// source revisions already committed; no provider response body is exposed.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeEmbeddingProgress {
    pub status: KnowledgeEmbeddingStatus,
    pub embedded_chunks: usize,
    pub cache_hits: usize,
    pub remaining_chunks: usize,
    pub skipped_chunks: usize,
    pub next_attempt_ms: Option<i64>,
    pub failure_code: Option<String>,
}

impl From<memory_store::knowledge::embedding::EmbeddingProgress> for KnowledgeEmbeddingProgress {
    fn from(value: memory_store::knowledge::embedding::EmbeddingProgress) -> Self {
        use memory_store::knowledge::embedding::EmbeddingStatus;
        let status = match value.status {
            EmbeddingStatus::Progress => KnowledgeEmbeddingStatus::Progress,
            EmbeddingStatus::Complete => KnowledgeEmbeddingStatus::Complete,
            EmbeddingStatus::AwaitingConsent => KnowledgeEmbeddingStatus::AwaitingConsent,
            EmbeddingStatus::Deferred => KnowledgeEmbeddingStatus::Deferred,
            EmbeddingStatus::Cancelled => KnowledgeEmbeddingStatus::Cancelled,
            EmbeddingStatus::Failed => KnowledgeEmbeddingStatus::Failed,
        };
        Self {
            status,
            embedded_chunks: value.embedded_chunks,
            cache_hits: value.cache_hits,
            remaining_chunks: value.remaining_chunks,
            skipped_chunks: value.skipped_chunks,
            next_attempt_ms: value.next_attempt_ms,
            failure_code: value.failure_code,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeEmailCapture {
    pub run_id: Uuid,
    pub tool_call_id: Uuid,
    pub account: String,
    pub created_ms: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRevision {
    pub expected_revision: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEmbed {
    pub expected_revision: u64,
    /// True only for an explicit user retry. Continuation batches keep retries,
    /// provider cooldowns and the shared quota ledger intact.
    #[serde(default)]
    pub reset_retry: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeReindex {
    pub expected_revision: u64,
    /// Last relative file from the previous page; omitted starts a new scan.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceAction {
    Enable,
    Disable,
    RevokeRemote,
    Rename,
    Rebind,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSourceChange {
    pub expected_revision: u64,
    pub action: KnowledgeSourceAction,
    pub label: Option<String>,
    /// An owner-selected replacement path, required to rebind imported files.
    pub path: Option<String>,
}

pub(crate) fn err(error: memory_store::MemoryError) -> ApiError {
    match error {
        memory_store::MemoryError::NotFound(message) => ApiError::not_found(message),
        memory_store::MemoryError::Invalid(message) => ApiError::conflict(message),
        _ => ApiError::internal("knowledge operation failed"),
    }
}

pub(crate) fn scope(s: &AppState, bot: Uuid) -> Result<Scope, ApiError> {
    let bot = s.bot_of_user(BotId(bot))?;
    Ok(Scope {
        user_id: bot.user_id,
        bot_id: bot.id,
    })
}

fn store(s: &AppState, owner: Scope) -> Result<KnowledgeStore, ApiError> {
    KnowledgeStore::open(s.data_dir(), owner).map_err(err)
}

fn required(value: Option<&str>, name: &str) -> Result<String, ApiError> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request(format!("{name} is required")))
}

fn source_dto(
    s: &AppState,
    store: &KnowledgeStore,
    source: Source,
) -> Result<KnowledgeSourceDto, ApiError> {
    let docs = store.list_documents(source.id).map_err(err)?;
    let kind = match source.kind {
        SourceKind::File => KnowledgeSourceKind::File,
        SourceKind::Folder => KnowledgeSourceKind::Folder,
        SourceKind::SavedWeb => KnowledgeSourceKind::SavedWeb,
        SourceKind::Conversation => KnowledgeSourceKind::Conversation,
        SourceKind::Email => KnowledgeSourceKind::Email,
        SourceKind::Note => KnowledgeSourceKind::Note,
        SourceKind::SharedBrain => KnowledgeSourceKind::SharedBrain,
    };
    let remote_profile_fingerprint =
        crate::knowledge_settings::remote_fingerprint(s, store.scope().bot_id.0)
            .ok()
            .filter(|fingerprint| {
                crate::knowledge_runtime::authorized(
                    s,
                    store.scope().bot_id,
                    &crate::knowledge_runtime::EmbeddingGrantTarget::Documents {
                        source_id: source.id,
                    },
                    fingerprint,
                )
                .is_some()
            });
    Ok(KnowledgeSourceDto {
        id: source.id,
        kind,
        label: source.label,
        enabled: source.enabled,
        requires_path_selection: matches!(source.kind, SourceKind::File | SourceKind::Folder)
            && source.locator == "imported:requires-path-selection",
        locator: source.locator,
        revision: source.revision,
        remote_profile_fingerprint,
        documents: docs.len(),
        chunks: docs.iter().map(|d| d.chunks).sum(),
        updated_ms: source.updated_ms,
    })
}

fn existing(store: &KnowledgeStore, id: Uuid, revision: u64) -> Result<Source, ApiError> {
    let source = store
        .source(id)
        .map_err(err)?
        .ok_or_else(|| ApiError::not_found("knowledge source"))?;
    if source.revision != revision {
        return Err(ApiError::conflict(
            "source changed; reload its current revision",
        ));
    }
    Ok(source)
}

fn bounded_text(text: &str) -> Result<String, ApiError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(ApiError::bad_request(
            "source snapshot exceeds text byte limit",
        ));
    }
    Ok(memory_store::knowledge::source_adapters::redact_source(
        text,
    ))
}

fn safe_metadata(s: &AppState, text: &str) -> Result<(), ApiError> {
    if s.agent().redact_external_text(text.to_owned()) != text {
        return Err(ApiError::bad_request(
            "source label or locator contains a secret",
        ));
    }
    Ok(())
}

fn redact_document(s: &AppState, doc: &mut DocumentInput) -> Result<(), ApiError> {
    safe_metadata(s, &doc.locator)?;
    doc.content = s
        .agent()
        .redact_external_text(std::mem::take(&mut doc.content));
    doc.title = s
        .agent()
        .redact_external_text(std::mem::take(&mut doc.title));
    Ok(())
}

fn conversation(s: &AppState, owner: Scope, id: Uuid) -> Result<DocumentInput, ApiError> {
    let chat = s.chat_of_user(ChatId(id))?;
    if chat.bot_id != owner.bot_id {
        return Err(ApiError::not_found("conversation for this bot"));
    }
    let mut content = String::new();
    let (messages, omitted) =
        s.storage()
            .list_messages_bounded(chat.id, None, 2000, MAX_TEXT_BYTES)?;
    if omitted {
        return Err(ApiError::bad_request(
            "conversation exceeds snapshot limit; select a smaller conversation",
        ));
    }
    for message in messages {
        if message.user_id != owner.user_id
            || !matches!(message.role.as_str(), "user" | "assistant")
        {
            continue;
        }
        if content
            .len()
            .saturating_add(message.content.len())
            .saturating_add(128)
            > MAX_TEXT_BYTES
        {
            return Err(ApiError::bad_request(
                "conversation exceeds snapshot limit; select a smaller conversation",
            ));
        }
        let _ = write!(
            content,
            "\n## {} · {}\n\n{}\n",
            message.role, message.created_at_ms, message.content
        );
    }
    Ok(DocumentInput {
        locator: format!("chat:{id}"),
        title: chat.title,
        content: bounded_text(&content)?,
        format: DocumentFormat::Markdown,
        mtime_ms: None,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct EmailLocator {
    account: String,
    run_id: Uuid,
    call_id: Uuid,
}

fn email(s: &AppState, owner: Scope, locator: &EmailLocator) -> Result<DocumentInput, ApiError> {
    let bot = owner.bot_id.to_string();
    let allowed = s.email().is_some_and(|host| {
        host.list().iter().any(|account| {
            account.name == locator.account && account.bot.as_ref().is_none_or(|b| b == &bot)
        })
    });
    if !allowed {
        return Err(ApiError::not_found("email account for this bot"));
    }
    let run = s.run_of_user(RunId(locator.run_id))?;
    if run.bot_id != owner.bot_id {
        return Err(ApiError::not_found("email run for this bot"));
    }
    for step in s.storage().knowledge_tool_steps(
        owner.user_id,
        owner.bot_id,
        Some(run.id),
        200,
        MAX_TEXT_BYTES * 4,
    )? {
        if step.name != "tools" || step.user_id != owner.user_id {
            continue;
        }
        let Some(results) = step.output.as_ref().and_then(|v| v["results"].as_array()) else {
            continue;
        };
        for result in results {
            if result["call_uuid"].as_str() != Some(locator.call_id.to_string().as_str())
                || result["name"] != "email_read"
                || result["is_error"] != false
                || result["spill_path"].is_string()
            {
                continue;
            }
            let Some(call) = step.input["calls"].as_array().and_then(|calls| {
                calls
                    .iter()
                    .find(|call| call["id"] == result["id"] && call["name"] == "email_read")
            }) else {
                continue;
            };
            let args: Value = serde_json::from_str(call["arguments"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null);
            if args["account"].as_str() != Some(&locator.account) {
                continue;
            }
            let content = bounded_text(
                result["text"]
                    .as_str()
                    .ok_or_else(|| ApiError::bad_request("email result has no text"))?,
            )?;
            return Ok(DocumentInput {
                locator: format!("email:{}:{}", locator.run_id, locator.call_id),
                title: format!("Email snapshot · {}", locator.account),
                content,
                format: DocumentFormat::PlainText,
                mtime_ms: step.finished_at_ms,
            });
        }
    }
    Err(ApiError::not_found(
        "successful explicit-account email_read result",
    ))
}

fn authorize_source(s: &AppState, owner: Scope, source: &Source) -> Result<(), ApiError> {
    #[cfg(test)]
    if let Some((bot, sender)) = &*SOURCE_WORKER_PROBE.lock()
        && *bot == owner.bot_id
    {
        let _ = sender.send(std::thread::current().id());
    }
    match source.kind {
        SourceKind::File | SourceKind::Folder | SourceKind::SharedBrain => {
            if matches!(source.kind, SourceKind::File | SourceKind::Folder)
                && source.locator == "imported:requires-path-selection"
            {
                return Err(ApiError::conflict(
                    "select a local file or folder path again before enabling this imported source",
                ));
            }
            let expected = s.data_dir().join("brain").display().to_string();
            if source.kind == SourceKind::SharedBrain && source.locator != expected {
                return Err(ApiError::conflict(
                    "imported brain path needs a new source binding",
                ));
            }
            SelectedSource::open_on_host(
                Path::new(&source.locator),
                source.kind != SourceKind::File,
                &s.data_dir(),
                source.kind == SourceKind::SharedBrain,
            )
            .map_err(err)?;
        }
        SourceKind::Conversation => {
            conversation(s, owner, parse_chat(&source.locator)?)?;
        }
        SourceKind::Email => {
            email(
                s,
                owner,
                &serde_json::from_str(&source.locator)
                    .map_err(|_| ApiError::bad_request("invalid email locator"))?,
            )?;
        }
        SourceKind::SavedWeb | SourceKind::Note => {}
    }
    Ok(())
}

fn parse_chat(locator: &str) -> Result<Uuid, ApiError> {
    locator
        .strip_prefix("chat:")
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| ApiError::bad_request("invalid conversation locator"))
}

#[allow(clippy::too_many_lines)] // One scan transaction: walk, index, record failures, prune.
fn reindex_inner(
    s: &AppState,
    owner: Scope,
    source: &Source,
    cursor: Option<&str>,
) -> Result<KnowledgeMutation, ApiError> {
    reindex_limited(s, owner, source, cursor, 32)
}

#[allow(clippy::too_many_lines)] // One scan transaction: walk, index, record failures, prune.
pub(crate) fn reindex_limited(
    s: &AppState,
    owner: Scope,
    source: &Source,
    cursor: Option<&str>,
    max_files: usize,
) -> Result<KnowledgeMutation, ApiError> {
    if !source.enabled {
        return Err(ApiError::conflict(
            "source is disabled; explicitly reauthorize it first",
        ));
    }
    let store = store(s, owner)?;
    let mut revision = source.revision;
    let mut status = KnowledgeIndexStatus::default();
    match source.kind {
        SourceKind::File | SourceKind::Folder | SourceKind::SharedBrain => {
            authorize_source(s, owner, source)?;
            let selected = SelectedSource::open_on_host(
                Path::new(&source.locator),
                source.kind != SourceKind::File,
                &s.data_dir(),
                source.kind == SourceKind::SharedBrain,
            )
            .map_err(err)?;
            let page: Vec<_> = selected
                .files()
                .iter()
                .filter(|name| cursor.is_none_or(|last| name.as_str() > last))
                .take(max_files.clamp(1, 32))
                .collect();
            for name in &page {
                let failed = |code: &str| KnowledgeFileFailure {
                    locator: s.agent().redact_external_text((*name).clone()),
                    code: code.to_owned(),
                };
                let mut doc = match selected.read(name) {
                    Ok(doc) => doc,
                    Err(error) if source.kind == SourceKind::File => return Err(err(error)),
                    Err(error) => {
                        status.failures.push(failed(
                            if memory_store::knowledge::source_adapters::is_private_key_refusal(
                                &error,
                            ) {
                                "secret_content"
                            } else {
                                "unreadable_source"
                            },
                        ));
                        continue;
                    }
                };
                if let Err(error) = redact_document(s, &mut doc) {
                    if source.kind == SourceKind::File {
                        return Err(error);
                    }
                    status.failures.push(failed("unsafe_metadata"));
                    continue;
                }
                let outcome = match store.index_document(source.id, revision, &doc) {
                    Ok(outcome) => outcome,
                    Err(error @ memory_store::MemoryError::Invalid(_))
                        if source.kind != SourceKind::File =>
                    {
                        let current = store
                            .source(source.id)
                            .map_err(err)?
                            .ok_or_else(|| ApiError::not_found("source removed while indexing"))?;
                        if current.revision != revision {
                            return Err(err(error));
                        }
                        status.failures.push(failed("index_rejected"));
                        continue;
                    }
                    Err(error) => return Err(err(error)),
                };
                revision = outcome.source_revision;
                status.processed += 1;
                status.unchanged += usize::from(outcome.unchanged);
            }
            status.skipped = selected.skipped().into();
            status.next_cursor = page
                .last()
                .filter(|last| {
                    selected
                        .files()
                        .last()
                        .is_some_and(|end| end.as_str() > last.as_str())
                })
                .map(|last| (*last).clone());
            if status.next_cursor.is_none() {
                for doc in store.list_documents(source.id).map_err(err)? {
                    let locator = doc
                        .locator
                        .strip_suffix("#extracted-text")
                        .unwrap_or(&doc.locator);
                    if selected
                        .files()
                        .binary_search_by(|v| v.as_str().cmp(locator))
                        .is_err()
                    {
                        revision = store
                            .delete_document(source.id, doc.id, revision)
                            .map_err(err)?;
                        status.removed += 1;
                    }
                }
            }
        }
        SourceKind::Conversation | SourceKind::Email => {
            let mut doc = if source.kind == SourceKind::Conversation {
                conversation(s, owner, parse_chat(&source.locator)?)?
            } else {
                email(
                    s,
                    owner,
                    &serde_json::from_str(&source.locator)
                        .map_err(|_| ApiError::bad_request("invalid email locator"))?,
                )?
            };
            redact_document(s, &mut doc)?;
            let outcome = store
                .index_document(source.id, revision, &doc)
                .map_err(err)?;
            status.processed = 1;
            status.unchanged = usize::from(outcome.unchanged);
        }
        SourceKind::SavedWeb | SourceKind::Note => {
            status.unchanged = store.list_documents(source.id).map_err(err)?.len();
        }
    }
    let latest = store
        .source(source.id)
        .map_err(err)?
        .ok_or_else(|| ApiError::not_found("source removed while indexing"))?;
    Ok(KnowledgeMutation {
        source: source_dto(s, &store, latest)?,
        indexing: status,
    })
}

pub(crate) async fn blocking<T: Send + 'static>(
    data_dir: std::path::PathBuf,
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    blocking_with_permit(worker_permit(data_dir)?, work).await
}

/// Read-only settings/listing work has a separate bound so one settings page's
/// concurrent reads do not contend for the serialized source mutation worker.
pub(crate) async fn read_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static READERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = READERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::unavailable("knowledge reader unavailable"))?;
    blocking_with_permit(permit, work).await
}

pub(crate) fn worker_permit(
    data_dir: std::path::PathBuf,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    let mut workers = WORKERS.get_or_init(SourceWorkers::default).lock();
    workers.retain(|_, worker| worker.strong_count() > 0);
    let worker = workers
        .get(&data_dir)
        .and_then(std::sync::Weak::upgrade)
        .unwrap_or_else(|| {
            let worker = Arc::new(tokio::sync::Semaphore::new(1));
            workers.insert(data_dir, Arc::downgrade(&worker));
            worker
        });
    drop(workers);
    worker.try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "knowledge_busy",
            "a knowledge indexing worker is already running",
        )
    })
}

pub(crate) async fn blocking_with_permit<T: Send + 'static>(
    permit: tokio::sync::OwnedSemaphorePermit,
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| ApiError::internal("knowledge worker failed"))?
}

#[utoipa::path(get, path = "/v1/bots/{bot}/knowledge/sources", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), responses((status = 200, body = Vec<KnowledgeSourceDto>), (status = 404, body = ErrorBody)))]
pub(crate) async fn list_sources(
    State(s): State<AppState>,
    ApiPath(bot): ApiPath<Uuid>,
) -> Result<Json<Vec<KnowledgeSourceDto>>, ApiError> {
    let owner = scope(&s, bot)?;
    read_blocking(move || {
        let Some(store) = KnowledgeStore::open_existing(s.data_dir(), owner).map_err(err)? else {
            return Ok(Json(Vec::new()));
        };
        Ok(Json(
            store
                .list_sources()
                .map_err(err)?
                .into_iter()
                .map(|source| source_dto(&s, &store, source))
                .collect::<Result<_, _>>()?,
        ))
    })
    .await
}

#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/sources", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), request_body = KnowledgeSourceInput, responses((status = 201, body = KnowledgeMutation), (status = 409, body = ErrorBody)))]
#[allow(clippy::too_many_lines)] // One match arm per source kind.
pub(crate) async fn create_source(
    State(s): State<AppState>,
    ApiPath(bot): ApiPath<Uuid>,
    ApiJson(input): ApiJson<KnowledgeSourceInput>,
) -> Result<(StatusCode, Json<KnowledgeMutation>), ApiError> {
    let owner = scope(&s, bot)?;
    let authority = try_authority(&s, owner.bot_id)?;
    let result = blocking(s.data_dir(), move || {
        let _authority = authority;
        s.agent().refresh_external_redactor();
        let kind = SourceKind::from(input.kind);
        let label = if kind == SourceKind::SharedBrain && input.label.trim().is_empty() {
            "Shared brain".to_owned()
        } else {
            input.label
        };
        let mut document = None;
        let locator = match kind {
            SourceKind::File | SourceKind::Folder => {
                let path = required(input.path.as_deref(), "path")?;
                SelectedSource::open_on_host(
                    Path::new(&path),
                    kind == SourceKind::Folder,
                    &s.data_dir(),
                    false,
                )
                .map_err(err)?;
                path
            }
            SourceKind::SharedBrain => {
                let root =
                    memory_store::knowledge::source_adapters::trusted_directory(&s.data_dir())
                        .map_err(err)?;
                match root.create_dir("brain") {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(ApiError::internal("cannot create shared brain")),
                }
                s.data_dir().join("brain").display().to_string()
            }
            SourceKind::SavedWeb => {
                let url = required(input.url.as_deref(), "url")?;
                document = Some(
                    saved_web(
                        &url,
                        &required(input.content.as_deref(), "content")?,
                        input.html,
                    )
                    .map_err(err)?,
                );
                url
            }
            SourceKind::Note => {
                let locator = format!("note:{}", Uuid::now_v7());
                document = Some(DocumentInput {
                    locator: locator.clone(),
                    title: label.clone(),
                    content: bounded_text(&required(input.content.as_deref(), "content")?)?,
                    format: DocumentFormat::Markdown,
                    mtime_ms: None,
                });
                locator
            }
            SourceKind::Conversation => {
                let id = input
                    .chat_id
                    .ok_or_else(|| ApiError::bad_request("chat_id is required"))?;
                document = Some(conversation(&s, owner, id)?);
                format!("chat:{id}")
            }
            SourceKind::Email => {
                let locator = EmailLocator {
                    account: required(input.account.as_deref(), "account")?,
                    run_id: input
                        .run_id
                        .ok_or_else(|| ApiError::bad_request("run_id is required"))?,
                    call_id: input
                        .tool_call_id
                        .ok_or_else(|| ApiError::bad_request("tool_call_id is required"))?,
                };
                document = Some(email(&s, owner, &locator)?);
                serde_json::to_string(&locator).map_err(|_| ApiError::internal("email locator"))?
            }
        };
        safe_metadata(&s, &label)?;
        safe_metadata(&s, &locator)?;
        let store = store(&s, owner)?;
        let source = store
            .add_source(NewSource::new(kind, label, locator))
            .map_err(err)?;
        if let Some(mut doc) = document {
            if let Err(error) = redact_document(&s, &mut doc).and_then(|()| {
                store
                    .index_document(source.id, source.revision, &doc)
                    .map_err(err)
                    .map(|_| ())
            }) {
                let _ = store.delete_source(source.id, source.revision);
                return Err(error);
            }
            let source = store
                .source(source.id)
                .map_err(err)?
                .ok_or_else(|| ApiError::not_found("source removed"))?;
            Ok(KnowledgeMutation {
                source: source_dto(&s, &store, source)?,
                indexing: KnowledgeIndexStatus {
                    processed: 1,
                    ..Default::default()
                },
            })
        } else {
            let result = reindex_inner(&s, owner, &source, None);
            if result.is_err()
                && let Some(current) = store.source(source.id).map_err(err)?
            {
                store
                    .delete_source(current.id, current.revision)
                    .map_err(err)?;
            }
            result
        }
    })
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/sources/{source}/reindex", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path)), request_body = KnowledgeReindex, responses((status = 200, body = KnowledgeMutation), (status = 409, body = ErrorBody)))]
pub(crate) async fn reindex_source(
    State(s): State<AppState>,
    ApiPath((bot, id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<KnowledgeReindex>,
) -> Result<Json<KnowledgeMutation>, ApiError> {
    let owner = scope(&s, bot)?;
    let authority = try_authority(&s, owner.bot_id)?;
    crate::knowledge_runtime::quiesce_bot(&s, owner.bot_id).await?;
    Ok(Json(
        blocking(s.data_dir(), move || {
            let _authority = authority;
            s.agent().refresh_external_redactor();
            let source = existing(&store(&s, owner)?, id, input.expected_revision)?;
            reindex_inner(&s, owner, &source, input.cursor.as_deref())
        })
        .await?,
    ))
}

#[utoipa::path(post, path = "/v1/bots/{bot}/knowledge/sources/{source}/embed", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path)), request_body = KnowledgeEmbed, responses((status = 200, body = KnowledgeEmbeddingProgress), (status = 409, body = ErrorBody)))]
pub(crate) async fn embed_source(
    State(s): State<AppState>,
    ApiPath((bot, source)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<KnowledgeEmbed>,
) -> Result<Json<KnowledgeEmbeddingProgress>, ApiError> {
    let owner = scope(&s, bot)?;
    // The runtime acquires the shared lock and checks this revision itself.
    let progress = if input.reset_retry {
        crate::knowledge_runtime::retry_embedding_batch(
            &s,
            owner.bot_id,
            source,
            input.expected_revision,
        )
        .await?
    } else {
        crate::knowledge_runtime::embedding_batch(&s, owner.bot_id, source, input.expected_revision)
            .await?
    };
    Ok(Json(progress.into()))
}

#[utoipa::path(put, path = "/v1/bots/{bot}/knowledge/sources/{source}", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path)), request_body = KnowledgeSourceChange, responses((status = 200, body = KnowledgeSourceDto), (status = 409, body = ErrorBody)))]
pub(crate) async fn update_source(
    State(s): State<AppState>,
    ApiPath((bot, id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<KnowledgeSourceChange>,
) -> Result<Json<KnowledgeSourceDto>, ApiError> {
    let owner = scope(&s, bot)?;
    let authority = try_authority(&s, owner.bot_id)?;
    crate::knowledge_runtime::quiesce_bot(&s, owner.bot_id).await?;
    blocking(s.data_dir(), move || {
        let _authority = authority;
        s.agent().refresh_external_redactor();
        let store = store(&s, owner)?;
        let source = existing(&store, id, input.expected_revision)?;
        let mut update = SourceUpdate::default();
        match input.action {
            KnowledgeSourceAction::Enable => {
                authorize_source(&s, owner, &source)?;
                update.enabled = Some(true);
            }
            KnowledgeSourceAction::Disable => {
                update.enabled = Some(false);
                update.remote_embedding = Some(None);
            }
            KnowledgeSourceAction::Rename => {
                let label = required(input.label.as_deref(), "label")?;
                safe_metadata(&s, &label)?;
                update.label = Some(label);
            }
            KnowledgeSourceAction::Rebind => {
                if !matches!(source.kind, SourceKind::File | SourceKind::Folder) {
                    return Err(ApiError::bad_request(
                        "choose a file or folder source to rebind",
                    ));
                }
                let path = required(input.path.as_deref(), "path")?;
                safe_metadata(&s, &path)?;
                SelectedSource::open_on_host(
                    Path::new(&path),
                    source.kind == SourceKind::Folder,
                    &s.data_dir(),
                    false,
                )
                .map_err(err)?;
                let rebound = store
                    .rebind_source(id, input.expected_revision, path)
                    .map_err(err)?;
                crate::knowledge_runtime::revoke_grant(
                    &s,
                    owner.bot_id,
                    crate::knowledge_runtime::EmbeddingGrantTarget::Documents { source_id: id },
                )?;
                return Ok(Json(source_dto(&s, &store, rebound)?));
            }
            KnowledgeSourceAction::RevokeRemote => update.remote_embedding = Some(None),
        }
        let updated = store
            .update_source(id, input.expected_revision, update)
            .map_err(err)?;
        if matches!(
            input.action,
            KnowledgeSourceAction::Disable | KnowledgeSourceAction::RevokeRemote
        ) {
            crate::knowledge_runtime::revoke_grant(
                &s,
                owner.bot_id,
                crate::knowledge_runtime::EmbeddingGrantTarget::Documents { source_id: id },
            )?;
        }
        Ok(Json(source_dto(&s, &store, updated)?))
    })
    .await
}

#[utoipa::path(delete, path = "/v1/bots/{bot}/knowledge/sources/{source}", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path)), request_body = KnowledgeRevision, responses((status = 204), (status = 409, body = ErrorBody)))]
pub(crate) async fn delete_source(
    State(s): State<AppState>,
    ApiPath((bot, id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<KnowledgeRevision>,
) -> Result<StatusCode, ApiError> {
    let owner = scope(&s, bot)?;
    let authority = try_authority(&s, owner.bot_id)?;
    crate::knowledge_runtime::quiesce_bot(&s, owner.bot_id).await?;
    blocking(s.data_dir(), move || {
        let _authority = authority;
        store(&s, owner)?
            .delete_source(id, input.expected_revision)
            .map_err(err)?;
        crate::knowledge_runtime::revoke_grant(
            &s,
            owner.bot_id,
            crate::knowledge_runtime::EmbeddingGrantTarget::Documents { source_id: id },
        )?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// Recent successful, explicitly scoped mailbox reads available to this bot.
#[utoipa::path(get, path = "/v1/bots/{bot}/knowledge/email-captures", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), responses((status = 200, body = Vec<KnowledgeEmailCapture>), (status = 404, body = ErrorBody)))]
pub(crate) async fn email_captures(
    State(s): State<AppState>,
    ApiPath(bot): ApiPath<Uuid>,
) -> Result<Json<Vec<KnowledgeEmailCapture>>, ApiError> {
    let owner = scope(&s, bot)?;
    read_blocking(move || {
        let bot_text = owner.bot_id.to_string();
        let accounts: Vec<String> = s
            .email()
            .map(|host| {
                host.list()
                    .into_iter()
                    .filter(|a| a.bot.as_ref().is_none_or(|b| b == &bot_text))
                    .map(|a| a.name)
                    .collect()
            })
            .unwrap_or_default();
        let mut found = Vec::new();
        for step in s.storage().knowledge_tool_steps(
            owner.user_id,
            owner.bot_id,
            None,
            100,
            MAX_TEXT_BYTES * 4,
        )? {
            if step.name != "tools" || step.user_id != owner.user_id {
                continue;
            }
            let Some(results) = step.output.as_ref().and_then(|v| v["results"].as_array()) else {
                continue;
            };
            for result in results {
                if result["name"] != "email_read"
                    || result["is_error"] != false
                    || result["spill_path"].is_string()
                {
                    continue;
                }
                let Some(call_id) = result["call_uuid"]
                    .as_str()
                    .and_then(|id| id.parse::<Uuid>().ok())
                else {
                    continue;
                };
                let Some(call) = step.input["calls"].as_array().and_then(|calls| {
                    calls
                        .iter()
                        .find(|call| call["id"] == result["id"] && call["name"] == "email_read")
                }) else {
                    continue;
                };
                let args: Value =
                    serde_json::from_str(call["arguments"].as_str().unwrap_or_default())
                        .unwrap_or(Value::Null);
                let Some(account) = args["account"]
                    .as_str()
                    .filter(|account| accounts.iter().any(|a| a == *account))
                else {
                    continue;
                };
                found.push(KnowledgeEmailCapture {
                    run_id: step.run_id.0,
                    tool_call_id: call_id,
                    account: account.into(),
                    created_ms: step.finished_at_ms.unwrap_or(step.started_at_ms),
                });
                if found.len() == 50 {
                    return Ok(Json(found));
                }
            }
        }
        Ok(Json(found))
    })
    .await
}

/// Caller already owns the source mutation permit, preventing new selections
/// during this scan. Sorted per-bot lanes cannot deadlock with another account.
pub(crate) async fn email_authorities(
    s: &AppState,
    account: &str,
) -> Result<Vec<tokio::sync::OwnedMutexGuard<()>>, ApiError> {
    let state = s.clone();
    let account = account.to_owned();
    let mut affected = read_blocking(move || {
        let mut affected = Vec::new();
        for bot in state.storage().list_bots(state.user_id())? {
            let Some(index) = KnowledgeStore::open_existing(
                state.data_dir(),
                Scope {
                    user_id: bot.user_id,
                    bot_id: bot.id,
                },
            )
            .map_err(err)?
            else {
                continue;
            };
            if index.list_sources().map_err(err)?.iter().any(|source| {
                source.kind == SourceKind::Email
                    && serde_json::from_str::<EmailLocator>(&source.locator)
                        .is_ok_and(|locator| locator.account == account)
            }) {
                affected.push(bot.id);
            }
        }
        Ok(affected)
    })
    .await?;
    affected.sort_unstable_by_key(|bot| bot.0);
    let mut guards = Vec::with_capacity(affected.len());
    for bot in affected {
        let guard = authority_lock(s, bot).lock_owned().await;
        crate::knowledge_runtime::quiesce_bot(s, bot).await?;
        guards.push(guard);
    }
    Ok(guards)
}

/// Rebinding/removing a mailbox revokes every copied source of that account.
/// Called after a confirmed mailbox binding change or removal.
pub(crate) fn revoke_email_sources(s: &AppState, account: &str) -> Result<(), ApiError> {
    for bot in s.storage().list_bots(s.user_id())? {
        if !s
            .data_dir()
            .join("bots")
            .join(bot.id.to_string())
            .join("knowledge.db")
            .is_file()
        {
            continue;
        }
        let store = store(
            s,
            Scope {
                user_id: bot.user_id,
                bot_id: bot.id,
            },
        )?;
        for source in store.list_sources().map_err(err)? {
            if source.kind != SourceKind::Email {
                continue;
            }
            if serde_json::from_str::<EmailLocator>(&source.locator)
                .is_ok_and(|v| v.account == account)
            {
                store
                    .update_source(
                        source.id,
                        source.revision,
                        SourceUpdate {
                            enabled: Some(false),
                            remote_embedding: Some(None),
                            ..Default::default()
                        },
                    )
                    .map_err(err)?;
                crate::knowledge_runtime::revoke_grant(
                    s,
                    bot.id,
                    crate::knowledge_runtime::EmbeddingGrantTarget::Documents {
                        source_id: source.id,
                    },
                )?;
            }
        }
    }
    Ok(())
}

/// Caller holds the bot authority lane and the bot deletion transaction.
/// Remove only this bot's knowledge files through no-follow directory handles.
pub(crate) fn remove_bot_index(s: &AppState, bot: &storage::Bot) -> std::io::Result<()> {
    crate::knowledge_runtime::forget_bot(s, bot.id);
    let data = memory_store::knowledge::source_adapters::trusted_directory(&s.data_dir())
        .map_err(|_| std::io::Error::other("unsafe knowledge state directory"))?;
    let bots = match data.open_dir_nofollow("bots") {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let directory = match bots.open_dir_nofollow(bot.id.to_string()) {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let files = [
        "knowledge.db-wal",
        "knowledge.db-shm",
        "knowledge.db-journal",
        "knowledge.db",
    ];
    for name in files {
        match directory.symlink_metadata(name) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(std::io::Error::other(
                    "knowledge index contains an unsafe file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if directory.symlink_metadata("knowledge.db").is_ok() {
        // Attest owner before unlinking; never erase a mismatched scoped index.
        drop(
            KnowledgeStore::open(
                s.data_dir(),
                Scope {
                    user_id: bot.user_id,
                    bot_id: bot.id,
                },
            )
            .map_err(|_| std::io::Error::other("knowledge index owner mismatch"))?,
        );
    }
    for name in files {
        match directory.remove_file(name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

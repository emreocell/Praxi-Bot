//! Deleting a bot for good, whatever chats and runs it has: `GET /v1/bots/{id}/delete-preview`
//! says what would go (counts) and what is outside the database (a mailbox, channels), and
//! `DELETE /v1/bots/{id}` removes it.
//!
//! Both are owner-only. The delete is refused while the bot has a run or a mission in progress.
//! One transaction removes everything the bot owns in the main database
//! ([`storage::Storage::delete_bot_cascade`]); the stores that live elsewhere (memory, missions,
//! schedules, knowledge, channel and mailbox links) are cleaned in the same step or right after.
//! The bot-management tool a bot itself can call keeps the stricter
//! [`crate::routes::remove_bot`] (it refuses a bot with history): only the owner may erase one.
//!
//! What is never touched: files the bot wrote into the owner's folders (or its workspace folder),
//! IMAP/SMTP accounts other bots share (they are only unlinked), the `AgentMail` inbox unless the
//! owner asks for it, and the audit log (it gets a `bot.deleted` entry).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use protocol::BotId;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::email::EmailProviderKind;
use crate::error::{ApiError, ErrorBody};
use crate::routes::id;
use crate::{AppState, Bot};

/// What happens to a mailbox when the bot is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotDeleteMailboxFate {
    /// Only this bot used the account: it is removed with its stored password or API key. An
    /// `AgentMail` inbox at the provider stays unless the owner asks to delete it as well.
    Removed,
    /// Other bots were given a share: the account stays and passes to the first of them, with
    /// the same bots as before (minus this one). Nobody gains access.
    HandedOver,
    /// The account belongs to another bot or to every bot; this bot only used or shared it.
    Unlinked,
}

/// A mailbox the bot owns, shares or uses by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotDeleteMailbox {
    /// Account name.
    pub name: String,
    /// Address mail is sent from.
    pub address: String,
    /// Kind of mailbox.
    pub provider: EmailProviderKind,
    /// What happens to it.
    pub fate: BotDeleteMailboxFate,
    /// The inbox at the provider can be deleted with the bot (an `AgentMail` inbox only this bot
    /// used). Offered as a separate choice; mail in it is lost for good.
    pub inbox_deletable: bool,
}

/// How many chats of a messaging channel chose this bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotDeleteChannel {
    /// Channel id (`telegram`, `discord`).
    pub channel: String,
    /// Chats (channels) that chose the bot. Their choice is forgotten; pairing stays.
    pub chats: u64,
}

/// What deleting a bot would remove (`GET /v1/bots/{id}/delete-preview`). Nothing is changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotDeletePreview {
    /// The bot.
    pub bot_id: Uuid,
    /// Its name, which the owner types to confirm.
    pub name: String,
    /// Chats.
    pub chats: u64,
    /// Messages in those chats.
    pub messages: u64,
    /// Runs, subagent and scheduled runs included.
    pub runs: u64,
    /// Steps of those runs.
    pub run_steps: u64,
    /// Approvals decided in those runs.
    pub approvals: u64,
    /// Schedules and triggers of the bot.
    pub schedules: u64,
    /// Incoming webhooks bound to the bot.
    pub webhooks: u64,
    /// Missions the bot leads.
    pub missions: u64,
    /// Memories only this bot knows (memories shared across bots stay).
    pub memories: u64,
    /// "Always allow" and per-site or per-app rules the owner gave this bot.
    pub grants: u64,
    /// Messaging channels that map chats to this bot.
    pub channels: Vec<BotDeleteChannel>,
    /// Mailboxes it owns, shares or uses by default.
    pub mailboxes: Vec<BotDeleteMailbox>,
    /// Runs that have not finished. Deleting is refused while there are any.
    pub active_runs: u64,
    /// Missions in the running state. Deleting is refused while there are any.
    pub running_missions: u64,
    /// The bot's workspace folder on this computer. It is never deleted.
    pub workspace_folder: Option<String>,
}

/// Query of `DELETE /v1/bots/{id}`.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub(crate) struct DeleteBotQuery {
    /// Also delete the `AgentMail` inbox(es) only this bot used, at the provider. Default `false`:
    /// the mail in an inbox would be lost for good.
    #[serde(default)]
    delete_inbox: bool,
}

/// What was removed (`DELETE /v1/bots/{id}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotDeleteReport {
    /// The bot's name.
    pub name: String,
    /// Chats removed.
    pub chats: u64,
    /// Messages removed.
    pub messages: u64,
    /// Runs removed.
    pub runs: u64,
    /// Schedules and triggers removed.
    pub schedules: u64,
    /// Missions removed.
    pub missions: u64,
    /// Memories removed.
    pub memories: u64,
    /// Mailbox accounts removed from this app.
    pub mailboxes_removed: u64,
    /// Mailbox accounts that stay for other bots.
    pub mailboxes_kept: u64,
    /// Addresses of the `AgentMail` inboxes deleted at the provider.
    pub inboxes_deleted: Vec<String>,
}

const RUN_ACTIVE: &str = "the bot has a run in progress: stop it first, then delete it";
const MISSION_RUNNING: &str =
    "the bot leads a mission that is running: stop the mission first, then delete the bot";

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The mailboxes `forget_bot` of the e-mail host would remove, hand over or unlink. The same
/// rules as there: an account bound to the bot goes to the first other bot it is shared with, or
/// is removed when there is none; any other account only loses the bot from its lists.
fn mailboxes_of(s: &AppState, bot: BotId) -> Vec<BotDeleteMailbox> {
    let Some(email) = s.email() else {
        return Vec::new();
    };
    let me = bot.to_string();
    email
        .list()
        .into_iter()
        .filter_map(|a| {
            let owned = a.bot.as_deref().is_some_and(|b| same(b, &me));
            let fate = if owned {
                if a.shared_with.iter().any(|b| !same(b, &me)) {
                    BotDeleteMailboxFate::HandedOver
                } else {
                    BotDeleteMailboxFate::Removed
                }
            } else if a.shared_with.iter().any(|b| same(b, &me))
                || a.default_for.iter().any(|b| same(b, &me))
            {
                BotDeleteMailboxFate::Unlinked
            } else {
                return None;
            };
            Some(BotDeleteMailbox {
                inbox_deletable: fate == BotDeleteMailboxFate::Removed
                    && a.provider == EmailProviderKind::Agentmail,
                name: a.name,
                address: a.address,
                provider: a.provider,
                fate,
            })
        })
        .collect()
}

fn preview_of(s: &AppState, bot: &Bot) -> Result<BotDeletePreview, ApiError> {
    let data = s.storage().bot_data_counts(bot.id)?;
    let schedules = s
        .schedule_store()
        .ok()
        .and_then(|store| store.list_by_bot(bot.id).ok())
        .map_or(0, |list| list.len() as u64);
    let missions = s
        .mission_store()
        .ok()
        .and_then(|store| store.bot_mission_counts(&bot.id.to_string()).ok())
        .unwrap_or_default();
    let memories = s
        .memory_store_if_present()
        .and_then(|m| m.count_bot(bot.user_id, bot.id).ok())
        .map_or(0, |n| n as u64);
    let channels = s
        .channels()
        .iter()
        .filter_map(|c| {
            let chats = c.chats_using_bot(bot.id) as u64;
            (chats > 0).then(|| BotDeleteChannel {
                channel: c.info().id,
                chats,
            })
        })
        .collect();
    let workspace_folder = s
        .storage()
        .get_bot_workspace(bot.id)?
        .map(|w| w.folder_name);
    Ok(BotDeletePreview {
        bot_id: bot.id.0,
        name: bot.name.clone(),
        chats: data.chats,
        messages: data.messages,
        runs: data.runs,
        run_steps: data.run_steps,
        approvals: data.approvals,
        schedules,
        webhooks: data.webhooks,
        missions: missions.total,
        memories,
        grants: s.agent().count_standing_rules(bot.id) as u64,
        channels,
        mailboxes: mailboxes_of(s, bot.id),
        active_runs: data.active_runs,
        running_missions: missions.running,
        workspace_folder,
    })
}

/// Refuse while the bot is doing something: stop it first.
fn refuse_while_busy(p: &BotDeletePreview) -> Result<(), ApiError> {
    if p.active_runs > 0 {
        return Err(ApiError::conflict(RUN_ACTIVE));
    }
    if p.running_missions > 0 {
        return Err(ApiError::conflict(MISSION_RUNNING));
    }
    Ok(())
}

/// What deleting a bot would remove. Changes nothing.
#[utoipa::path(get, path = "/v1/bots/{id}/delete-preview", tag = "bots",
    security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id")),
    responses((status = 200, body = BotDeletePreview), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn delete_preview(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(bid): Path<String>,
) -> Result<Json<BotDeletePreview>, ApiError> {
    // Defence in depth next to `OWNER_ROUTES`.
    s.require_owner(&headers)?;
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    Ok(Json(preview_of(&s, &bot)?))
}

/// Delete a bot and everything only it owns, whatever chats and runs it has. Irreversible.
/// Refused (`409`) while the bot has a run or a mission in progress (stop it first). With
/// `delete_inbox=true` the `AgentMail` inbox only this bot used is deleted at the provider first
/// (`502` and nothing deleted if that fails). Files in the owner's folders are never deleted.
#[utoipa::path(delete, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id"), DeleteBotQuery),
    responses((status = 200, body = BotDeleteReport), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "the mail provider failed")))]
pub(crate) async fn delete_bot(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(bid): Path<String>,
    Query(q): Query<DeleteBotQuery>,
) -> Result<Json<BotDeleteReport>, ApiError> {
    s.require_owner(&headers)?;
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    // A dropped connection must not stop it half way.
    tokio::spawn(async move { erase_bot(&s, &bot, q.delete_inbox).await.map(Json) })
        .await
        .map_err(|_| ApiError::internal("bot delete task failed"))?
}

async fn erase_bot(
    s: &AppState,
    bot: &Bot,
    delete_inbox: bool,
) -> Result<BotDeleteReport, ApiError> {
    let preview = preview_of(s, bot)?;
    refuse_while_busy(&preview)?;

    // The irreversible outside step goes first: if the provider refuses, nothing is deleted and
    // the owner can retry (the account, with its API key, is still there). The mailbox leaves
    // through the account removal, which also closes what was read from it.
    let mut inboxes_deleted = Vec::new();
    if delete_inbox && let Some(control) = s.email() {
        for mailbox in preview.mailboxes.iter().filter(|m| m.inbox_deletable) {
            let permit = crate::knowledge::worker_permit(s.data_dir())?;
            crate::email_routes::remove_account_with_links(
                s,
                &control,
                &mailbox.name,
                true,
                permit,
            )
            .await?;
            inboxes_deleted.push(mailbox.address.clone());
        }
    }

    let permit = crate::knowledge::worker_permit(s.data_dir())?;
    let authority = crate::knowledge::authority_lock(s, bot.id)
        .lock_owned()
        .await;
    crate::knowledge_runtime::quiesce_bot(s, bot.id).await?;
    let state = s.clone();
    let removed = bot.clone();
    let cascade = crate::knowledge::blocking_with_permit(permit, move || {
        let _authority = authority;
        let io = |e: &dyn std::fmt::Display| {
            storage::StorageError::Io(std::io::Error::other(e.to_string()))
        };
        state
            .storage()
            .delete_bot_cascade(removed.user_id, removed.id, "user", |gone| {
                crate::knowledge_runtime::revoke_bot_grants(&state, removed.id)
                    .map_err(|_| io(&"knowledge authority could not be revoked"))?;
                crate::knowledge::remove_bot_index(&state, &removed)
                    .map_err(storage::StorageError::Io)?;
                if let Some(memory) = state.memory_store_if_present() {
                    // Memories only this bot knew, their journal and its forgotten items; the
                    // text of its chats in cross-session search. Shared memories stay.
                    memory
                        .erase_bot(removed.user_id, removed.id)
                        .map_err(|e| io(&e))?;
                    memory.erase_chats(&gone.chat_ids).map_err(|e| io(&e))?;
                }
                if let Ok(missions) = state.mission_store() {
                    let bot_id = removed.id.to_string();
                    if missions
                        .bot_mission_counts(&bot_id)
                        .map_err(|e| io(&e))?
                        .running
                        > 0
                    {
                        return Err(storage::StorageError::Conflict(
                            "bot leads a running mission".into(),
                        ));
                    }
                    missions.delete_for_bot(&bot_id).map_err(|e| io(&e))?;
                }
                Ok(())
            })
            .map_err(|e| match e {
                storage::StorageError::Conflict(m) if m.contains("mission") => {
                    ApiError::conflict(MISSION_RUNNING)
                }
                storage::StorageError::Conflict(_) => ApiError::conflict(RUN_ACTIVE),
                other => ApiError::internal(format!("the bot could not be deleted: {other}")),
            })
    })
    .await?
    .ok_or_else(|| ApiError::not_found("bot"))?;

    // Files that belong to the rows that are gone: each run's event log, each chat's attachments.
    let data_dir = s.data_dir();
    for run in &cascade.run_ids {
        let _ = std::fs::remove_file(storage::event_log_path(&data_dir, *run));
    }
    for chat in &cascade.chat_ids {
        agent_core::attachments::remove_chat(&data_dir, *chat);
    }
    // The bot's browser slot, driver and screen target; what other stores keep about it
    // (schedules, MCP and REST-app lists, mailbox links); what the channels remember; what the
    // loop holds in memory.
    s.live_view().registry.retire_bot(bot.id.0);
    crate::routes::forget_bot_links(s, bot).await;
    for channel in s.channels() {
        channel.forget_bot(bot.id).await;
    }
    s.agent().forget_bot(bot.id);

    let removed_mailboxes = preview
        .mailboxes
        .iter()
        .filter(|m| m.fate == BotDeleteMailboxFate::Removed)
        .count() as u64;
    Ok(BotDeleteReport {
        name: bot.name.clone(),
        chats: cascade.removed.chats,
        messages: cascade.removed.messages,
        runs: cascade.removed.runs,
        schedules: preview.schedules,
        missions: preview.missions,
        memories: preview.memories,
        mailboxes_removed: removed_mailboxes,
        mailboxes_kept: preview.mailboxes.len() as u64 - removed_mailboxes,
        inboxes_deleted,
    })
}

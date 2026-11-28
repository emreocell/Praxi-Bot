//! Deleting a bot for good, whatever chats and runs it has: `GET /v1/bots/{id}/delete-preview`
//! says what would go (counts) and what is outside the database (a mailbox, channels), and
//! `DELETE /v1/bots/{id}` removes it.
//!
//! Both are owner-only. The delete happens in this order, so nothing irreversible happens before
//! the point of no return:
//!
//! 1. **Lock.** The bot is retired ([`BotLock`]): no new chat, run (a turn, a schedule's, a
//!    webhook's), mission, mission session or mailbox share starts for it from here on.
//! 2. **Check and commit.** In one step, while the mission store is held: the delete is refused
//!    (`409`) while a mission of the bot is running, waiting (it would wake on its own) or holds
//!    a session, a run of the bot (or any run in one of its chats: another bot answering an
//!    `@mention` there) is unfinished in the database, or such a run is alive in the loop.
//!    Otherwise its missions and its rows in the main database go, and the main database's
//!    transaction ([`storage::Storage::delete_bot_cascade`]) is the commit point. When it fails,
//!    nothing at all is deleted and the bot works again.
//! 3. **Clean up.** What cannot be rolled back runs after the commit: knowledge index and
//!    grants, memories and chat search text, event logs and attachments (never through a link),
//!    webhook secrets, schedules, MCP/app/channel links. A cleanup marker, written in the commit,
//!    stays until every step is done; a step that fails, or that needs a store or host this
//!    daemon does not have right now, is reported (`cleanup_pending`) and retried by
//!    `DELETE /v1/bots/{id}` again, and once when the daemon starts ([`spawn_pending_cleanups`]).
//!    One cleanup of a bot runs at a time ([`CleanupClaim`]); another request for it meanwhile
//!    is refused (`409`).
//! 4. **Mailboxes last.** Accounts only this bot could use are removed (shared ones pass to the
//!    first bot they were shared with); with `delete_inbox=true` the `AgentMail` inbox goes at
//!    the provider, only if it is still the bot's alone, decided under the mail host's lock.
//!    When the provider fails the bot is already gone; the report says so and the same request
//!    retries.
//!
//! The bot-management tool a bot itself can call keeps the stricter
//! [`crate::routes::remove_bot`] (it refuses a bot with history): only the owner may erase one.
//!
//! What is never touched: files the bot wrote into the owner's folders (or its workspace folder),
//! IMAP/SMTP accounts other bots share (they are only unlinked), the `AgentMail` inbox unless the
//! owner asks for it, and the audit log (it gets a `bot.deleted` entry).

use std::collections::BTreeSet;
use std::path::Path;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use cap_fs_ext::DirExt;
use missions::{IdleDelete, MissionError};
use protocol::BotId;
use serde::{Deserialize, Serialize};
use storage::{BotCascade, BotDeleteCleanup, StorageError};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::email::{EmailInboxNotDeleted, EmailProviderKind, ForgottenMailboxes};
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
    /// Missions in the running state (or holding a session). Deleting is refused while there are
    /// any.
    pub running_missions: u64,
    /// Missions waiting for a timer, a mail or a webhook (they would wake on their own).
    /// Deleting is refused while there are any: cancel them first.
    pub waiting_missions: u64,
    /// The bot's workspace folder on this computer. It is never deleted.
    pub workspace_folder: Option<String>,
}

/// Query of `DELETE /v1/bots/{id}`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct DeleteBotQuery {
    /// Also delete the `AgentMail` inbox(es) only this bot used, at the provider. Default `false`:
    /// the mail in an inbox would be lost for good.
    #[serde(default)]
    delete_inbox: bool,
}

/// A cleanup step after the bot's rows were deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotDeleteStep {
    /// Its knowledge index and knowledge grants.
    Knowledge,
    /// Its memories and the search text of its chats.
    Memory,
    /// The event logs of its runs and the attachments of its chats.
    Files,
    /// The signing secrets of its webhooks.
    Secrets,
    /// Its missions.
    Missions,
    /// Its schedules and triggers.
    Schedules,
    /// Its place in MCP servers' and connected apps' bot lists.
    Links,
    /// Its mailboxes (and the inbox at the provider, when asked).
    Mailboxes,
}

/// What was removed (`DELETE /v1/bots/{id}`). The bot is gone, whatever else it says.
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
    /// Incoming webhooks removed.
    pub webhooks: u64,
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
    /// Inboxes asked for that were not deleted at the provider, and why.
    pub inboxes_not_deleted: Vec<EmailInboxNotDeleted>,
    /// Cleanup steps that did not finish. The bot is gone; `DELETE /v1/bots/{id}` again (with
    /// the same `delete_inbox`) retries them.
    pub cleanup_pending: Vec<BotDeleteStep>,
}

const RUN_ACTIVE: &str = "the bot has a run in progress: stop it first, then delete it";
const MISSION_RUNNING: &str =
    "the bot leads a mission that is running: stop the mission first, then delete the bot";
const MISSION_WAITING: &str = "the bot leads a mission that is waiting for a timer, a mail or a webhook and would wake on its own: cancel it first, then delete the bot";
const ALREADY_DELETING: &str = "this bot is already being deleted";
/// The mail settings of `bot serve` in the data directory.
const EMAIL_FILE: &str = "email.toml";

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The mailboxes `forget_bot` of the e-mail host would remove, hand over or unlink. The same
/// rules as there: an account bound to the bot goes to the first other bot it is shared with, or
/// is removed when there is none; any other account only loses the bot from its lists. Its inbox
/// can go at the provider only when no other bot shares it or lists it as its default.
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
                if crate::email::mailbox_heir(&a.shared_with, &me).is_some() {
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
                    && a.provider == EmailProviderKind::Agentmail
                    && crate::email::mailbox_exclusive(&a.shared_with, &a.default_for, &me),
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
        running_missions: missions.running.max(missions.leased),
        waiting_missions: missions.waiting,
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
    if p.waiting_missions > 0 {
        return Err(ApiError::conflict(MISSION_WAITING));
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
    AxumPath(bid): AxumPath<String>,
) -> Result<Json<BotDeletePreview>, ApiError> {
    // Defence in depth next to `OWNER_ROUTES`.
    s.require_owner(&headers)?;
    let bot = s.bot_of_user(BotId(id(&bid)?))?;
    Ok(Json(preview_of(&s, &bot)?))
}

/// Delete a bot and everything only it owns, whatever chats and runs it has. Irreversible.
/// Refused (`409`) while the bot has a run or a mission in progress or waiting (stop or cancel
/// it first); then nothing is deleted. With `delete_inbox=true` the `AgentMail` inbox only this
/// bot used is deleted at the provider as the last step, after the bot is gone here; when the
/// provider fails, the report says so (`inboxes_not_deleted`). Cleanup steps that fail after the
/// bot's rows are gone are reported in `cleanup_pending`; the same request retries them (also for
/// a bot that is already gone). Files in the owner's folders are never deleted.
#[utoipa::path(delete, path = "/v1/bots/{id}", tag = "bots", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Bot id"), DeleteBotQuery),
    responses((status = 200, body = BotDeleteReport), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub(crate) async fn delete_bot(
    State(s): State<AppState>,
    headers: HeaderMap,
    AxumPath(bid): AxumPath<String>,
    Query(q): Query<DeleteBotQuery>,
) -> Result<Json<BotDeleteReport>, ApiError> {
    s.require_owner(&headers)?;
    let bot = BotId(id(&bid)?);
    // A dropped connection must not stop it half way.
    tokio::spawn(async move {
        match s.bot_of_user(bot) {
            Ok(bot) => erase_bot(&s, &bot, q.delete_inbox).await.map(Json),
            Err(gone) => retry_cleanup(&s, bot, q.delete_inbox)
                .await?
                .map(Json)
                .ok_or(gone),
        }
    })
    .await
    .map_err(|_| ApiError::internal("bot delete task failed"))?
}

/// While a bot is being deleted nothing new starts for it: no chat, run (a turn, a schedule's or
/// a webhook's), mission, mission session or mailbox share ([`is_being_deleted`]). Dropped
/// without [`BotLock::keep`] (the delete did not happen) it lets the bot work again.
pub(crate) struct BotLock {
    s: AppState,
    bot: BotId,
    kept: bool,
}

impl BotLock {
    pub(crate) fn take(s: &AppState, bot: BotId) -> Result<Self, ApiError> {
        if !s.storage().retire_bot(bot) {
            return Err(ApiError::conflict(ALREADY_DELETING));
        }
        if let Ok(missions) = s.mission_store() {
            missions.retire_bot(&bot.to_string());
        }
        Ok(Self {
            s: s.clone(),
            bot,
            kept: false,
        })
    }

    /// The bot is gone: it stays retired.
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for BotLock {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        self.s.storage().restore_bot(self.bot);
        if let Ok(missions) = self.s.mission_store() {
            missions.restore_bot(&self.bot.to_string());
        }
    }
}

/// Whether `raw` names a bot that is being (or was) deleted: nothing may be given to it.
pub(crate) fn is_being_deleted(s: &AppState, raw: &str) -> bool {
    raw.parse::<Uuid>()
        .is_ok_and(|id| s.storage().is_retired(BotId(id)))
}

/// Why the check-and-commit step did not delete anything.
enum CommitError {
    Api(ApiError),
    Missions(MissionError),
}

impl From<MissionError> for CommitError {
    fn from(e: MissionError) -> Self {
        Self::Missions(e)
    }
}

impl From<ApiError> for CommitError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

/// The bot's rows are gone; what the commit left for the cleanup.
struct Committed {
    cascade: BotCascade,
    /// Missions deleted, or `None` when their delete could not be committed (the cleanup retries).
    missions: Option<u64>,
}

/// Step 2: check that nothing of the bot is alive and delete its missions and its rows, all while
/// the mission store is held (no mission can start, wake or take a session meanwhile) and inside
/// the main database's write transaction (no run can be inserted or change state meanwhile). The
/// missions are committed only after the main database committed. Blocking.
fn check_and_commit(s: &AppState, bot: &Bot, delete_inbox: bool) -> Result<Committed, ApiError> {
    let delete_rows = |_missions: u64| -> Result<BotCascade, CommitError> {
        s.storage()
            .delete_bot_cascade(bot.user_id, bot.id, "user", delete_inbox, |chats| {
                fault(bot.id, Fault::LiveRun)?;
                if s.agent().bot_has_live_run(bot.id, chats) {
                    return Err(StorageError::Conflict("a run of the bot is alive".into()));
                }
                Ok(())
            })
            .map_err(|e| match e {
                StorageError::Conflict(_) => ApiError::conflict(RUN_ACTIVE),
                other => ApiError::internal(format!("the bot could not be deleted: {other}")),
            })?
            .ok_or_else(|| ApiError::not_found(format!("bot {}", bot.id)).into())
    };
    let outcome = match s.mission_store() {
        Ok(missions) => missions.delete_for_bot_if_idle(&bot.id.to_string(), delete_rows),
        // Without the mission store nothing can run a mission.
        Err(_) => delete_rows(0).map(|cascade| IdleDelete::Done {
            outcome: cascade,
            missions: Ok(0),
        }),
    };
    match outcome {
        Ok(IdleDelete::Busy(c)) => Err(ApiError::conflict(
            if c.running == 0 && c.leased == 0 && c.waiting > 0 {
                MISSION_WAITING
            } else {
                MISSION_RUNNING
            },
        )),
        Ok(IdleDelete::Done { outcome, missions }) => Ok(Committed {
            cascade: outcome,
            missions: missions.ok(),
        }),
        Err(CommitError::Api(e)) => Err(e),
        Err(CommitError::Missions(e)) => Err(ApiError::internal(format!(
            "the bot could not be deleted: {e}"
        ))),
    }
}

async fn erase_bot(
    s: &AppState,
    bot: &Bot,
    delete_inbox: bool,
) -> Result<BotDeleteReport, ApiError> {
    // 1. Nothing new starts for the bot from here on. Its cleanup is this request's until it
    // returns, so a retry can not run the same steps beside it.
    let lock = BotLock::take(s, bot.id)?;
    let _claim = CleanupClaim::take(s, bot.id)?;
    // A clear answer for what is already known; step 2 decides again, atomically.
    refuse_while_busy(&preview_of(s, bot)?)?;
    if s.agent()
        .bot_has_live_run(bot.id, &s.storage().bot_chat_ids(bot.id)?)
    {
        return Err(ApiError::conflict(RUN_ACTIVE));
    }

    // 2. The point of no return. The knowledge authority and the worker are held from here
    // through the removal of the index, so no search or ingest of the bot runs in between
    // (cancelling one destroys nothing).
    let permit = crate::knowledge::worker_permit(s.data_dir())?;
    let authority = crate::knowledge::authority_lock(s, bot.id)
        .lock_owned()
        .await;
    crate::knowledge_runtime::quiesce_bot(s, bot.id).await?;
    let state = s.clone();
    let removed = bot.clone();
    // The knowledge index goes in the same worker, right after the commit (it cannot be rolled
    // back, so never before).
    let (committed, knowledge) = crate::knowledge::blocking_with_permit(permit, move || {
        let _authority = authority;
        let committed = check_and_commit(&state, &removed, delete_inbox)?;
        Ok((committed, remove_knowledge(&state, &removed)))
    })
    .await?;
    lock.keep();

    // 3. and 4. What cannot be rolled back, now that the bot is gone. With missions off here
    // a ledger may still hold the bot's missions: the cleanup looks.
    let missions_left = committed.missions.is_none() || s.mission_store().is_err();
    let cleaned = clean_up(
        s,
        &committed.cascade.cleanup,
        missions_left,
        Some(knowledge),
    )
    .await;
    let removed = committed.cascade.removed;
    let mut report = cleaned.report(committed.cascade.cleanup.name.clone());
    report.chats = removed.chats;
    report.messages = removed.messages;
    report.runs = removed.runs;
    report.webhooks = removed.webhooks;
    report.missions += committed.missions.unwrap_or(0);
    Ok(report)
}

/// `DELETE` of a bot that is already gone: finish the cleanup its delete left, if any.
async fn retry_cleanup(
    s: &AppState,
    bot: BotId,
    delete_inbox: bool,
) -> Result<Option<BotDeleteReport>, ApiError> {
    // Claimed before the marker is read: a cleanup that just finished is not run again.
    let _claim = CleanupClaim::take(s, bot)?;
    let Some(mut left) = s
        .storage()
        .bot_delete_cleanup(bot)?
        .filter(|c| c.user_id == s.user_id())
    else {
        return Ok(None);
    };
    // A gone bot stays retired, in case a stale request tries to give it something (`false`
    // when it already was: nothing to undo).
    let _ = s.storage().retire_bot(bot);
    // Asked now to delete the inbox as well: remembered for the next retry before anything runs.
    if delete_inbox && !left.delete_inbox {
        left.delete_inbox = true;
        s.storage().save_bot_delete_cleanup(&left)?;
    }
    let cleaned = clean_up(s, &left, true, None).await;
    Ok(Some(cleaned.report(left.name.clone())))
}

/// How long after the daemon starts the cleanups a delete left are retried (the mail host, MCP
/// servers and channels are wired by then).
const PENDING_CLEANUP_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// Retry, once at daemon start, the cleanups bot deletes left (a failing step, or the daemon
/// stopped half way): the bot is gone from the list, so the owner has nothing to click "try
/// again" on. The same steps as `DELETE /v1/bots/{id}` on a gone bot, with the `delete_inbox` the
/// owner chose then. Safe to call once at daemon start.
pub fn spawn_pending_cleanups(state: AppState) {
    if state.safe_mode() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(PENDING_CLEANUP_DELAY).await;
        finish_pending_cleanups(&state).await;
    });
}

/// Every unfinished cleanup of this owner's deleted bots, retried once; one that a request is
/// running right now is left to it. Returns how many are still unfinished.
pub(crate) async fn finish_pending_cleanups(s: &AppState) -> usize {
    let pending = match s.storage().bot_delete_cleanups() {
        Ok(list) => list,
        Err(e) => {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: warning: could not read the bot delete markers: {e}");
            return 0;
        }
    };
    let mut left = 0;
    for c in pending.into_iter().filter(|c| c.user_id == s.user_id()) {
        let Ok(_claim) = CleanupClaim::take(s, c.bot_id) else {
            left += 1;
            continue;
        };
        // Read again under the claim: it may have finished meanwhile.
        let c = match s.storage().bot_delete_cleanup(c.bot_id) {
            Ok(Some(c)) => c,
            Ok(None) => continue,
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: warning: the bot delete marker: {e}");
                left += 1;
                continue;
            }
        };
        let _ = s.storage().retire_bot(c.bot_id);
        let cleaned = clean_up(s, &c, true, None).await;
        if !cleaned.pending.is_empty() {
            left += 1;
        }
    }
    left
}

/// The cleanup of a deleted bot (steps 3 and 4) is being run by its holder: a delete from before
/// the commit until it returns, or a retry. Nobody else runs it meanwhile.
struct CleanupClaim {
    s: AppState,
    bot: BotId,
}

impl CleanupClaim {
    fn take(s: &AppState, bot: BotId) -> Result<Self, ApiError> {
        if !s.storage().claim_bot_delete_cleanup(bot) {
            return Err(ApiError::conflict(ALREADY_DELETING));
        }
        Ok(Self { s: s.clone(), bot })
    }
}

impl Drop for CleanupClaim {
    fn drop(&mut self) {
        self.s.storage().release_bot_delete_cleanup(self.bot);
    }
}

/// What the cleanup removed, and the steps that did not finish.
#[derive(Default)]
struct Cleaned {
    schedules: u64,
    missions: u64,
    memories: u64,
    mail: ForgottenMailboxes,
    pending: Vec<BotDeleteStep>,
}

impl Cleaned {
    fn report(self, name: String) -> BotDeleteReport {
        BotDeleteReport {
            name,
            schedules: self.schedules,
            missions: self.missions,
            memories: self.memories,
            mailboxes_removed: self.mail.removed.len() as u64,
            mailboxes_kept: self.mail.kept.len() as u64,
            inboxes_deleted: self.mail.inboxes_deleted,
            inboxes_not_deleted: self.mail.inboxes_not_deleted,
            cleanup_pending: self.pending,
            ..BotDeleteReport::default()
        }
    }

    /// Record a step's outcome: a failure is logged and left for the retry.
    fn step<T>(&mut self, step: BotDeleteStep, result: Result<T, String>) -> Option<T> {
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: warning: bot delete cleanup step {step:?} failed: {e}");
                self.pending.push(step);
                None
            }
        }
    }
}

/// Steps 3 and 4: everything outside the main database, after its commit. Every step runs even
/// when an earlier one failed; the marker goes only when all of them finished. The mailboxes come
/// last (an inbox at the provider is the one thing outside this computer).
async fn clean_up(
    s: &AppState,
    c: &BotDeleteCleanup,
    missions_left: bool,
    knowledge: Option<Result<(), String>>,
) -> Cleaned {
    let mut out = Cleaned::default();
    let bot = Bot {
        id: c.bot_id,
        user_id: c.user_id,
        name: c.name.clone(),
        config: serde_json::Value::Null,
        created_at_ms: 0,
    };

    // Knowledge index and grants (done already, right after the commit, unless this is a retry).
    let knowledge = match knowledge {
        Some(done) => done,
        None => remove_knowledge_again(s, &bot).await,
    };
    out.step(BotDeleteStep::Knowledge, knowledge);

    // Memories only this bot knew, their journal and its forgotten items; the text of its chats
    // in cross-session search. Shared memories stay.
    let memory = (|| {
        fault(c.bot_id, Fault::Step(BotDeleteStep::Memory)).map_err(|e| e.to_string())?;
        let Some(memory) = s.memory_store_if_present() else {
            return Ok(0);
        };
        let n = memory
            .erase_bot(c.user_id, c.bot_id)
            .map_err(|e| e.to_string())?;
        memory.erase_chats(&c.chat_ids).map_err(|e| e.to_string())?;
        Ok(n as u64)
    })();
    out.memories = out.step(BotDeleteStep::Memory, memory).unwrap_or(0);

    let files = (|| {
        fault(c.bot_id, Fault::Step(BotDeleteStep::Files)).map_err(|e| e.to_string())?;
        remove_files(&s.data_dir(), c).map_err(|e| e.to_string())
    })();
    out.step(BotDeleteStep::Files, files);

    out.step(BotDeleteStep::Secrets, remove_webhook_secrets(s, c));

    if missions_left {
        let missions = (|| {
            fault(c.bot_id, Fault::Step(BotDeleteStep::Missions)).map_err(|e| e.to_string())?;
            match s.mission_store() {
                Ok(store) => store
                    .delete_for_bot(&c.bot_id.to_string())
                    .map_err(|e| e.to_string()),
                // Missions are off here, but a ledger with the bot's missions may still exist.
                Err(e) if s.data_dir().join(missions::DB_FILE).exists() => {
                    Err(e.message().to_owned())
                }
                Err(_) => Ok(0),
            }
        })();
        out.missions = out.step(BotDeleteStep::Missions, missions).unwrap_or(0);
    }

    // A job of a deleted bot could only fail on every due time.
    let schedules = (|| {
        fault(c.bot_id, Fault::Step(BotDeleteStep::Schedules)).map_err(|e| e.to_string())?;
        s.schedule_store()
            .and_then(|store| store.delete_by_bot(c.bot_id))
            .map(|n| n as u64)
            .map_err(|e| format!("{e:?}"))
    })();
    out.schedules = out.step(BotDeleteStep::Schedules, schedules).unwrap_or(0);

    // Its id must not linger in any MCP server's or connected REST API's `bots` list (ids are
    // never reused, so a leftover cannot widen access).
    let links = async {
        fault(c.bot_id, Fault::Step(BotDeleteStep::Links)).map_err(|e| e.to_string())?;
        let id = c.bot_id.to_string();
        if let Some(mcp) = s.mcp() {
            mcp.forget_bot(&id).await.map_err(|e| format!("{e:?}"))?;
        }
        if let Some(apps) = s.openapi_apps() {
            apps.forget_bot(&id).await.map_err(|e| format!("{e:?}"))?;
        }
        Ok::<(), String>(())
    }
    .await;
    out.step(BotDeleteStep::Links, links);
    // What lives only in memory: the browser slot, driver and screen target; the chats that
    // chose it in the channels; the loop's profile and rules.
    s.live_view().registry.retire_bot(c.bot_id.0);
    for channel in s.channels() {
        channel.forget_bot(c.bot_id).await;
    }
    s.agent().forget_bot(c.bot_id);

    if let Some(mail) = forget_mailboxes(s, c)
        .await
        .and_then(|r| out.step(BotDeleteStep::Mailboxes, r))
    {
        // An account kept because the provider refused its inbox is left for the retry.
        if mail
            .inboxes_not_deleted
            .iter()
            .any(|n| !mail.removed.contains(&n.account))
        {
            out.pending.push(BotDeleteStep::Mailboxes);
        }
        out.mail = mail;
    }

    if out.pending.is_empty()
        && let Err(e) = s.storage().finish_bot_delete_cleanup(c.bot_id)
    {
        support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: warning: could not clear the bot delete marker: {e}");
    }
    out
}

/// The signing secrets of the bot's webhooks, which live in the secret store.
fn remove_webhook_secrets(s: &AppState, c: &BotDeleteCleanup) -> Result<(), String> {
    fault(c.bot_id, Fault::Step(BotDeleteStep::Secrets)).map_err(|e| e.to_string())?;
    if c.webhook_ids.is_empty() {
        return Ok(());
    }
    let Some(store) = s.secret_store() else {
        return Err("the secret store is not available".into());
    };
    for hook in &c.webhook_ids {
        store
            .delete(&crate::incoming_webhook::secret_name(*hook))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// [`remove_knowledge`] on a retry, under the bot's knowledge authority and a worker permit.
async fn remove_knowledge_again(s: &AppState, bot: &Bot) -> Result<(), String> {
    let authority = crate::knowledge::authority_lock(s, bot.id)
        .lock_owned()
        .await;
    let permit =
        crate::knowledge::worker_permit(s.data_dir()).map_err(|e| e.message().to_owned())?;
    let state = s.clone();
    let gone = bot.clone();
    crate::knowledge::blocking_with_permit(permit, move || {
        let _authority = authority;
        Ok(remove_knowledge(&state, &gone))
    })
    .await
    .unwrap_or_else(|e| Err(e.message().to_owned()))
}

/// The bot's knowledge grants and index. Blocking; call holding its knowledge authority and a
/// worker permit.
fn remove_knowledge(s: &AppState, bot: &Bot) -> Result<(), String> {
    fault(bot.id, Fault::Step(BotDeleteStep::Knowledge)).map_err(|e| e.to_string())?;
    crate::knowledge_runtime::revoke_bot_grants(s, bot.id).map_err(|e| e.message().to_owned())?;
    crate::knowledge::remove_bot_index(s, bot).map_err(|e| format!("knowledge index: {e}"))
}

/// Step 4: the bot's mailboxes, and with `delete_inbox` its own inbox at the provider; then the
/// mail tools of the bots that keep or lose an account, and the knowledge sources read from
/// removed accounts. `None` when there is nothing to do: no mail host, no mail settings and no
/// sources left to revoke. Without the mail host while mail settings exist, the step stays
/// pending.
async fn forget_mailboxes(
    s: &AppState,
    c: &BotDeleteCleanup,
) -> Option<Result<ForgottenMailboxes, String>> {
    let email = s.email();
    if email.is_none() && c.email_sources.is_empty() && !s.data_dir().join(EMAIL_FILE).exists() {
        return None;
    }
    Some(
        async {
            fault(c.bot_id, Fault::Step(BotDeleteStep::Mailboxes)).map_err(|e| e.to_string())?;
            let Some(email) = email else {
                revoke_mail_sources(s, c, &[]).await?;
                if s.data_dir().join(EMAIL_FILE).exists() {
                    return Err("the mail host is not available".into());
                }
                return Ok(ForgottenMailboxes::default());
            };
            let id = c.bot_id.to_string();
            let before = email.list();
            let out = email
                .forget_bot_mailboxes(&id, c.delete_inbox)
                .await
                .map_err(|e| e.to_string())?;
            // The mail host saved the change: the tools follow it whatever fails below.
            let affected: BTreeSet<String> = before
                .iter()
                .filter(|a| out.removed.contains(&a.name) || out.kept.contains(&a.name))
                .flat_map(crate::email_bot_tools::bots_of)
                .collect();
            crate::email_bot_tools::sync(s, &email.list(), &affected);
            revoke_mail_sources(s, c, &out.removed).await?;
            Ok::<_, String>(out)
        }
        .await,
    )
}

/// The knowledge sources read from the mail accounts the delete removed (`removed` now, and those
/// a failed earlier attempt left in the marker). The accounts still to revoke are saved in the
/// marker before any is revoked and after, so a failure or a stop here is retried though the
/// accounts themselves are gone.
async fn revoke_mail_sources(
    s: &AppState,
    c: &BotDeleteCleanup,
    removed: &[String],
) -> Result<(), String> {
    let save = |l: &BotDeleteCleanup| {
        s.storage()
            .save_bot_delete_cleanup(l)
            .map(|_| ())
            .map_err(|e| format!("the bot delete marker: {e}"))
    };
    let mut left = c.clone();
    for name in removed {
        if !left.email_sources.contains(name) {
            left.email_sources.push(name.clone());
        }
    }
    let mut saved = c.email_sources.clone();
    if left.email_sources != saved {
        match save(&left) {
            Ok(()) => saved.clone_from(&left.email_sources),
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "daemon-api: warning: {e}");
            }
        }
    }
    let mut first_error = None;
    for name in left.email_sources.clone() {
        match revoke_mail_source(s, c.bot_id, &name).await {
            Ok(()) => left.email_sources.retain(|n| *n != name),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    if left.email_sources != saved {
        save(&left)?;
    }
    first_error.map_or(Ok(()), Err)
}

async fn revoke_mail_source(s: &AppState, bot: BotId, account: &str) -> Result<(), String> {
    fault(bot, Fault::RevokeMail).map_err(|e| e.to_string())?;
    let permit = crate::knowledge::worker_permit(s.data_dir()).map_err(|e| e.to_string())?;
    let _authority = crate::knowledge::email_authorities(s, account)
        .await
        .map_err(|e| e.to_string())?;
    let state = s.clone();
    let account = account.to_owned();
    crate::knowledge::blocking_with_permit(permit, move || {
        crate::knowledge::revoke_email_sources(&state, &account)
    })
    .await
    .map_err(|e| e.to_string())
}

/// The event logs of the bot's runs and the attachments of its chats. Opened from the data
/// directory one component at a time without following a link: a `runs` or `attachments` folder,
/// a log or a chat folder that is a symbolic link or a junction is refused, not followed, and
/// nothing inside it is touched.
fn remove_files(data_dir: &Path, c: &BotDeleteCleanup) -> std::io::Result<()> {
    let root = memory_store::knowledge::source_adapters::trusted_directory(data_dir)
        .map_err(|_| std::io::Error::other("unsafe data directory"))?;
    let mut first_error = None;
    let logs: Vec<String> = c.run_ids.iter().map(|r| format!("{r}.ndjson")).collect();
    let chats: Vec<String> = c.chat_ids.iter().map(ToString::to_string).collect();
    for (folder, names, tree) in [("runs", logs, false), ("attachments", chats, true)] {
        if let Err(e) = remove_in(&root, folder, &names, tree) {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn remove_in(
    root: &cap_std::fs::Dir,
    folder: &str,
    names: &[String],
    tree: bool,
) -> std::io::Result<()> {
    use std::io::ErrorKind::NotFound;
    if names.is_empty() {
        return Ok(());
    }
    let dir = match root.open_dir_nofollow(folder) {
        Ok(dir) => dir,
        Err(e) if e.kind() == NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut first_error = None;
    for name in names {
        let result = match dir.symlink_metadata(name) {
            Err(e) if e.kind() == NotFound => Ok(()),
            Err(e) => Err(e),
            Ok(m) if m.file_type().is_symlink() => Err(std::io::Error::other(format!(
                "{folder}/{name} is a link; it was not followed"
            ))),
            // The folder is opened without following a link and removed through that handle, so
            // a junction (a reparse point that may not read as a link) or a link swapped in after
            // the check is refused, not followed.
            Ok(m) if tree && m.is_dir() => match dir.open_dir_nofollow(name) {
                Ok(sub) => sub.remove_open_dir_all(),
                Err(e) => Err(std::io::Error::other(format!(
                    "{folder}/{name} could not be opened without following a link: {e}"
                ))),
            },
            Ok(m) if !tree && m.is_file() => dir.remove_file(name),
            Ok(_) => Err(std::io::Error::other(format!(
                "{folder}/{name} is not what the bot left there"
            ))),
        };
        if let Err(e) = result {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Where a test makes the delete fail on purpose.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// A run of the bot is alive in the loop at the commit.
    LiveRun,
    /// A cleanup step fails.
    Step(BotDeleteStep),
    /// Revoking the knowledge sources of a removed mail account fails.
    RevokeMail,
}

#[cfg(test)]
static FAULTS: std::sync::Mutex<Vec<(BotId, Fault)>> = std::sync::Mutex::new(Vec::new());

/// Make `fault` happen for `bot` (tests); `clear` takes them all back.
#[cfg(test)]
pub(crate) fn inject(bot: BotId, fault: Fault, on: bool) {
    let mut faults = FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    faults.retain(|f| *f != (bot, fault));
    if on {
        faults.push((bot, fault));
    }
}

#[cfg_attr(
    not(test),
    allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)
)]
fn fault(bot: BotId, at: Fault) -> Result<(), FaultError> {
    #[cfg(test)]
    if FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&(bot, at))
    {
        return Err(FaultError);
    }
    let _ = (bot, at);
    Ok(())
}

/// An injected failure (tests).
#[derive(Debug)]
struct FaultError;

impl std::fmt::Display for FaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("injected failure")
    }
}

impl From<FaultError> for StorageError {
    fn from(_: FaultError) -> Self {
        Self::Conflict("injected: a run of the bot is alive".into())
    }
}

impl From<FaultError> for ApiError {
    fn from(e: FaultError) -> Self {
        Self::internal(e)
    }
}

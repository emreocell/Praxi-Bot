//! Deleting a bot for good: the preview, the refusals, what goes and what stays.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use memory_store::{Kind, NewMemory};
use parking_lot::Mutex;
use protocol::{BotId, ChatId, RunId};
use serde_json::{Value, json};

use super::{PROFILE, T, call, call_token_only, make_chat, setup};
use crate::bot_delete_routes::{BotDeleteStep, BotLock, Fault, inject};
use crate::email::{
    EmailAccountDto, EmailAccountInput, EmailControl, EmailControlError, EmailImapServer,
    EmailInboxNotDeleted, EmailProbeStatus, EmailProviderKind, EmailSendMode, EmailSmtpServer,
    EmailTestReport, ForgottenMailboxes, mailbox_exclusive, mailbox_heir,
};

fn bot_id(s: &str) -> BotId {
    BotId(s.parse().unwrap())
}

/// A finished run of `bot` in `chat`, with two messages the search can find by `word`.
fn history(t: &T, bot: &str, chat: &str, word: &str) -> RunId {
    let st = t.state.storage();
    let run = RunId::new();
    let chat_id = ChatId(chat.parse().unwrap());
    st.insert_run(&storage::Run {
        id: run,
        user_id: t.state.user_id(),
        bot_id: bot_id(bot),
        chat_id: Some(chat_id),
        kind: "turn".into(),
        state: "succeeded".into(),
        error: None,
        created_at_ms: 10,
        updated_at_ms: 10,
        parent_run_id: None,
        spec: None,
        computer_target: None,
        computer_node: None,
        computer_node_name: None,
        paused_at_ms: None,
        pause_reason: None,
    })
    .unwrap();
    for role in ["user", "assistant"] {
        let m = storage::Message {
            id: uuid::Uuid::now_v7(),
            user_id: t.state.user_id(),
            chat_id,
            run_id: Some(run),
            role: role.into(),
            content: format!("{word} from the {role}"),
            created_at_ms: 11,
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        };
        st.insert_message(&m).unwrap();
        t.state.memory_store().unwrap().index_message(&m).unwrap();
    }
    // The run's event log and the chat's attachments are files.
    let log = storage::event_log_path(&t.state.data_dir(), run);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "{}\n").unwrap();
    run
}

async fn schedule(t: &T, bot: &str, name: &str) {
    let body = json!({"bot_id": bot, "name": name, "spec": {"every": {"seconds": 3600}},
        "payload": {"kind": "agent_prompt", "text": "hi"}});
    let r = call(t, Method::POST, "/v1/schedules", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
}

fn delete_path(bot: &str) -> String {
    format!("/v1/bots/{bot}")
}

fn preview_path(bot: &str) -> String {
    format!("/v1/bots/{bot}/delete-preview")
}

#[tokio::test]
async fn the_preview_counts_what_would_go_and_changes_nothing() {
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    history(&t, &bot, &chat, "quokka");
    schedule(&t, &bot, "daily").await;
    let memory = t.state.memory_store().unwrap();
    memory
        .add(
            &NewMemory::new(
                t.state.user_id(),
                Some(bot_id(&bot)),
                Kind::Bot,
                "Likes tea",
            ),
            "t",
        )
        .unwrap();

    let r = call(&t, Method::GET, &preview_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let p = r.json();
    assert_eq!(p["bot_id"], bot);
    assert_eq!(p["name"], "Tester");
    assert_eq!(
        (&p["chats"], &p["messages"], &p["runs"]),
        (&json!(1), &json!(2), &json!(1))
    );
    assert_eq!(
        (&p["schedules"], &p["memories"], &p["missions"]),
        (&json!(1), &json!(1), &json!(0))
    );
    assert_eq!(
        (
            &p["active_runs"],
            &p["running_missions"],
            &p["waiting_missions"]
        ),
        (&json!(0), &json!(0), &json!(0))
    );
    assert_eq!(p["grants"], 0);
    assert_eq!(p["channels"], json!([]));
    assert_eq!(p["mailboxes"], json!([]));

    // Nothing was deleted by looking.
    assert_eq!(
        call(&t, Method::GET, &format!("/v1/bots/{bot}"), None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        call(&t, Method::GET, &preview_path(&bot), None)
            .await
            .json(),
        p
    );
}

#[tokio::test]
async fn the_preview_and_the_delete_need_the_owner_key_and_an_existing_bot() {
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    history(&t, &bot, &chat, "quokka");
    for (m, path) in [
        (Method::GET, preview_path(&bot)),
        (Method::DELETE, delete_path(&bot)),
    ] {
        let r = call_token_only(&t, m.clone(), &path, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {path}");
        assert_eq!(r.json()["error"]["code"], "owner_key_required");
    }
    // A refused delete changed nothing.
    assert_eq!(
        call(&t, Method::GET, &preview_path(&bot), None)
            .await
            .json()["chats"],
        1
    );
    let unknown = uuid::Uuid::now_v7();
    for (m, path) in [
        (Method::GET, preview_path(&unknown.to_string())),
        (Method::DELETE, delete_path(&unknown.to_string())),
    ] {
        assert_eq!(call(&t, m, &path, None).await.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        call(&t, Method::GET, &preview_path("not-a-uuid"), None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_bot_with_chats_and_runs_is_deleted_with_all_of_it_and_another_bot_is_untouched() {
    let t = setup();
    let (gone, gone_chat) = make_chat(&t).await;
    let (kept, kept_chat) = make_chat(&t).await;
    let gone_run = history(&t, &gone, &gone_chat, "quokka");
    let kept_run = history(&t, &kept, &kept_chat, "quokka");
    schedule(&t, &gone, "gone-daily").await;
    schedule(&t, &kept, "kept-daily").await;
    let memory = t.state.memory_store().unwrap();
    let user = t.state.user_id();
    let kept_memory = memory
        .add(
            &NewMemory::new(user, Some(bot_id(&kept)), Kind::Bot, "Kept likes tea"),
            "t",
        )
        .unwrap();
    let shared = memory
        .add(
            &NewMemory::new(user, None, Kind::Profile, "Owner likes tea"),
            "t",
        )
        .unwrap();
    memory
        .add(
            &NewMemory::new(user, Some(bot_id(&gone)), Kind::Bot, "Gone likes coffee"),
            "t",
        )
        .unwrap();
    let attachments = t.state.data_dir().join("attachments").join(&gone_chat);
    std::fs::create_dir_all(&attachments).unwrap();
    std::fs::write(attachments.join("a.txt"), "x").unwrap();

    let r = call(&t, Method::DELETE, &delete_path(&gone), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let report = r.json();
    assert_eq!(report["name"], "Tester");
    assert_eq!(
        (&report["chats"], &report["messages"], &report["runs"]),
        (&json!(1), &json!(2), &json!(1))
    );
    assert_eq!(
        (&report["schedules"], &report["memories"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(report["inboxes_deleted"], json!([]));
    assert_eq!(report["cleanup_pending"], json!([]));
    // Everything outside the database went: the cleanup marker is gone.
    assert!(
        t.state
            .storage()
            .bot_delete_cleanup(bot_id(&gone))
            .unwrap()
            .is_none()
    );

    // The bot, its chat and its history are gone.
    for path in [
        format!("/v1/bots/{gone}"),
        format!("/v1/chats/{gone_chat}/messages"),
        format!("/v1/runs/{gone_run}"),
    ] {
        assert_eq!(
            call(&t, Method::GET, &path, None).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        call(&t, Method::DELETE, &delete_path(&gone), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // What lived outside the main database went too: schedule, memory, search text, files.
    let id = bot_id(&gone);
    assert_eq!(
        t.state
            .schedule_store()
            .unwrap()
            .list_by_bot(id)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(memory.count_bot(user, id).unwrap(), 0);
    let found = memory.search_sessions(user, "quokka", 10).unwrap();
    assert_eq!(
        found.len(),
        2,
        "only the kept bot's two messages are searchable"
    );
    assert!(found.iter().all(|h| h.chat_id.to_string() == kept_chat));
    assert!(!storage::event_log_path(&t.state.data_dir(), gone_run).exists());
    assert!(!attachments.exists());
    // The audit log says it happened; its older rows stay.
    let audit = t.state.storage().list_audit(user).unwrap();
    let entry = audit.iter().find(|a| a.action == "bot.deleted").unwrap();
    assert_eq!(entry.detail["name"], "Tester");
    assert_eq!(entry.detail["chats"], 1);

    assert_kept_bot_untouched(
        &t,
        &kept,
        &kept_chat,
        kept_run,
        &[kept_memory.id, shared.id],
    )
    .await;
}

/// The bot that was not deleted has everything it had.
async fn assert_kept_bot_untouched(
    t: &T,
    kept: &str,
    kept_chat: &str,
    kept_run: RunId,
    memories: &[uuid::Uuid],
) {
    let memory = t.state.memory_store().unwrap();
    let r = call(t, Method::GET, &preview_path(kept), None).await;
    let p = r.json();
    assert_eq!(
        (
            &p["chats"],
            &p["messages"],
            &p["runs"],
            &p["schedules"],
            &p["memories"]
        ),
        (&json!(1), &json!(2), &json!(1), &json!(1), &json!(1))
    );
    assert!(memory.get(memories[0]).unwrap().is_some());
    assert!(memory.get(memories[1]).unwrap().is_some());
    assert!(storage::event_log_path(&t.state.data_dir(), kept_run).exists());
    assert_eq!(
        call(
            t,
            Method::GET,
            &format!("/v1/chats/{kept_chat}/messages"),
            None
        )
        .await
        .json()
        .as_array()
        .unwrap()
        .len(),
        2
    );
}

#[tokio::test]
async fn a_run_in_progress_refuses_the_delete_until_it_has_stopped() {
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    let run = history(&t, &bot, &chat, "quokka");
    t.state
        .storage()
        .update_run_state(run, "running", None)
        .unwrap();

    let p = call(&t, Method::GET, &preview_path(&bot), None)
        .await
        .json();
    assert_eq!(p["active_runs"], 1);
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("run in progress")
    );
    // Nothing was removed.
    let p = call(&t, Method::GET, &preview_path(&bot), None)
        .await
        .json();
    assert_eq!(
        (&p["chats"], &p["messages"], &p["runs"]),
        (&json!(1), &json!(2), &json!(1))
    );

    t.state
        .storage()
        .update_run_state(run, "cancelled", None)
        .unwrap();
    assert_eq!(
        call(&t, Method::DELETE, &delete_path(&bot), None)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_running_mission_refuses_the_delete_and_a_stopped_one_goes_with_the_bot() {
    let t = setup();
    t.state.enable_missions().unwrap();
    let (bot, chat) = make_chat(&t).await;
    let store = t.state.mission_store().unwrap();
    let mission = store
        .create(
            &missions::NewMission {
                user_id: t.state.user_id().to_string(),
                bot_id: bot.clone(),
                chat_id: chat.clone(),
                title: "Backlinks".into(),
                goal_text: "register me".into(),
                target_done: None,
                budgets: missions::Budgets::default(),
                items: vec![missions::NewItem {
                    key: "a.example".into(),
                    kind: "site".into(),
                    title: "a.example".into(),
                    payload: None,
                }],
            },
            storage::now_ms(),
        )
        .unwrap();

    let p = call(&t, Method::GET, &preview_path(&bot), None)
        .await
        .json();
    assert_eq!(
        (&p["missions"], &p["running_missions"]),
        (&json!(1), &json!(1))
    );
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("mission")
    );
    assert!(store.get(&mission.id).unwrap().is_some());

    // Waiting for a timer, a mail or a webhook: it would wake on its own, so it refuses too.
    store
        .set_state(
            &mission.id,
            missions::MissionState::Waiting,
            Some("test"),
            2,
        )
        .unwrap();
    let p = call(&t, Method::GET, &preview_path(&bot), None)
        .await
        .json();
    assert_eq!(
        (&p["running_missions"], &p["waiting_missions"]),
        (&json!(0), &json!(1))
    );
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("waiting")
    );
    assert!(store.get(&mission.id).unwrap().is_some());

    // Paused, but a session still holds a run.
    store
        .set_state(&mission.id, missions::MissionState::Running, None, 3)
        .unwrap();
    store
        .set_state(&mission.id, missions::MissionState::Paused, Some("test"), 4)
        .unwrap();
    store.set_lease(&mission.id, Some("run-1")).unwrap();
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(store.get(&mission.id).unwrap().is_some());
    // Every refusal left the bot able to work: it is not locked.
    assert!(!t.state.storage().is_retired(bot_id(&bot)));

    store.set_lease(&mission.id, None).unwrap();
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["missions"], 1);
    assert!(store.get(&mission.id).unwrap().is_none());
}

// ---- mailboxes ---------------------------------------------------------------

/// A mail control with a list of accounts that records what is removed at the provider, what
/// `forget_bot` is asked, and whether the bot was still in the database at that moment.
struct Mail {
    accounts: Mutex<Vec<EmailAccountDto>>,
    removed: Mutex<Vec<(String, bool)>>,
    forgotten: Mutex<Vec<String>>,
    provider_error: Mutex<Option<EmailControlError>>,
    storage: Mutex<Option<Arc<storage::Storage>>>,
    /// For every `forget_bot_mailboxes` call: did the bot's row still exist?
    bot_existed: Mutex<Vec<bool>>,
}

fn account(
    name: &str,
    provider: EmailProviderKind,
    owner: Option<&str>,
    shared_with: &[&str],
) -> EmailAccountDto {
    EmailAccountDto {
        name: name.into(),
        provider,
        address: format!("{name}@agentmail.to"),
        inbox_id: None,
        username: String::new(),
        imap: EmailImapServer::default(),
        smtp: EmailSmtpServer::default(),
        drafts_folder: "Drafts".into(),
        bot: owner.map(str::to_owned),
        allowed_senders: Vec::new(),
        shared_with: shared_with.iter().map(|s| (*s).to_owned()).collect(),
        default_for: Vec::new(),
        send_mode: EmailSendMode::default(),
        password_set: true,
        custom_ca: false,
    }
}

#[async_trait]
impl EmailControl for Mail {
    fn list(&self) -> Vec<EmailAccountDto> {
        self.accounts.lock().clone()
    }
    async fn add(&self, _: EmailAccountInput) -> Result<EmailAccountDto, EmailControlError> {
        Err(EmailControlError::Invalid("not in this test".into()))
    }
    async fn update(&self, name: &str, patch: Value) -> Result<EmailAccountDto, EmailControlError> {
        let mut all = self.accounts.lock();
        let a = all
            .iter_mut()
            .find(|a| a.name == name)
            .ok_or_else(|| EmailControlError::NotFound("x".into()))?;
        if let Some(list) = patch.get("shared_with").and_then(Value::as_array) {
            a.shared_with = list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        Ok(a.clone())
    }
    async fn remove(&self, name: &str) -> Result<(), EmailControlError> {
        self.remove_account(name, false).await
    }
    async fn remove_account(&self, name: &str, remote: bool) -> Result<(), EmailControlError> {
        if remote && let Some(e) = self.provider_error.lock().clone() {
            return Err(e);
        }
        self.removed.lock().push((name.to_owned(), remote));
        self.accounts.lock().retain(|a| a.name != name);
        Ok(())
    }
    async fn test_saved(&self, _: &str) -> Result<EmailTestReport, EmailControlError> {
        Ok(EmailTestReport {
            imap: EmailProbeStatus::Ok,
            smtp: EmailProbeStatus::Ok,
            api: None,
            ok: true,
        })
    }
    async fn test_draft(&self, _: EmailAccountInput) -> Result<EmailTestReport, EmailControlError> {
        self.test_saved("").await
    }
    async fn forget_bot(&self, bot: &str) -> Result<usize, EmailControlError> {
        Ok(self.forget_bot_mailboxes(bot, false).await?.removed.len())
    }
    /// The host's rules, simplified: an account only this bot could use goes (its inbox first
    /// when asked and still its alone), a shared one passes on, others are unlinked.
    async fn forget_bot_mailboxes(
        &self,
        bot: &str,
        delete_inboxes: bool,
    ) -> Result<ForgottenMailboxes, EmailControlError> {
        self.forgotten.lock().push(bot.to_owned());
        if let Some(st) = self.storage.lock().clone() {
            let id = BotId(bot.parse().unwrap());
            self.bot_existed
                .lock()
                .push(st.get_bot(id).unwrap().is_some());
        }
        let mut out = ForgottenMailboxes::default();
        let error = self.provider_error.lock().clone();
        let mut keep = Vec::new();
        for mut a in self.accounts.lock().drain(..) {
            let owned = a
                .bot
                .as_deref()
                .is_some_and(|b| b.eq_ignore_ascii_case(bot));
            if !owned {
                let n = a.shared_with.len() + a.default_for.len();
                a.shared_with.retain(|b| !b.eq_ignore_ascii_case(bot));
                a.default_for.retain(|b| !b.eq_ignore_ascii_case(bot));
                if n != a.shared_with.len() + a.default_for.len() {
                    out.kept.push(a.name.clone());
                }
                keep.push(a);
                continue;
            }
            if let Some(heir) = mailbox_heir(&a.shared_with, bot).map(str::to_owned) {
                a.shared_with
                    .retain(|b| *b != heir && !b.eq_ignore_ascii_case(bot));
                a.bot = Some(heir);
                out.kept.push(a.name.clone());
                keep.push(a);
                continue;
            }
            if delete_inboxes
                && a.provider == EmailProviderKind::Agentmail
                && mailbox_exclusive(&a.shared_with, &a.default_for, bot)
            {
                if let Some(e) = &error {
                    out.inboxes_not_deleted.push(EmailInboxNotDeleted {
                        account: a.name.clone(),
                        address: a.address.clone(),
                        reason: e.to_string(),
                    });
                    keep.push(a);
                    continue;
                }
                self.removed.lock().push((a.name.clone(), true));
                out.inboxes_deleted.push(a.address.clone());
            }
            out.removed.push(a.name.clone());
        }
        *self.accounts.lock() = keep;
        Ok(out)
    }
}

/// A bot with one chat, and a mail control with: its own `AgentMail` inbox, an IMAP account it
/// shares with `other`, an account of `other` it was given a share of, and an account of
/// nobody's it has nothing to do with.
async fn with_mail() -> (T, String, String, Arc<Mail>) {
    let t = setup();
    let other = uuid::Uuid::now_v7().to_string();
    let mail = Arc::new(Mail {
        accounts: Mutex::new(Vec::new()),
        removed: Mutex::new(Vec::new()),
        forgotten: Mutex::new(Vec::new()),
        provider_error: Mutex::new(None),
        storage: Mutex::new(Some(t.state.storage_arc())),
        bot_existed: Mutex::new(Vec::new()),
    });
    t.state.set_email(mail.clone());
    let (bot, _chat) = make_chat(&t).await;
    *mail.accounts.lock() = vec![
        account("praxi", EmailProviderKind::Agentmail, Some(&bot), &[]),
        account("team", EmailProviderKind::ImapSmtp, Some(&bot), &[&other]),
        account("theirs", EmailProviderKind::ImapSmtp, Some(&other), &[&bot]),
        account("unrelated", EmailProviderKind::ImapSmtp, Some(&other), &[]),
    ];
    (t, bot, other, mail)
}

#[tokio::test]
async fn the_preview_lists_the_mailboxes_and_what_becomes_of_each() {
    let (t, bot, other, mail) = with_mail().await;
    // An inbox another bot lists as its default is not offered for deletion at the provider.
    let mut listed = account("listed", EmailProviderKind::Agentmail, Some(&bot), &[]);
    listed.default_for = vec![other.to_uppercase()];
    mail.accounts.lock().push(listed);
    let p = call(&t, Method::GET, &preview_path(&bot), None)
        .await
        .json();
    let boxes = p["mailboxes"].as_array().unwrap();
    let find = |n: &str| boxes.iter().find(|m| m["name"] == n).cloned();
    let own = find("praxi").unwrap();
    assert_eq!(own["fate"], "removed");
    assert_eq!(own["provider"], "agentmail");
    assert_eq!(own["address"], "praxi@agentmail.to");
    assert_eq!(own["inbox_deletable"], true);
    let shared = find("team").unwrap();
    assert_eq!(
        (&shared["fate"], &shared["inbox_deletable"]),
        (&json!("handed_over"), &json!(false))
    );
    assert_eq!(find("theirs").unwrap()["fate"], "unlinked");
    let listed = find("listed").unwrap();
    assert_eq!(
        (&listed["fate"], &listed["inbox_deletable"]),
        (&json!("removed"), &json!(false))
    );
    assert!(
        find("unrelated").is_none(),
        "an account of another bot is not this bot's"
    );
}

#[tokio::test]
async fn the_agentmail_inbox_is_deleted_at_the_provider_only_when_asked_and_only_after_the_bot_is_gone()
 {
    let (t, bot, _other, mail) = with_mail().await;
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["inboxes_deleted"], json!([]));
    assert_eq!(r.json()["mailboxes_removed"], 1);
    assert_eq!(r.json()["mailboxes_kept"], 2);
    assert!(
        mail.removed.lock().is_empty(),
        "the default never deletes an inbox"
    );
    assert_eq!(
        *mail.forgotten.lock(),
        vec![bot.clone()],
        "the links are cleaned"
    );

    let (t, bot, _other, mail) = with_mail().await;
    let r = call(
        &t,
        Method::DELETE,
        &format!("{}?delete_inbox=true", delete_path(&bot)),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["inboxes_deleted"], json!(["praxi@agentmail.to"]));
    // Only the inbox only this bot used; the shared IMAP account is never touched remotely.
    assert_eq!(*mail.removed.lock(), vec![("praxi".to_owned(), true)]);
    // The provider was asked last, after the bot's rows were committed away.
    assert_eq!(*mail.bot_existed.lock(), vec![false]);
}

#[tokio::test]
async fn a_provider_failure_leaves_the_bot_deleted_reports_the_inbox_and_the_same_request_retries()
{
    let (t, bot, _other, mail) = with_mail().await;
    *mail.provider_error.lock() = Some(EmailControlError::Upstream("provider down".into()));
    let path = format!("{}?delete_inbox=true", delete_path(&bot));
    let r = call(&t, Method::DELETE, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let report = r.json();
    assert_eq!(report["inboxes_deleted"], json!([]));
    assert_eq!(
        report["inboxes_not_deleted"][0]["address"],
        "praxi@agentmail.to"
    );
    assert_eq!(report["cleanup_pending"], json!(["mailboxes"]));
    // The bot is gone here; its account stays (bound to the gone bot) for the retry.
    assert_eq!(
        call(&t, Method::GET, &format!("/v1/bots/{bot}"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert!(mail.accounts.lock().iter().any(|a| a.name == "praxi"));
    assert!(
        t.state
            .storage()
            .bot_delete_cleanup(bot_id(&bot))
            .unwrap()
            .is_some()
    );
    // Once the provider answers, the same request finishes it (the choice was remembered).
    *mail.provider_error.lock() = None;
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["inboxes_deleted"], json!(["praxi@agentmail.to"]));
    assert_eq!(r.json()["cleanup_pending"], json!([]));
    assert!(!mail.accounts.lock().iter().any(|a| a.name == "praxi"));
    assert!(
        t.state
            .storage()
            .bot_delete_cleanup(bot_id(&bot))
            .unwrap()
            .is_none()
    );
    // Nothing is left: now it is simply not found.
    assert_eq!(
        call(&t, Method::DELETE, &delete_path(&bot), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

// ---- order of operations ------------------------------------------------------------

/// A bot with history, a memory, a schedule, an attachment file and a mailbox, to see that a
/// refused delete leaves every piece of it.
async fn busy_bot() -> (T, String, String, RunId, Arc<Mail>) {
    let (t, bot, _other, mail) = with_mail().await;
    let r = call(
        &t,
        Method::POST,
        "/v1/chats",
        Some(json!({ "bot_id": bot })),
    )
    .await;
    let chat = r.json()["id"].as_str().unwrap().to_owned();
    let run = history(&t, &bot, &chat, "quokka");
    schedule(&t, &bot, "daily").await;
    t.state
        .memory_store()
        .unwrap()
        .add(
            &NewMemory::new(
                t.state.user_id(),
                Some(bot_id(&bot)),
                Kind::Bot,
                "Likes tea",
            ),
            "t",
        )
        .unwrap();
    let attachments = t.state.data_dir().join("attachments").join(&chat);
    std::fs::create_dir_all(&attachments).unwrap();
    std::fs::write(attachments.join("a.txt"), "x").unwrap();
    (t, bot, chat, run, mail)
}

fn assert_all_still_there(t: &T, bot: &str, chat: &str, run: RunId, mail: &Mail) {
    let id = bot_id(bot);
    assert!(t.state.storage().get_bot(id).unwrap().is_some());
    assert_eq!(t.state.storage().bot_data_counts(id).unwrap().chats, 2);
    assert_eq!(
        t.state
            .memory_store()
            .unwrap()
            .count_bot(t.state.user_id(), id)
            .unwrap(),
        1
    );
    assert_eq!(
        t.state
            .schedule_store()
            .unwrap()
            .list_by_bot(id)
            .unwrap()
            .len(),
        1
    );
    assert!(storage::event_log_path(&t.state.data_dir(), run).exists());
    assert!(
        t.state
            .data_dir()
            .join("attachments")
            .join(chat)
            .join("a.txt")
            .exists()
    );
    assert_eq!(mail.accounts.lock().len(), 4);
    assert!(mail.removed.lock().is_empty(), "no inbox was deleted");
    assert!(t.state.storage().bot_delete_cleanup(id).unwrap().is_none());
    assert!(!t.state.storage().is_retired(id), "the bot works again");
}

/// A run that turns out to be alive at the commit (checked inside the write transaction): the
/// delete is refused and nothing at all was deleted, not even what lives outside the database,
/// and no inbox at the provider.
#[tokio::test]
async fn a_run_found_alive_at_the_commit_refuses_and_deletes_nothing_anywhere() {
    let (t, bot, chat, run, mail) = busy_bot().await;
    inject(bot_id(&bot), Fault::LiveRun, true);
    let r = call(
        &t,
        Method::DELETE,
        &format!("{}?delete_inbox=true", delete_path(&bot)),
        None,
    )
    .await;
    inject(bot_id(&bot), Fault::LiveRun, false);
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("run in progress")
    );
    assert_all_still_there(&t, &bot, &chat, run, &mail);
    assert!(mail.forgotten.lock().is_empty());
    // The refusal released the lock: the bot can start a chat again.
    let r = call(
        &t,
        Method::POST,
        "/v1/chats",
        Some(json!({ "bot_id": bot })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
}

/// A waiting mission refuses before anything is deleted, mailbox included.
#[tokio::test]
async fn a_waiting_mission_refuses_and_deletes_nothing_anywhere() {
    let (t, bot, chat, run, mail) = busy_bot().await;
    t.state.enable_missions().unwrap();
    let store = t.state.mission_store().unwrap();
    let m = store
        .create(
            &missions::NewMission {
                user_id: t.state.user_id().to_string(),
                bot_id: bot.clone(),
                chat_id: chat.clone(),
                title: "Wait".into(),
                goal_text: "wait for mail".into(),
                target_done: None,
                budgets: missions::Budgets::default(),
                items: Vec::new(),
            },
            storage::now_ms(),
        )
        .unwrap();
    store
        .set_state(&m.id, missions::MissionState::Waiting, None, 2)
        .unwrap();
    let r = call(
        &t,
        Method::DELETE,
        &format!("{}?delete_inbox=true", delete_path(&bot)),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert_all_still_there(&t, &bot, &chat, run, &mail);
    assert!(store.get(&m.id).unwrap().is_some());
    // Cancelled (what the dialog's "cancel it first" does), the delete goes through.
    store
        .set_state(&m.id, missions::MissionState::Cancelled, None, 3)
        .unwrap();
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["missions"], 1);
}

/// A cleanup step that fails after the commit: the bot is gone, the other steps ran, the report
/// names the step, the marker stays; the same request finishes it.
#[tokio::test]
async fn a_failing_cleanup_step_is_reported_and_the_same_request_finishes_it() {
    let (t, bot, chat, run, mail) = busy_bot().await;
    let id = bot_id(&bot);
    for step in [BotDeleteStep::Files, BotDeleteStep::Memory] {
        inject(id, Fault::Step(step), true);
    }
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let report = r.json();
    assert_eq!(report["cleanup_pending"], json!(["memory", "files"]));
    assert_eq!((&report["chats"], &report["runs"]), (&json!(2), &json!(1)));
    // The bot is gone; the steps that worked did their part, the failed ones left their part.
    assert!(t.state.storage().get_bot(id).unwrap().is_none());
    assert_eq!(report["schedules"], 1);
    assert!(storage::event_log_path(&t.state.data_dir(), run).exists());
    let memory = t.state.memory_store().unwrap();
    assert_eq!(memory.count_bot(t.state.user_id(), id).unwrap(), 1);
    assert_eq!(
        mail.forgotten.lock().len(),
        1,
        "the mailboxes still came last"
    );
    let left = t.state.storage().bot_delete_cleanup(id).unwrap().unwrap();
    assert_eq!(left.run_ids, vec![run]);

    for step in [BotDeleteStep::Files, BotDeleteStep::Memory] {
        inject(id, Fault::Step(step), false);
    }
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["cleanup_pending"], json!([]));
    assert_eq!(r.json()["memories"], 1);
    assert!(!storage::event_log_path(&t.state.data_dir(), run).exists());
    assert!(!t.state.data_dir().join("attachments").join(&chat).exists());
    assert_eq!(memory.count_bot(t.state.user_id(), id).unwrap(), 0);
    assert!(t.state.storage().bot_delete_cleanup(id).unwrap().is_none());
}

/// A cleanup left over when the daemon stopped is finished at the next start, without the
/// owner (the gone bot has no "try again" left to click); one that still fails stays marked.
#[tokio::test]
async fn the_daemon_start_finishes_a_cleanup_a_delete_left() {
    let (t, bot, chat, run, _mail) = busy_bot().await;
    let id = bot_id(&bot);
    inject(id, Fault::Step(BotDeleteStep::Files), true);
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.json()["cleanup_pending"], json!(["files"]));
    // Still failing: tried, kept for later.
    assert_eq!(
        crate::bot_delete_routes::finish_pending_cleanups(&t.state).await,
        1
    );
    assert!(storage::event_log_path(&t.state.data_dir(), run).exists());
    assert!(t.state.storage().bot_delete_cleanup(id).unwrap().is_some());
    inject(id, Fault::Step(BotDeleteStep::Files), false);
    assert_eq!(
        crate::bot_delete_routes::finish_pending_cleanups(&t.state).await,
        0
    );
    assert!(!storage::event_log_path(&t.state.data_dir(), run).exists());
    assert!(!t.state.data_dir().join("attachments").join(&chat).exists());
    assert!(t.state.storage().bot_delete_cleanup(id).unwrap().is_none());
}

/// While a bot is being deleted nothing new starts for it: no chat, no mission, no mailbox share.
#[tokio::test]
async fn a_bot_being_deleted_gets_no_new_chat_mission_or_mailbox() {
    let (t, bot, other, mail) = with_mail().await;
    t.state.enable_missions().unwrap();
    let id = bot_id(&bot);
    let lock = BotLock::take(&t.state, id).unwrap();
    // A second delete of the same bot is refused while the first runs.
    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already being deleted")
    );
    let r = call(
        &t,
        Method::POST,
        "/v1/chats",
        Some(json!({ "bot_id": bot })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    let new_mission = missions::NewMission {
        user_id: t.state.user_id().to_string(),
        bot_id: bot.clone(),
        chat_id: "c".into(),
        title: "x".into(),
        goal_text: "x".into(),
        target_done: None,
        budgets: missions::Budgets::default(),
        items: Vec::new(),
    };
    let store = t.state.mission_store().unwrap();
    assert!(store.create(&new_mission, 1).is_err());
    let r = call(
        &t,
        Method::PUT,
        "/v1/email/accounts/unrelated",
        Some(json!({ "shared_with": [bot.to_uppercase()] })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        mail.accounts
            .lock()
            .iter()
            .all(|a| a.name != "unrelated" || a.shared_with.is_empty())
    );
    // Not deleted after all: the bot works again.
    drop(lock);
    let r = call(
        &t,
        Method::POST,
        "/v1/chats",
        Some(json!({ "bot_id": bot })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    store.create(&new_mission, 2).unwrap();
    let r = call(
        &t,
        Method::PUT,
        "/v1/email/accounts/unrelated",
        Some(json!({ "shared_with": [bot] })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let _ = other;
}

/// The signing secrets of the deleted bot's webhooks leave the secret store; another bot's stay.
#[tokio::test]
async fn the_webhook_secrets_of_the_bot_go_with_it() {
    let t = setup();
    t.state
        .set_secret_store(Arc::new(secret_store::MemoryStore::new()));
    let (gone, _) = make_chat(&t).await;
    let (kept, _) = make_chat(&t).await;
    let mut hooks = Vec::new();
    for bot in [&gone, &kept] {
        let r = call(
            &t,
            Method::POST,
            &format!("/v1/bots/{bot}/incoming-webhooks"),
            Some(json!({"name": "hook"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
        hooks.push(r.json()["webhook"]["id"].as_str().unwrap().to_owned());
    }
    let secret = |id: &str| {
        t.state
            .secret_store()
            .unwrap()
            .get(&format!("webhook/{id}"))
            .unwrap()
    };
    assert!(secret(&hooks[0]).is_some() && secret(&hooks[1]).is_some());
    let r = call(&t, Method::DELETE, &delete_path(&gone), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["webhooks"], 1);
    assert!(secret(&hooks[0]).is_none());
    assert!(secret(&hooks[1]).is_some());
}

/// A `runs` or `attachments` folder (or a chat's folder) that is a link is not followed: what it
/// points at stays, and the step is reported for a retry.
#[cfg(unix)]
#[tokio::test]
async fn the_file_cleanup_never_follows_a_link() {
    let t = setup();
    let (bot, chat) = make_chat(&t).await;
    let run = history(&t, &bot, &chat, "quokka");
    let outside = tempfile::tempdir().unwrap();
    // `attachments` itself is a link to a folder outside the data directory.
    std::fs::create_dir_all(outside.path().join(&chat)).unwrap();
    std::fs::write(outside.path().join(&chat).join("keep.txt"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path(), t.state.data_dir().join("attachments")).unwrap();
    // The run's log is a link to a file outside.
    let log = storage::event_log_path(&t.state.data_dir(), run);
    std::fs::remove_file(&log).unwrap();
    std::fs::write(outside.path().join("log.txt"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path().join("log.txt"), &log).unwrap();

    let r = call(&t, Method::DELETE, &delete_path(&bot), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["cleanup_pending"], json!(["files"]));
    assert!(outside.path().join(&chat).join("keep.txt").exists());
    assert!(outside.path().join("log.txt").exists());
    // The link itself is not removed either: the owner looks at it.
    assert!(log.symlink_metadata().is_ok());
}

#[tokio::test]
async fn the_last_bot_can_be_deleted_too() {
    let t = setup();
    let r = call(
        &t,
        Method::POST,
        "/v1/bots",
        Some(json!({"profile_toml": PROFILE})),
    )
    .await;
    let bot = r.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(
        call(&t, Method::DELETE, &delete_path(&bot), None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        call(&t, Method::GET, "/v1/bots", None).await.json(),
        json!([])
    );
}

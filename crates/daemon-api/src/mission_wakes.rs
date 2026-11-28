//! Daemon side of mission wakes: what `mission_wait` may rely on, and the look into the bot's
//! own mailbox that the supervisor's wake loop makes.
//!
//! Both only ever see the bot's OWN mail identity (bound to it or shared with it, the scope check
//! `email_send` and `email_wait_for` use) and only the incoming webhooks the owner created for
//! that bot. The mailbox is read with the same engine as `email_wait_for`
//! ([`connector_email::wait_for_mail`], one look instead of a polling loop): the filter is applied
//! locally, nothing the model wrote reaches the mail server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use builtin_tools::connector_email::inbound::{apply_to_message, sweep_reports};
use builtin_tools::connector_email::{
    MailHandle, WaitEnd, WaitFilter, WaitPlan, found_json, link_basis, wait_for_mail,
};
use missions::wake::{MailCheck, MailHit, MailProbe, WakeChecks};
use protocol::{BotId, RunId, UserId};
use storage::Storage;
use task_runtime::RunState;
use uuid::Uuid;

/// A mail a little older than the wait itself still counts (the clocks of the mail server and
/// of this computer differ, and a site may send the mail before the form's answer arrives).
const MAIL_SLACK: Duration = Duration::from_secs(10 * 60);

/// What `mission_wait` checks when it registers a mail or webhook wait.
pub(crate) struct DaemonWakeChecks {
    pub(crate) storage: Arc<Storage>,
    pub(crate) mail: Arc<MailHandle>,
    pub(crate) user_id: UserId,
}

impl WakeChecks for DaemonWakeChecks {
    fn mail_ready(&self, bot_id: &str) -> Result<(), String> {
        let connector = self
            .mail
            .get()
            .ok_or("no mail account is set up on this computer")?;
        if connector.scoped(Some(bot_id)).own_identities().is_empty() {
            return Err(
                "you have no mail identity of your own (see email_identities); a mail wait only reads your own inbox".into(),
            );
        }
        Ok(())
    }

    fn webhook(&self, bot_id: &str, trigger_id: &str) -> Result<String, String> {
        let not_yours = || "that is not a webhook the owner created for you".to_owned();
        let id = Uuid::parse_str(trigger_id.trim()).map_err(|_| not_yours())?;
        let bot = Uuid::parse_str(bot_id).map_err(|_| not_yours())?;
        let hook = self
            .storage
            .get_incoming_webhook(self.user_id, id)
            .map_err(|e| format!("the webhook could not be read: {e}"))?
            .filter(|h| h.bot_id == BotId(bot))
            .ok_or_else(not_yours)?;
        if !hook.enabled {
            return Err("that webhook is turned off".into());
        }
        Ok(hook.name)
    }

    fn child_run(&self, parent_run_id: &str, child_run_id: &str) -> Result<String, String> {
        let parent = Uuid::parse_str(parent_run_id.trim())
            .map_err(|_| "this is not a mission session run".to_string())?;
        let child = Uuid::parse_str(child_run_id.trim())
            .map_err(|_| "child_run must be a run id".to_string())?;
        let parent = self
            .storage
            .get_run(RunId(parent))
            .map_err(|e| format!("the session could not be read: {e}"))?
            .ok_or_else(|| "the session run no longer exists".to_string())?;
        if missions::mission_of_kind(&parent.kind).is_none() {
            return Err("this is not a mission session run".into());
        }
        let child = self
            .storage
            .get_run(RunId(child))
            .map_err(|e| format!("the child run could not be read: {e}"))?
            .ok_or_else(|| "that child run does not exist".to_string())?;
        if child.parent_run_id != Some(parent.id) {
            return Err("that run is not a child of this mission session".into());
        }
        if RunState::parse(&child.state).is_some_and(RunState::is_terminal) {
            return Err("that child run already finished".into());
        }
        Ok(child.kind)
    }
}

/// How often the inbox of one mailbox is searched for delivery reports (bounces).
const REPORT_SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);

/// One look into the bot's own inbox (and junk folder) for the wake loop.
pub(crate) struct OwnInboxProbe {
    pub(crate) mail: Arc<MailHandle>,
    /// When each mailbox was last searched for delivery reports.
    pub(crate) sweeps: parking_lot::Mutex<HashMap<String, Instant>>,
}

impl OwnInboxProbe {
    pub(crate) fn new(mail: Arc<MailHandle>) -> Self {
        Self {
            mail,
            sweeps: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Whether `account` is due for a search for delivery reports (and marks it done).
    fn sweep_due(&self, account: &str) -> bool {
        let mut sweeps = self.sweeps.lock();
        let due = sweeps
            .get(account)
            .is_none_or(|at| at.elapsed() >= REPORT_SWEEP_EVERY);
        if due {
            sweeps.insert(account.to_owned(), Instant::now());
        }
        due
    }
}

#[async_trait]
impl MailProbe for OwnInboxProbe {
    async fn check(&self, req: &MailCheck<'_>) -> Result<Option<MailHit>, String> {
        let connector = self.mail.get().ok_or("no mail account is set up")?;
        let scoped = connector.scoped(Some(req.bot_id));
        let acct = scoped.resolve(None).map_err(|e| e.to_string())?;
        // Own identity only, like `email_wait_for`: an account every bot may use (the owner's
        // own mailbox) is never read for a wake.
        if !scoped.owns(Some(&acct.name)) {
            return Err("the bot has no mail identity of its own".into());
        }
        let f = &req.filter;
        let filter = WaitFilter::new(
            f.from_domain.as_deref(),
            f.subject_contains.as_deref(),
            f.keyword.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        // Bounces come from a mail system, not from the person the wait is for: look for them
        // on their own (at most every few minutes per mailbox), whatever the filter says. A hard
        // bounce suppresses the address and a temporary failure is counted, before any wait or
        // session writes to it again.
        let guard = connector.guard();
        if self.sweep_due(&acct.name) {
            sweep_reports(&scoped, &guard, &acct.name).await;
        }
        let waited_ms = u64::try_from(req.now_ms.saturating_sub(req.registered_at_ms)).unwrap_or(0);
        // One look, no polling loop: the wake loop paces the looks itself.
        let plan = WaitPlan {
            timeout: Duration::ZERO,
            max_age: Duration::from_millis(waited_ms) + MAIL_SLACK,
            // The wait's own start (less the slack) is its watermark: an earlier mail is not it.
            since: Some(
                ((req.registered_at_ms / 1000)
                    - i64::try_from(MAIL_SLACK.as_secs()).unwrap_or(i64::MAX))
                .max(req.not_before_ms.div_euclid(1000) + 1),
            ),
            first_pause: Duration::from_secs(1),
            max_pause: Duration::from_secs(1),
        };
        match wait_for_mail(
            &scoped,
            Some(&acct.name),
            &filter,
            &plan,
            std::future::pending::<()>(),
        )
        .await
        {
            Ok(WaitEnd::Found(detail)) => {
                // No page is open, so no link is judged as belonging to the sign-up site: the
                // summary lists senders, subject and codes. The next session opens the site and
                // calls `email_wait_for`, which finds this mail at once and judges its links.
                let basis = link_basis(None, None);
                let owner_domains = connector.guard().settings().verification_domains();
                let mut summary = found_json(&detail, &acct.name, &basis, &owner_domains);
                // Match it to what this mailbox sent and act on a confident match (suppress a
                // sender who said STOP, count a temporary failure, note a follow-up date). What
                // the mail says is data: it only picks one of these fixed actions.
                let outcome = apply_to_message(&guard, &acct.name, &detail);
                // Only a notice about a mail THIS mission sent puts its wait off: not one for a
                // mail of another mission, and not one for a mail that belongs to no mission.
                let follow_up_at_ms = outcome
                    .follow_up_secs()
                    .filter(|_| outcome.mission_id.as_deref() == Some(req.mission_id))
                    .and_then(|secs| i64::try_from(secs).ok())
                    .and_then(|secs| secs.checked_mul(1000));
                if let Some(object) = summary.as_object_mut() {
                    object.insert("reply_class_hint".into(), serde_json::json!(outcome.class));
                    object.insert("inbound".into(), serde_json::json!(outcome));
                }
                Ok(Some(MailHit {
                    summary,
                    follow_up_at_ms,
                    message_id: detail
                        .message_id
                        .clone()
                        .or_else(|| detail.summary.id.clone()),
                }))
            }
            Ok(WaitEnd::TimedOut { .. } | WaitEnd::Cancelled) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

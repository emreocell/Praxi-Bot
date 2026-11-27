//! Calendar connector (`CalDAV`) as agent tools.
//!
//! - [`tools::LIST`] reads events in a time range (`Effect::Read`; the text is
//!   third-party data and is marked untrusted).
//! - [`tools::FREE_BUSY`] finds free slots across calendars (`Effect::Read`).
//! - [`tools::CREATE`] adds an event, optionally recurring and with attendees
//!   (`Effect::Write`); [`tools::UPDATE`] moves, renames or changes the
//!   attendees of one (`Effect::Write`). A call that names attendees (or edits an
//!   event that has some) sends mail to other people, so it goes to the approval
//!   gate the way `email_send` does.
//! - [`tools::DELETE`] removes one by `UID` (`Effect::Destructive`). Like the other
//!   tools that act outward it follows the permission level (lower levels ask, the
//!   Full level lets a trusted run through) and has no always-ask of its own.
//! - Invitations: when the server delivers them itself (`CalDAV` scheduling,
//!   RFC 6638) it does; otherwise they go out as e-mail with an `.ics` through
//!   an [`InviteMailer`] (the bot's e-mail connector). Without either, the
//!   event is saved and the answer says nobody was notified. Either way the owner's
//!   mail rules (opt-out list, daily limit) are checked through the mailer BEFORE
//!   the event is written, and what the server delivered is logged afterwards.
//! - Passwords are [`secret_store::SecretRef`]s resolved only when calling;
//!   TLS is required (plain HTTP only for loopback), redirects are refused and
//!   every connection goes through a URL policy ([`policy`]).

pub mod client;
pub mod freebusy;
pub mod ical;
pub mod policy;
pub mod rrule;
pub mod tools;

use std::fmt::Write as _;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use secret_store::SecretStore;
use serde::Serialize;
use tool_registry::{Tool, ToolRegistry};
use web_tools::UrlPolicy;

pub use client::{CalendarAccount, CalendarClient, CalendarError, CalendarInfo};
pub use ical::{Event, When};
pub use policy::calendar_policy;
pub use tools::TOOL_NAMES;

/// Sends an iTIP message (an invitation or a cancellation) by e-mail, and applies the
/// owner's mail rules to every invitation, whoever delivers it.
#[async_trait]
pub trait InviteMailer: Send + Sync {
    /// May an invitation (or its update or cancellation) go to `to`? Called BEFORE the
    /// event is written, whether the calendar server or [`InviteMailer::send`] will
    /// deliver it: an opted-out address or a daily limit stops the change.
    ///
    /// # Errors
    /// The reason, in words the agent can read.
    async fn check(&self, bot: Option<&str>, to: &[String]) -> Result<(), String>;

    /// Send `ics` (an iCalendar document whose `METHOD` is `method`) to `to`
    /// as a mail with that subject and text. `bot` is the bot that is acting.
    ///
    /// # Errors
    /// A message the agent can read: no usable e-mail account, a refusal, a network failure.
    async fn send(
        &self,
        bot: Option<&str>,
        to: &[String],
        subject: &str,
        body: &str,
        ics: &str,
        method: &str,
    ) -> Result<(), String>;

    /// Note that the calendar server itself told `to` about the event `subject`
    /// (from `organizer`), so the send log and the daily limit count it.
    fn record_server_delivery(
        &self,
        bot: Option<&str>,
        organizer: &str,
        to: &[String],
        subject: &str,
    );
}

/// How the people on an event were told about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Delivery {
    /// `none` (nobody to tell), `calendar_server` (the server sends them),
    /// `email` (sent from the bot's mailbox) or `not_sent`.
    pub method: &'static str,
    /// Addresses an e-mail went to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sent_to: Vec<String>,
    /// What the agent should tell the user.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Delivery {
    const fn plain(method: &'static str) -> Self {
        Self {
            method,
            sent_to: Vec::new(),
            note: String::new(),
        }
    }
}

/// An event that was written.
#[derive(Debug, Clone, Serialize)]
pub struct Written {
    /// Its `UID`.
    pub uid: String,
    /// Invitations.
    pub invitations: Delivery,
}

/// An event that was deleted.
#[derive(Debug, Clone, Serialize)]
pub struct Removed {
    /// `false` when there was no event with that `UID`.
    pub deleted: bool,
    /// Who was told it is cancelled.
    pub invitations: Delivery,
}

/// The configured calendars. Accounts can be replaced while the daemon runs.
pub struct CalendarConnector {
    clients: RwLock<Arc<Vec<CalendarClient>>>,
    mailer: RwLock<Option<Arc<dyn InviteMailer>>>,
    policy: Arc<dyn UrlPolicy>,
}

impl Default for CalendarConnector {
    fn default() -> Self {
        Self::empty()
    }
}

fn build(
    accounts: Vec<CalendarAccount>,
    store: &Arc<dyn SecretStore>,
    policy: &Arc<dyn UrlPolicy>,
) -> (Vec<CalendarClient>, Vec<String>) {
    let mut clients = Vec::new();
    let mut problems = Vec::new();
    for a in accounts {
        if clients.iter().any(|c: &CalendarClient| c.name() == a.name) {
            problems.push(format!("calendar `{}` is listed twice", a.name));
            continue;
        }
        match CalendarClient::with_policy(a, Arc::clone(store), Arc::clone(policy)) {
            Ok(c) => clients.push(c),
            Err(e) => problems.push(e.to_string()),
        }
    }
    (clients, problems)
}

impl CalendarConnector {
    /// A connector with no calendars; fill it with [`CalendarConnector::replace_accounts`].
    #[must_use]
    pub fn empty() -> Self {
        Self::with_policy(calendar_policy(&[]))
    }

    /// Like [`CalendarConnector::empty`], dialling only what `policy` allows.
    #[must_use]
    pub fn with_policy(policy: Arc<dyn UrlPolicy>) -> Self {
        Self {
            clients: RwLock::new(Arc::new(Vec::new())),
            mailer: RwLock::new(None),
            policy,
        }
    }

    /// Build clients for `accounts`; an account that fails validation is
    /// skipped and reported in the second value.
    #[must_use]
    pub fn new(
        accounts: Vec<CalendarAccount>,
        store: &Arc<dyn SecretStore>,
    ) -> (Self, Vec<String>) {
        let c = Self::empty();
        let problems = c.replace_accounts(accounts, store);
        (c, problems)
    }

    /// Replace every calendar at once; returns what was skipped and why.
    pub fn replace_accounts(
        &self,
        accounts: Vec<CalendarAccount>,
        store: &Arc<dyn SecretStore>,
    ) -> Vec<String> {
        let (clients, problems) = build(accounts, store, &self.policy);
        *self.clients.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(clients);
        problems
    }

    /// Where invitations go when the calendar server does not send them.
    pub fn set_mailer(&self, mailer: Arc<dyn InviteMailer>) {
        *self.mailer.write().unwrap_or_else(PoisonError::into_inner) = Some(mailer);
    }

    fn clients(&self) -> Arc<Vec<CalendarClient>> {
        Arc::clone(&self.clients.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn mailer(&self) -> Option<Arc<dyn InviteMailer>> {
        self.mailer
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Names of the usable calendars.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.clients().iter().map(|c| c.name().to_owned()).collect()
    }

    /// The named calendar, or the only one when `name` is `None`.
    ///
    /// # Errors
    /// [`CalendarError::Invalid`] when the name is unknown or ambiguous.
    pub fn pick(&self, name: Option<&str>) -> Result<CalendarClient, CalendarError> {
        let all = self.clients();
        let names = || {
            all.iter()
                .map(CalendarClient::name)
                .collect::<Vec<_>>()
                .join(", ")
        };
        match (name, all.as_slice()) {
            (Some(n), cs) => cs.iter().find(|c| c.name() == n).cloned().ok_or_else(|| {
                CalendarError::Invalid(format!("no calendar named `{n}`; have: {}", names()))
            }),
            (None, [only]) => Ok(only.clone()),
            (None, []) => Err(CalendarError::Invalid("no calendar is configured".into())),
            (None, _) => Err(CalendarError::Invalid(format!(
                "several calendars are configured; pass `account` ({})",
                names()
            ))),
        }
    }

    /// Every configured calendar, or the named ones.
    ///
    /// # Errors
    /// [`CalendarError::Invalid`] for an unknown name or when none is configured.
    pub fn pick_many(
        &self,
        names: Option<&[String]>,
    ) -> Result<Vec<CalendarClient>, CalendarError> {
        match names {
            None => {
                let all = self.clients();
                if all.is_empty() {
                    return Err(CalendarError::Invalid("no calendar is configured".into()));
                }
                Ok(all.to_vec())
            }
            Some(list) => list.iter().map(|n| self.pick(Some(n))).collect(),
        }
    }

    /// Apply the owner's mail rules to an invitation (or a change, or a cancellation)
    /// for `to`: refused here, before anything is written, when the rules say no. No
    /// mailer means no rules to apply.
    async fn screen(&self, bot: Option<&str>, to: &[String]) -> Result<(), CalendarError> {
        if to.is_empty() {
            return Ok(());
        }
        match self.mailer() {
            Some(m) => m.check(bot, to).await.map_err(CalendarError::Invalid),
            None => Ok(()),
        }
    }

    /// Create an event and tell its attendees.
    ///
    /// # Errors
    /// See [`CalendarError`]; also when the owner's mail rules forbid writing to an attendee
    /// (nothing is written then). Failing to deliver an invitation is not an error: it is in [`Written::invitations`].
    pub async fn create_event(
        &self,
        client: &CalendarClient,
        bot: Option<&str>,
        spec: &ical::EventSpec<'_>,
    ) -> Result<Written, CalendarError> {
        self.screen(bot, spec.attendees).await?;
        let made = client.create(spec).await?;
        let invitations = if spec.attendees.is_empty() {
            Delivery::plain("none")
        } else {
            let event = ical::parse_events(&made.ics)
                .into_iter()
                .next()
                .unwrap_or_else(|| empty_event(&made.uid));
            self.invite(
                client,
                bot,
                &made.ics,
                &event,
                spec.attendees,
                &[],
                "Invitation",
            )
            .await
        };
        Ok(Written {
            uid: made.uid,
            invitations,
        })
    }

    /// Change an event and tell the people it affects. `None` when there is no such event.
    ///
    /// # Errors
    /// See [`CalendarError`]; also when the owner's mail rules forbid writing to someone the
    /// change would tell (nothing is written then). Failing to deliver an invitation is not an
    /// error: it is in [`Written::invitations`].
    pub async fn update_event(
        &self,
        client: &CalendarClient,
        bot: Option<&str>,
        uid: &str,
        patch: &ical::Patch,
    ) -> Result<Option<Written>, CalendarError> {
        let Some(stored) = client.fetch(uid).await? else {
            return Ok(None);
        };
        let e = CalendarClient::plan_update(&stored, patch)?;
        let tell: Vec<String> = if e.material {
            e.attendees.clone()
        } else {
            Vec::new()
        };
        let mut told = tell.clone();
        told.extend(e.removed.iter().filter(|r| !tell.contains(r)).cloned());
        self.screen(bot, &told).await?;
        client.put_edited(stored, &e).await?;
        let event = ical::parse_events(&e.ics)
            .into_iter()
            .next()
            .unwrap_or_else(|| empty_event(uid));
        let invitations = if told.is_empty() {
            Delivery::plain("none")
        } else {
            self.invite(
                client,
                bot,
                &e.ics,
                &event,
                &tell,
                &e.removed,
                "Updated invitation",
            )
            .await
        };
        Ok(Some(Written {
            uid: e.uid,
            invitations,
        }))
    }

    /// Delete an event. When the calendar server cancels it for the attendees (it
    /// does that only for an event this account organises), the owner's mail rules
    /// apply to them first.
    ///
    /// # Errors
    /// See [`CalendarError`]; also when the owner's mail rules forbid writing to an attendee
    /// the server would cancel for (nothing is deleted then).
    pub async fn delete_event(
        &self,
        client: &CalendarClient,
        bot: Option<&str>,
        uid: &str,
    ) -> Result<Removed, CalendarError> {
        let Some(found) = client.fetch(uid).await? else {
            return Ok(Removed {
                deleted: false,
                invitations: Delivery::plain("none"),
            });
        };
        let event = found.event();
        let mine = client.organizer();
        let told: Vec<String> = match (&event, &mine) {
            (Some(e), Some(me)) if e.organizer.eq_ignore_ascii_case(me) => {
                e.attendees.iter().map(|a| a.email.clone()).collect()
            }
            _ => Vec::new(),
        };
        let server_cancels = !told.is_empty() && client.supports_scheduling().await;
        if server_cancels {
            self.screen(bot, &told).await?;
        }
        let deleted = client.remove(found).await?;
        let invitations = match (deleted, told.is_empty(), server_cancels) {
            (true, false, true) => {
                if let Some(m) = self.mailer() {
                    let summary = event.as_ref().map_or("", |e| e.summary.as_str());
                    m.record_server_delivery(
                        bot,
                        mine.as_deref().unwrap_or_default(),
                        &told,
                        &subject_line("Canceled", summary),
                    );
                }
                Delivery {
                    note: "The calendar server sends the cancellations.".into(),
                    ..Delivery::plain("calendar_server")
                }
            }
            (true, false, false) => Delivery {
                note: "The event is deleted. This calendar server does not send cancellations, \
                       so the attendees were not told."
                    .into(),
                ..Delivery::plain("not_sent")
            },
            _ => Delivery::plain("none"),
        };
        Ok(Removed {
            deleted,
            invitations,
        })
    }

    #[allow(clippy::too_many_arguments)] // one call site per tool; a struct would only rename them
    async fn invite(
        &self,
        client: &CalendarClient,
        bot: Option<&str>,
        ics: &str,
        event: &Event,
        request_to: &[String],
        cancel_to: &[String],
        subject_prefix: &str,
    ) -> Delivery {
        if client.supports_scheduling().await {
            // The server sends these mails, so they must still show in the owner's send log.
            if let Some(m) = self.mailer() {
                let all: Vec<String> = request_to.iter().chain(cancel_to).cloned().collect();
                m.record_server_delivery(
                    bot,
                    client.organizer().as_deref().unwrap_or_default(),
                    &all,
                    &subject_line(subject_prefix, &event.summary),
                );
            }
            return Delivery {
                note: "The calendar server sends the invitations.".into(),
                ..Delivery::plain("calendar_server")
            };
        }
        let Some(mailer) = self.mailer() else {
            return Delivery {
                note: "No e-mail account is available to send invitations, so nobody was told. \
                       The event is saved with its attendees."
                    .into(),
                ..Delivery::plain("not_sent")
            };
        };
        let mut sent = Vec::new();
        if !request_to.is_empty() {
            let body = invite_text(event, "invited you to");
            let subject = subject_line(subject_prefix, &event.summary);
            let ics = ical::itip(ics, "REQUEST", None);
            if let Err(e) = mailer
                .send(bot, request_to, &subject, &body, &ics, "REQUEST")
                .await
            {
                return not_sent(&sent, &e);
            }
            sent.extend_from_slice(request_to);
        }
        if !cancel_to.is_empty() {
            let body = invite_text(event, "removed you from");
            let subject = subject_line("Canceled", &event.summary);
            let ics = ical::itip(ics, "CANCEL", Some(cancel_to));
            if let Err(e) = mailer
                .send(bot, cancel_to, &subject, &body, &ics, "CANCEL")
                .await
            {
                return not_sent(&sent, &e);
            }
            sent.extend_from_slice(cancel_to);
        }
        Delivery {
            sent_to: sent,
            ..Delivery::plain("email")
        }
    }
}

/// `text` on one line: every control character (a line break included) becomes a space,
/// runs of white space collapse. An event title is third-party text and goes into a mail
/// header, where a line break would split it and fail the send after the event is saved.
fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_control() || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Longest subject line of an invitation, in characters.
const MAX_SUBJECT: usize = 200;

/// `Invitation: Dentist`, safe as a mail subject.
fn subject_line(prefix: &str, summary: &str) -> String {
    format!("{prefix}: {}", one_line(summary))
        .chars()
        .take(MAX_SUBJECT)
        .collect()
}

fn not_sent(already: &[String], why: &str) -> Delivery {
    Delivery {
        sent_to: already.to_vec(),
        note: format!("The event is saved, but an invitation could not be e-mailed: {why}"),
        ..Delivery::plain("not_sent")
    }
}

fn empty_event(uid: &str) -> Event {
    Event {
        uid: uid.to_owned(),
        summary: String::new(),
        start: String::new(),
        end: String::new(),
        location: String::new(),
        description: String::new(),
        recurrence: String::new(),
        organizer: String::new(),
        attendees: Vec::new(),
        busy: true,
        span: None,
    }
}

fn invite_text(e: &Event, what: &str) -> String {
    let who = if e.organizer.is_empty() {
        "Someone"
    } else {
        e.organizer.as_str()
    };
    let mut s = format!(
        "{who} {what} \"{}\".\n\nWhen: {} to {} (UTC)\n",
        one_line(&e.summary),
        e.start,
        e.end
    );
    if !e.location.is_empty() {
        let _ = writeln!(s, "Where: {}", one_line(&e.location));
    }
    if !e.recurrence.is_empty() {
        let _ = writeln!(s, "Repeats: {}", one_line(&e.recurrence));
    }
    s.push_str("\nThe invitation is attached; open it in your calendar to answer.\n");
    s
}

/// Register the calendar tools; returns how many were added.
#[must_use]
pub fn register(registry: &ToolRegistry, connector: &Arc<CalendarConnector>) -> usize {
    tools::tools(connector)
        .into_iter()
        .filter(|t: &Arc<dyn Tool>| registry.register(t.clone()))
        .count()
}

#[cfg(test)]
mod tests;

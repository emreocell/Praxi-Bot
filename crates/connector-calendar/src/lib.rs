//! Calendar connector (`CalDAV`) as agent tools.
//!
//! - [`tools::LIST`] reads events in a time range (`Effect::Read`; the text is
//!   third-party data and is marked untrusted).
//! - [`tools::FREE_BUSY`] finds free slots across calendars (`Effect::Read`).
//! - [`tools::CREATE`] adds an event, optionally recurring and with attendees
//!   (`Effect::Write`); [`tools::UPDATE`] moves, renames or changes the
//!   attendees of one (`Effect::Write`). A call that names attendees always
//!   asks the user: it sends invitations to other people.
//! - [`tools::DELETE`] removes one by `UID` (`Effect::Destructive`).
//! - Invitations: when the server delivers them itself (`CalDAV` scheduling,
//!   RFC 6638) it does; otherwise they go out as e-mail with an `.ics` through
//!   an [`InviteMailer`] (the bot's e-mail connector). Without either, the
//!   event is saved and the answer says nobody was notified.
//! - Passwords are [`secret_store::SecretRef`]s resolved only when calling;
//!   TLS is required (plain HTTP only for loopback) and redirects are refused.

pub mod client;
pub mod freebusy;
pub mod ical;
pub mod rrule;
pub mod tools;

use std::fmt::Write as _;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use secret_store::SecretStore;
use serde::Serialize;
use tool_registry::{Tool, ToolRegistry};

pub use client::{CalendarAccount, CalendarClient, CalendarError, CalendarInfo};
pub use ical::{Event, When};
pub use tools::TOOL_NAMES;

/// Sends an iTIP message (an invitation or a cancellation) by e-mail.
#[async_trait]
pub trait InviteMailer: Send + Sync {
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

/// The configured calendars. Accounts can be replaced while the daemon runs.
pub struct CalendarConnector {
    clients: RwLock<Arc<Vec<CalendarClient>>>,
    mailer: RwLock<Option<Arc<dyn InviteMailer>>>,
}

impl Default for CalendarConnector {
    fn default() -> Self {
        Self::empty()
    }
}

fn build(
    accounts: Vec<CalendarAccount>,
    store: &Arc<dyn SecretStore>,
) -> (Vec<CalendarClient>, Vec<String>) {
    let mut clients = Vec::new();
    let mut problems = Vec::new();
    for a in accounts {
        if clients.iter().any(|c: &CalendarClient| c.name() == a.name) {
            problems.push(format!("calendar `{}` is listed twice", a.name));
            continue;
        }
        match CalendarClient::new(a, Arc::clone(store)) {
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
        Self {
            clients: RwLock::new(Arc::new(Vec::new())),
            mailer: RwLock::new(None),
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
        let (clients, problems) = build(accounts, store);
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

    /// Create an event and tell its attendees.
    ///
    /// # Errors
    /// See [`CalendarError`]. Failing to deliver an invitation is not an error: it is in [`Written::invitations`].
    pub async fn create_event(
        &self,
        client: &CalendarClient,
        bot: Option<&str>,
        spec: &ical::EventSpec<'_>,
    ) -> Result<Written, CalendarError> {
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
    /// See [`CalendarError`]. Failing to deliver an invitation is not an error: it is in [`Written::invitations`].
    pub async fn update_event(
        &self,
        client: &CalendarClient,
        bot: Option<&str>,
        uid: &str,
        patch: &ical::Patch,
    ) -> Result<Option<Written>, CalendarError> {
        let Some(done) = client.update(uid, patch).await? else {
            return Ok(None);
        };
        let e = done.edited;
        let event = ical::parse_events(&e.ics)
            .into_iter()
            .next()
            .unwrap_or_else(|| empty_event(uid));
        let tell: Vec<String> = if e.material {
            e.attendees.clone()
        } else {
            Vec::new()
        };
        let invitations = if tell.is_empty() && e.removed.is_empty() {
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
            let subject = format!("{subject_prefix}: {}", event.summary);
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
            let subject = format!("Canceled: {}", event.summary);
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
        e.summary, e.start, e.end
    );
    if !e.location.is_empty() {
        let _ = writeln!(s, "Where: {}", e.location);
    }
    if !e.recurrence.is_empty() {
        let _ = writeln!(s, "Repeats: {}", e.recurrence);
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

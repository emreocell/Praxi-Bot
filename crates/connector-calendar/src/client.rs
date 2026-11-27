//! `CalDAV` client: REPORT (query), PUT (create) and DELETE over one calendar
//! collection. TLS is required (plain HTTP only for loopback), redirects are
//! never followed (the password must not travel to another host), and an
//! `href` the server returns is used only when it stays on the configured origin.
//! Every connection goes through a [`web_tools::UrlPolicy`] (see [`crate::policy`]).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::Event as Xml;
use reqwest::{Method, StatusCode};
use secret_store::{SecretRef, SecretStore, resolve};
use serde::{Deserialize, Serialize};
use url::{Host, Url};
use web_tools::UrlPolicy;

use crate::ical::{self, Budget, Edited, Event, EventSpec, Patch, When};
use crate::policy::{GuardedResolver, calendar_policy, names_loopback};

/// Largest response body read from a server.
const MAX_BODY: usize = 4 << 20;
/// Most events one listing may expand to, however many the server sends.
const MAX_EXPANDED_EVENTS: usize = 10_000;
/// Most recurrence-rule steps one listing may spend.
const MAX_RULE_WORK: u64 = 20_000_000;
/// Longest event `UID` accepted from the agent.
const MAX_UID: usize = 255;
/// Services whose calendars live on several hosts of one domain (iCloud keeps them on
/// `pNN-caldav.icloud.com`). A discovery link may move between hosts of the same one of
/// these; for every other server it must stay on the host that was configured.
const PROVIDER_DOMAINS: [&str; 3] = ["icloud.com", "fastmail.com", "googleusercontent.com"];

/// One calendar the agent can use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarAccount {
    /// Short name the agent uses to pick the calendar (e.g. `"personal"`).
    pub name: String,
    /// URL of the calendar collection (not the server root).
    pub url: String,
    /// Login name.
    pub username: String,
    /// Secret-store reference of the (app) password.
    pub password: SecretRef,
    /// The address invitations are sent from (the organizer). Defaults to
    /// `username` when that looks like an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Per-request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    20
}

/// Why a calendar call failed. Never contains the password.
#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    /// Bad configuration or arguments.
    #[error("{0}")]
    Invalid(String),
    /// The secret could not be read.
    #[error("cannot read the calendar password from the secret store: {0}")]
    Credential(String),
    /// Network or HTTP failure.
    #[error("calendar server: {0}")]
    Server(String),
}

/// A configured account with its resolved base URL.
#[derive(Clone)]
pub struct CalendarClient {
    account: CalendarAccount,
    base: Url,
    store: Arc<dyn SecretStore>,
    http: reqwest::Client,
    policy: Arc<dyn UrlPolicy>,
}

impl CalendarClient {
    /// A client under the default [`calendar_policy`].
    ///
    /// # Errors
    /// [`CalendarError::Invalid`] for a bad URL or plain HTTP to a non-loopback host.
    pub fn new(
        account: CalendarAccount,
        store: Arc<dyn SecretStore>,
    ) -> Result<Self, CalendarError> {
        Self::with_policy(account, store, calendar_policy(&[]))
    }

    /// A client that only dials what `policy` allows.
    ///
    /// # Errors
    /// [`CalendarError::Invalid`] for a bad URL, plain HTTP to a host that is not this
    /// machine, or an address the policy refuses.
    pub fn with_policy(
        account: CalendarAccount,
        store: Arc<dyn SecretStore>,
        policy: Arc<dyn UrlPolicy>,
    ) -> Result<Self, CalendarError> {
        let invalid =
            |m: String| CalendarError::Invalid(format!("calendar `{}`: {m}", account.name));
        let mut base = Url::parse(&account.url).map_err(|e| invalid(format!("bad url: {e}")))?;
        let plain = match base.scheme() {
            "https" => false,
            "http" if names_loopback(&base) => true,
            _ => {
                return Err(invalid(
                    "the url must be https (plain http only for localhost)".into(),
                ));
            }
        };
        if !base.username().is_empty() || base.password().is_some() {
            return Err(invalid(
                "put the login in `username`/`password`, not in the url".into(),
            ));
        }
        policy
            .check_url(&base)
            .map_err(|e| invalid(e.to_string()))?;
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(account.timeout_secs.clamp(1, 120)))
            // A proxy would resolve names itself and skip the address checks.
            .no_proxy()
            .dns_resolver(Arc::new(GuardedResolver {
                policy: Arc::clone(&policy),
                port: base.port_or_known_default().unwrap_or(443),
                loopback_only: plain,
            }))
            .build()
            .map_err(|e| CalendarError::Server(e.to_string()))?;
        Ok(Self {
            account,
            base,
            store,
            http,
            policy,
        })
    }

    /// The account name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.account.name
    }

    async fn password(&self) -> Result<String, CalendarError> {
        let (store, r) = (Arc::clone(&self.store), self.account.password.clone());
        tokio::task::spawn_blocking(move || {
            resolve(store.as_ref(), &r).map(|v| v.expose().to_string())
        })
        .await
        .map_err(|e| CalendarError::Credential(e.to_string()))?
        .map_err(|e| CalendarError::Credential(e.to_string()))
    }

    async fn exchange(
        &self,
        method: &str,
        url: Url,
        headers: &[(&str, &str)],
        body: Option<String>,
    ) -> Result<Reply, CalendarError> {
        let pw = self.password().await?;
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|e| CalendarError::Server(e.to_string()))?;
        let mut req = self
            .http
            .request(method, url)
            .basic_auth(&self.account.username, Some(pw));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req.body(b);
        }
        let mut resp = req
            .send()
            .await
            .map_err(|e| CalendarError::Server(redact(&e.to_string())))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(CalendarError::Server(format!(
                "the server answered with a redirect ({status}); use the final calendar url"
            )));
        }
        let headers = resp.headers().clone();
        let too_large = || CalendarError::Server("response too large".into());
        if resp
            .content_length()
            .is_some_and(|n| n > u64::try_from(MAX_BODY).unwrap_or(u64::MAX))
        {
            return Err(too_large());
        }
        // Read chunk by chunk and stop at the cap: a server that streams without end
        // (or lies about its length) never gets more than `MAX_BODY` into memory.
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| CalendarError::Server(redact(&e.to_string())))?
        {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn send(
        &self,
        method: &str,
        url: Url,
        headers: &[(&str, &str)],
        body: Option<String>,
    ) -> Result<(StatusCode, String), CalendarError> {
        let r = self.exchange(method, url, headers, body).await?;
        Ok((r.status, r.body))
    }

    fn check(status: StatusCode, what: &str) -> Result<(), CalendarError> {
        if status.is_success() {
            return Ok(());
        }
        Err(CalendarError::Server(match status.as_u16() {
            401 | 403 => format!("{what}: the server refused the login or the action ({status})"),
            404 => format!("{what}: not found ({status}); check the calendar url"),
            412 => format!("{what}: the event was changed by someone else meanwhile; try again"),
            _ => format!("{what}: HTTP {status}"),
        }))
    }

    async fn report(&self, body: String) -> Result<(StatusCode, String), CalendarError> {
        self.send(
            "REPORT",
            self.base.clone(),
            &[
                ("Depth", "1"),
                ("Content-Type", "application/xml; charset=utf-8"),
            ],
            Some(body),
        )
        .await
    }

    /// The address invitations come from: the account's `email`, else a `username` that is one.
    #[must_use]
    pub fn organizer(&self) -> Option<String> {
        let a = self
            .account
            .email
            .as_deref()
            .unwrap_or(&self.account.username);
        ical::clean_address(a).ok()
    }

    /// Events overlapping `[from, to)`, recurring ones expanded (by the server,
    /// or here when it sent the series instead).
    ///
    /// # Errors
    /// See [`CalendarError`].
    pub async fn list(
        &self,
        from: When,
        to: When,
        limit: usize,
    ) -> Result<Vec<Event>, CalendarError> {
        let (a, b) = (stamp(from.utc()), stamp(to.utc()));
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
<d:prop><c:calendar-data><c:expand start=\"{a}\" end=\"{b}\"/></c:calendar-data></d:prop>\
<c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\">\
<c:time-range start=\"{a}\" end=\"{b}\"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"
        );
        let (status, text) = self.report(body).await?;
        Self::check(status, "listing events")?;
        // One budget for the whole answer: a hostile server cannot make this expand forever.
        let mut budget = Budget::new(MAX_EXPANDED_EVENTS, MAX_RULE_WORK);
        let mut events: Vec<Event> = Vec::new();
        for r in multistatus(&text) {
            events.extend(ical::events_in_range(
                &r.data,
                from.utc(),
                to.utc(),
                &mut budget,
            ));
        }
        if budget.exhausted() {
            return Err(CalendarError::Server(
                "the calendar holds too many events in this range to list; ask for a shorter range"
                    .into(),
            ));
        }
        events.sort_by(|x, y| x.start.cmp(&y.start));
        events.truncate(limit);
        Ok(events)
    }

    /// Create an event; returns its new `UID`. `spec.uid` is ignored: a new one is made.
    /// With attendees the event is written with them; delivering the invitation is
    /// the caller's job (see [`CalendarClient::supports_scheduling`]).
    ///
    /// # Errors
    /// See [`CalendarError`].
    pub async fn create(&self, spec: &EventSpec<'_>) -> Result<Created, CalendarError> {
        if spec.end.utc() <= spec.start.utc() {
            return Err(CalendarError::Invalid("`end` must be after `start`".into()));
        }
        if matches!(spec.start, When::Day(_)) != matches!(spec.end, When::Day(_)) {
            return Err(CalendarError::Invalid(
                "`start` and `end` must both be dates or both be times".into(),
            ));
        }
        let uid = format!("{}@praxi", uuid::Uuid::now_v7());
        let ics = ical::build_event(&EventSpec { uid: &uid, ..*spec });
        let url = self
            .base
            .join(&format!("{uid}.ics"))
            .map_err(|e| CalendarError::Invalid(e.to_string()))?;
        let (status, _) = self
            .send(
                "PUT",
                url,
                &[
                    ("Content-Type", "text/calendar; charset=utf-8"),
                    ("If-None-Match", "*"),
                ],
                Some(ics.clone()),
            )
            .await?;
        Self::check(status, "creating the event")?;
        Ok(Created { uid, ics })
    }

    /// The stored event with this `UID`, with its link and version.
    pub(crate) async fn fetch(&self, uid: &str) -> Result<Option<Stored>, CalendarError> {
        check_uid(uid)?;
        let want = quick_xml::escape::escape(uid);
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
<d:prop><d:getetag/><c:calendar-data/></d:prop>\
<c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\">\
<c:prop-filter name=\"UID\"><c:text-match collation=\"i;octet\">{want}</c:text-match></c:prop-filter>\
</c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"
        );
        let (status, text) = self.report(body).await?;
        Self::check(status, "looking up the event")?;
        let Some(found) = multistatus(&text).into_iter().find(|r| !r.href.is_empty()) else {
            return Ok(None);
        };
        let url = self
            .base
            .join(&found.href)
            .map_err(|e| CalendarError::Server(format!("bad href from the server: {e}")))?;
        if url.origin() != self.base.origin() || !url.path().starts_with(self.base.path()) {
            return Err(CalendarError::Server(
                "the server pointed outside the configured calendar; refusing".into(),
            ));
        }
        Ok(Some(Stored {
            url,
            etag: (!found.etag.is_empty()).then_some(found.etag),
            ics: found.data,
        }))
    }

    /// The stored event (as the agent sees it) with this `UID`, whole series included.
    ///
    /// # Errors
    /// See [`CalendarError`].
    pub async fn get(&self, uid: &str) -> Result<Option<Event>, CalendarError> {
        Ok(self.fetch(uid).await?.and_then(|s| s.event()))
    }

    /// The edit `patch` makes to a stored event, worked out but not written yet (so the
    /// caller can check who it would tell before anything changes on the server).
    ///
    /// # Errors
    /// [`CalendarError::Invalid`] when the change makes no sense for this event.
    pub(crate) fn plan_update(stored: &Stored, patch: &Patch) -> Result<Edited, CalendarError> {
        if stored.ics.trim().is_empty() {
            return Err(CalendarError::Server(
                "the server did not return the event's data".into(),
            ));
        }
        ical::edit(&stored.ics, patch).map_err(CalendarError::Invalid)
    }

    /// Write an edit made by [`CalendarClient::plan_update`] over the stored event.
    pub(crate) async fn put_edited(
        &self,
        stored: Stored,
        edited: &Edited,
    ) -> Result<(), CalendarError> {
        let mut headers = vec![("Content-Type", "text/calendar; charset=utf-8")];
        if let Some(t) = stored.etag.as_deref() {
            headers.push(("If-Match", t));
        }
        let (status, _) = self
            .send("PUT", stored.url, &headers, Some(edited.ics.clone()))
            .await?;
        Self::check(status, "updating the event")
    }

    /// Whether the server delivers invitations itself when an event with
    /// attendees is written (`calendar-auto-schedule`, RFC 6638). `false` when
    /// it does not say so.
    pub async fn supports_scheduling(&self) -> bool {
        self.exchange("OPTIONS", self.base.clone(), &[], None)
            .await
            .is_ok_and(|r| {
                r.headers
                    .get_all("dav")
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .any(|v| {
                        v.split(',')
                            .any(|t| t.trim().eq_ignore_ascii_case("calendar-auto-schedule"))
                    })
            })
    }

    /// Delete a stored event found by [`CalendarClient::fetch`]. `false` when it is already gone.
    pub(crate) async fn remove(&self, found: Stored) -> Result<bool, CalendarError> {
        let (status, _) = self.send("DELETE", found.url, &[], None).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        Self::check(status, "deleting the event")?;
        Ok(true)
    }

    async fn propfind(&self, url: Url, depth: &str) -> Result<Vec<DavEntry>, CalendarError> {
        let body = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop>\
<d:resourcetype/><d:displayname/><d:current-user-principal/><c:calendar-home-set/>\
<c:supported-calendar-component-set/></d:prop></d:propfind>";
        let r = self
            .exchange(
                "PROPFIND",
                url,
                &[
                    ("Depth", depth),
                    ("Content-Type", "application/xml; charset=utf-8"),
                ],
                Some(body.to_owned()),
            )
            .await?;
        Self::check(r.status, "looking for calendars")?;
        Ok(dav_entries(&r.body))
    }

    /// Resolve a link the server gave during discovery. It must stay on the configured
    /// host. The one exception is a service that spreads its calendars over several
    /// hosts of its own domain (iCloud: `pNN-caldav.icloud.com`): there it may move
    /// between hosts of that domain, named in [`PROVIDER_DOMAINS`], never to an address
    /// literal and never by comparing a few trailing labels (`evil.co.uk` is not
    /// `a.co.uk`). It never changes scheme or port, carries no login in the url, and
    /// must pass the address policy.
    pub(crate) fn discovered(&self, from: &Url, href: &str) -> Result<Url, CalendarError> {
        let refuse =
            || CalendarError::Server("the server pointed to another service; refusing".into());
        let url = from
            .join(href)
            .map_err(|e| CalendarError::Server(format!("bad link from the server: {e}")))?;
        if url.scheme() != self.base.scheme()
            || url.port_or_known_default() != self.base.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(refuse());
        }
        let name = |u: &Url| match u.host() {
            Some(Host::Domain(d)) => Some(d.trim_end_matches('.').to_ascii_lowercase()),
            _ => None,
        };
        let same_host = match (url.host(), self.base.host()) {
            (Some(Host::Domain(_)), Some(Host::Domain(_))) => name(&url) == name(&self.base),
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        let same_provider = match (name(&url), name(&self.base)) {
            (Some(a), Some(b)) => {
                let provider = |h: &str| {
                    PROVIDER_DOMAINS
                        .iter()
                        .find(|d| h == **d || h.ends_with(&format!(".{d}")))
                };
                provider(&a).is_some() && provider(&a) == provider(&b)
            }
            _ => false,
        };
        if !(same_host || same_provider) {
            return Err(refuse());
        }
        self.policy.check_url(&url).map_err(|e| {
            CalendarError::Server(format!("the server pointed somewhere not allowed: {e}"))
        })?;
        Ok(url)
    }

    /// The calendars reachable from the account's url: the url itself when it
    /// is one, else the calendars in the user's calendar home.
    ///
    /// # Errors
    /// See [`CalendarError`].
    pub async fn discover(&self) -> Result<Vec<CalendarInfo>, CalendarError> {
        let first = self.propfind(self.base.clone(), "0").await?;
        let me = first.into_iter().next().unwrap_or_default();
        if me.is_calendar {
            return Ok(vec![CalendarInfo::of(&me, &self.base)]);
        }
        let mut home = self.base.clone();
        if !me.home.is_empty() {
            home = self.discovered(&self.base, &me.home)?;
        } else if !me.principal.is_empty() {
            let principal = self.discovered(&self.base, &me.principal)?;
            let p = self.propfind(principal.clone(), "0").await?;
            if let Some(h) = p.into_iter().map(|e| e.home).find(|h| !h.is_empty()) {
                home = self.discovered(&principal, &h)?;
            }
        }
        let mut found = Vec::new();
        for e in self.propfind(home.clone(), "1").await? {
            let Ok(url) = self.discovered(&home, &e.href) else {
                continue;
            };
            let events = e.components.is_empty()
                || e.components
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case("VEVENT"));
            if e.is_calendar && events && url.path() != home.path() {
                found.push(CalendarInfo::of(&e, &url));
            }
        }
        Ok(found)
    }
}

/// A calendar found by [`CalendarClient::discover`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarInfo {
    /// Name the owner gave it.
    pub name: String,
    /// Its url: what goes into [`CalendarAccount::url`].
    pub url: String,
}

impl CalendarInfo {
    fn of(e: &DavEntry, url: &Url) -> Self {
        let name = if e.display_name.trim().is_empty() {
            url.path_segments()
                .and_then(|mut s| s.rfind(|p| !p.is_empty()))
                .unwrap_or("calendar")
                .to_owned()
        } else {
            e.display_name.trim().to_owned()
        };
        Self {
            name,
            url: url.as_str().to_owned(),
        }
    }
}

/// An event written by [`CalendarClient::create`].
#[derive(Debug, Clone)]
pub struct Created {
    /// Its new `UID`.
    pub uid: String,
    /// The `.ics` body that was stored.
    pub ics: String,
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: String,
}

/// An event as the server holds it.
pub(crate) struct Stored {
    url: Url,
    etag: Option<String>,
    ics: String,
}

impl Stored {
    /// The event (the series master when it recurs).
    pub(crate) fn event(&self) -> Option<Event> {
        ical::parse_events(&self.ics).into_iter().next()
    }
}

/// An event `UID` the agent gave: it goes into an XML query, so it must be plain text.
fn check_uid(uid: &str) -> Result<(), CalendarError> {
    if uid.is_empty() || uid.len() > MAX_UID || uid.chars().any(char::is_control) {
        return Err(CalendarError::Invalid(
            "`uid` must be the uid of an event, as listed (plain text, up to 255 characters)"
                .into(),
        ));
    }
    Ok(())
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Remove anything that looks like a credential-bearing URL from a transport error text.
fn redact(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            if w.contains("://") && w.contains('@') {
                "<url>"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The text an entity reference (`&amp;`, `&#38;`) stands for.
fn general_ref(r: &quick_xml::events::BytesRef<'_>) -> Option<String> {
    match r.resolve_char_ref() {
        Ok(Some(c)) => Some(c.to_string()),
        _ => match &**r {
            "lt" => Some("<".to_owned()),
            "gt" => Some(">".to_owned()),
            "amp" => Some("&".to_owned()),
            "quot" => Some("\"".to_owned()),
            "apos" => Some("'".to_owned()),
            _ => None,
        },
    }
}

#[derive(Default)]
struct Response {
    href: String,
    data: String,
    etag: String,
}

#[derive(PartialEq, Clone, Copy)]
enum Field {
    None,
    Href,
    Data,
    Etag,
}

/// The `response` entries of a multistatus document.
fn multistatus(xml: &str) -> Vec<Response> {
    let mut reader = Reader::from_str(xml);
    let mut out: Vec<Response> = Vec::new();
    let mut field = Field::None;
    loop {
        let piece: Option<String> = match reader.read_event() {
            Ok(Xml::Start(e)) => {
                field = match e.local_name().as_ref() {
                    "response" => {
                        out.push(Response::default());
                        Field::None
                    }
                    "href" => Field::Href,
                    "calendar-data" => Field::Data,
                    "getetag" => Field::Etag,
                    _ => Field::None,
                };
                None
            }
            Ok(Xml::End(_)) => {
                field = Field::None;
                None
            }
            Ok(Xml::Text(t)) => Some(t.xml10_content().into_owned()),
            Ok(Xml::CData(t)) => Some(t.xml10_content().into_owned()),
            Ok(Xml::GeneralRef(r)) => general_ref(&r),
            Ok(Xml::Eof) | Err(_) => break,
            _ => None,
        };
        if let (Some(p), Some(last)) = (piece, out.last_mut()) {
            match field {
                Field::Href => last.href.push_str(&p),
                Field::Data => last.data.push_str(&p),
                Field::Etag => last.etag.push_str(&p),
                Field::None => {}
            }
        }
    }
    for r in &mut out {
        r.href = r.href.trim().to_owned();
        r.etag = r.etag.trim().to_owned();
    }
    out
}

/// One resource of a `PROPFIND` answer.
#[derive(Default)]
struct DavEntry {
    href: String,
    display_name: String,
    is_calendar: bool,
    principal: String,
    home: String,
    components: Vec<String>,
}

/// The resources of a `PROPFIND` multistatus document.
fn dav_entries(xml: &str) -> Vec<DavEntry> {
    let mut reader = Reader::from_str(xml);
    let mut out: Vec<DavEntry> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    loop {
        // Text arrives in pieces: plain text, CDATA and one piece per entity (`&amp;`).
        let piece: Option<String> = match reader.read_event() {
            Ok(Xml::Start(e)) => {
                let name = e.local_name().as_ref().to_owned();
                if name == "response" {
                    out.push(DavEntry::default());
                }
                note_dav_element(&mut out, &path, &name, &e);
                path.push(name);
                None
            }
            Ok(Xml::Empty(e)) => {
                let name = e.local_name().as_ref().to_owned();
                note_dav_element(&mut out, &path, &name, &e);
                None
            }
            Ok(Xml::End(_)) => {
                path.pop();
                None
            }
            Ok(Xml::Text(t)) => Some(t.xml10_content().into_owned()),
            Ok(Xml::CData(t)) => Some(t.xml10_content().into_owned()),
            Ok(Xml::GeneralRef(r)) => general_ref(&r),
            Ok(Xml::Eof) | Err(_) => break,
            _ => None,
        };
        if let (Some(text), Some(last), Some(here)) = (piece, out.last_mut(), path.last()) {
            let inside = |n: &str| path.iter().any(|p| p == n);
            // Whitespace between elements is a piece too; an address only ever grows by
            // its own characters, so pieces are joined and trimmed once, below.
            match here.as_str() {
                "href" if inside("current-user-principal") => last.principal.push_str(&text),
                "href" if inside("calendar-home-set") => last.home.push_str(&text),
                "href" => last.href.push_str(&text),
                "displayname" => last.display_name.push_str(&text),
                _ => {}
            }
        }
    }
    for e in &mut out {
        e.href = e.href.trim().to_owned();
        e.principal = e.principal.trim().to_owned();
        e.home = e.home.trim().to_owned();
    }
    out
}

fn note_dav_element(
    out: &mut [DavEntry],
    path: &[String],
    name: &str,
    e: &quick_xml::events::BytesStart<'_>,
) {
    let Some(last) = out.last_mut() else { return };
    let inside = |n: &str| path.iter().any(|p| p == n);
    if name == "calendar" && inside("resourcetype") {
        last.is_calendar = true;
    }
    if name == "comp"
        && inside("supported-calendar-component-set")
        && let Some(a) = e
            .attributes()
            .flatten()
            .find(|a| a.key.local_name().as_ref() == "name")
    {
        last.components.push(a.value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_links_keep_their_entities_and_cdata() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
 <d:response>
  <d:href>
    /cal/R&amp;D/team&#38;co/
  </d:href>
  <d:propstat><d:prop>
   <d:displayname>Work &amp; Play &lt;2026&gt; &quot;a&quot; &apos;b&apos; &#169;</d:displayname>
   <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
  </d:prop></d:propstat>
 </d:response>
 <d:response>
  <d:href><![CDATA[/cal/a&b/]]></d:href>
  <d:propstat><d:prop>
   <d:displayname><![CDATA[Fish & <Chips>]]></d:displayname>
   <d:current-user-principal><d:href>/principals/a&amp;b/</d:href></d:current-user-principal>
   <c:calendar-home-set><d:href>/home/a&amp;b/</d:href></c:calendar-home-set>
  </d:prop></d:propstat>
 </d:response>
</d:multistatus>"#;
        let got = dav_entries(xml);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].href, "/cal/R&D/team&co/");
        assert_eq!(got[0].display_name, "Work & Play <2026> \"a\" 'b' \u{a9}");
        assert!(got[0].is_calendar);
        assert_eq!(got[1].href, "/cal/a&b/");
        assert_eq!(got[1].display_name, "Fish & <Chips>");
        assert_eq!(got[1].principal, "/principals/a&b/");
        assert_eq!(got[1].home, "/home/a&b/");
    }

    #[test]
    fn a_uid_is_plain_text_of_a_sane_length() {
        assert!(check_uid("abc123@example.com").is_ok());
        assert!(check_uid(&"x".repeat(255)).is_ok());
        for bad in ["", "a\nb", "a\u{0}b", "\u{7f}", &"x".repeat(256)] {
            assert!(check_uid(bad).is_err(), "{bad:?}");
        }
    }
}

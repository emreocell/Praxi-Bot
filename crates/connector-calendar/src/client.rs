//! `CalDAV` client: REPORT (query), PUT (create) and DELETE over one calendar
//! collection. TLS is required (plain HTTP only for loopback), redirects are
//! never followed (the password must not travel to another host), and an
//! `href` the server returns is used only when it stays on the configured origin.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::Event as Xml;
use reqwest::{Method, StatusCode};
use secret_store::{SecretRef, SecretStore, resolve};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ical::{self, Edited, Event, EventSpec, Patch, When};

/// Largest response body read from a server.
const MAX_BODY: usize = 4 << 20;

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
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host == "[::1]" || host.starts_with("127.")
}

impl CalendarClient {
    /// # Errors
    /// [`CalendarError::Invalid`] for a bad URL or plain HTTP to a non-loopback host.
    pub fn new(
        account: CalendarAccount,
        store: Arc<dyn SecretStore>,
    ) -> Result<Self, CalendarError> {
        let mut base = Url::parse(&account.url).map_err(|e| {
            CalendarError::Invalid(format!("calendar `{}`: bad url: {e}", account.name))
        })?;
        let host = base.host_str().unwrap_or_default().to_owned();
        match base.scheme() {
            "https" => {}
            "http" if is_loopback(&host) => {}
            _ => {
                return Err(CalendarError::Invalid(format!(
                    "calendar `{}`: the url must be https (plain http only for localhost)",
                    account.name
                )));
            }
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(CalendarError::Invalid(format!(
                "calendar `{}`: put the login in `username`/`password`, not in the url",
                account.name
            )));
        }
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(account.timeout_secs.clamp(1, 120)))
            .build()
            .map_err(|e| CalendarError::Server(e.to_string()))?;
        Ok(Self {
            account,
            base,
            store,
            http,
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
        let resp = req
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
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| CalendarError::Server(redact(&e.to_string())))?;
        if bytes.len() > MAX_BODY {
            return Err(CalendarError::Server("response too large".into()));
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
        let mut events: Vec<Event> = multistatus(&text)
            .into_iter()
            .flat_map(|r| ical::events_in_range(&r.data, from.utc(), to.utc()))
            .collect();
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
    async fn fetch(&self, uid: &str) -> Result<Option<Stored>, CalendarError> {
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
        Ok(self
            .fetch(uid)
            .await?
            .and_then(|s| ical::parse_events(&s.ics).into_iter().next()))
    }

    /// Change an event (the whole series when it recurs). `None` when there is none.
    ///
    /// # Errors
    /// See [`CalendarError`].
    pub async fn update(&self, uid: &str, patch: &Patch) -> Result<Option<Updated>, CalendarError> {
        let Some(stored) = self.fetch(uid).await? else {
            return Ok(None);
        };
        if stored.ics.trim().is_empty() {
            return Err(CalendarError::Server(
                "the server did not return the event's data".into(),
            ));
        }
        let edited = ical::edit(&stored.ics, patch).map_err(CalendarError::Invalid)?;
        let mut headers = vec![("Content-Type", "text/calendar; charset=utf-8")];
        let etag = stored.etag.clone();
        if let Some(t) = etag.as_deref() {
            headers.push(("If-Match", t));
        }
        let (status, _) = self
            .send("PUT", stored.url, &headers, Some(edited.ics.clone()))
            .await?;
        Self::check(status, "updating the event")?;
        Ok(Some(Updated { edited }))
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

    /// Delete the event with this `UID`. Returns `false` when there is none.
    ///
    /// # Errors
    /// See [`CalendarError`].
    pub async fn delete(&self, uid: &str) -> Result<bool, CalendarError> {
        let Some(found) = self.fetch(uid).await? else {
            return Ok(false);
        };
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

    /// Resolve a link the server gave during discovery: it may live on another
    /// host of the same service (iCloud keeps calendars on `pNN-caldav.icloud.com`)
    /// but never on an unrelated one, and never over plain `http` from `https`.
    fn discovered(&self, from: &Url, href: &str) -> Result<Url, CalendarError> {
        let url = from
            .join(href)
            .map_err(|e| CalendarError::Server(format!("bad link from the server: {e}")))?;
        let site = |u: &Url| -> Option<String> {
            let h = u.host_str()?.trim_end_matches('.').to_ascii_lowercase();
            let labels: Vec<&str> = h.split('.').collect();
            Some(labels[labels.len().saturating_sub(2)..].join("."))
        };
        let same = url.scheme() == self.base.scheme()
            && url.port_or_known_default() == self.base.port_or_known_default()
            && (url.host_str() == self.base.host_str()
                || (!is_loopback(url.host_str().unwrap_or_default())
                    && site(&url).is_some()
                    && site(&url) == site(&self.base)));
        if same {
            Ok(url)
        } else {
            Err(CalendarError::Server(
                "the server pointed to another service; refusing".into(),
            ))
        }
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

/// An event after [`CalendarClient::update`].
#[derive(Debug, Clone)]
pub struct Updated {
    /// What changed and who is affected.
    pub edited: Edited,
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: String,
}

struct Stored {
    url: Url,
    etag: Option<String>,
    ics: String,
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
            Ok(Xml::GeneralRef(r)) => match r.resolve_char_ref() {
                Ok(Some(c)) => Some(c.to_string()),
                _ => match &*r {
                    "lt" => Some("<".to_owned()),
                    "gt" => Some(">".to_owned()),
                    "amp" => Some("&".to_owned()),
                    "quot" => Some("\"".to_owned()),
                    "apos" => Some("'".to_owned()),
                    _ => None,
                },
            },
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
        match reader.read_event() {
            Ok(Xml::Start(e)) => {
                let name = e.local_name().as_ref().to_owned();
                if name == "response" {
                    out.push(DavEntry::default());
                }
                note_dav_element(&mut out, &path, &name, &e);
                path.push(name);
            }
            Ok(Xml::Empty(e)) => {
                let name = e.local_name().as_ref().to_owned();
                note_dav_element(&mut out, &path, &name, &e);
            }
            Ok(Xml::End(_)) => {
                path.pop();
            }
            Ok(Xml::Text(t)) => {
                if let (Some(last), Some(here)) = (out.last_mut(), path.last()) {
                    let text = t.xml10_content().into_owned();
                    let inside = |n: &str| path.iter().any(|p| p == n);
                    match here.as_str() {
                        "href" if inside("current-user-principal") => {
                            last.principal.push_str(text.trim());
                        }
                        "href" if inside("calendar-home-set") => {
                            last.home.push_str(text.trim());
                        }
                        "href" => last.href.push_str(text.trim()),
                        "displayname" => last.display_name.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(Xml::Eof) | Err(_) => break,
            _ => {}
        }
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

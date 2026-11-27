//! Offline tests against an in-process fake `CalDAV` server.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tool_registry::{ContentPart, Effect, ToolContext, ToolError, ToolRegistry};

use crate::tools::tools;
use crate::{CalendarAccount, CalendarConnector, InviteMailer, calendar_policy, register};

type Events = Arc<Mutex<BTreeMap<String, String>>>;

/// How the fake server behaves.
#[derive(Clone, Copy, Default)]
struct Opts {
    /// REPORT answers point at another host.
    evil_href: bool,
    /// Advertise `calendar-auto-schedule`.
    scheduling: bool,
    /// Honour `<c:expand>` (else a series comes back as its master).
    expand: bool,
}

struct Fake {
    port: u16,
    events: Events,
    seen_auth: Arc<Mutex<Vec<String>>>,
    /// `(method, path, lower-cased headers)` of every request.
    requests: Arc<Mutex<Vec<(String, String, String)>>>,
}

fn attr(body: &str, name: &str) -> Option<DateTime<Utc>> {
    let key = format!("{name}=\"");
    let rest = &body[body.find(&key)? + key.len()..];
    let v = &rest[..rest.find('"')?];
    NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
        .ok()
        .map(|n| n.and_utc())
}

fn prop<'a>(ics: &'a str, name: &str) -> Option<&'a str> {
    ics.lines().find_map(|l| {
        l.strip_prefix(&format!("{name}:")).or_else(|| {
            l.strip_prefix(&format!("{name};"))
                .and_then(|r| r.split_once(':').map(|(_, v)| v))
        })
    })
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn at(v: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
        .ok()
        .map(|n| n.and_utc())
}

/// A server that does expansion the simple way: DAILY/WEEKLY with COUNT.
fn expanded(ics: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<String> {
    let (Some(start), Some(end)) = (
        prop(ics, "DTSTART").and_then(at),
        prop(ics, "DTEND").and_then(at),
    ) else {
        return vec![ics.to_owned()];
    };
    let Some(rule) = prop(ics, "RRULE") else {
        return if start < to && end > from {
            vec![ics.to_owned()]
        } else {
            Vec::new()
        };
    };
    let part = |k: &str| {
        rule.split(';')
            .find_map(|p| p.strip_prefix(&format!("{k}=")))
    };
    let step = if part("FREQ") == Some("WEEKLY") { 7 } else { 1 }
        * part("INTERVAL").and_then(|v| v.parse().ok()).unwrap_or(1);
    let count: i64 = part("COUNT").and_then(|v| v.parse().ok()).unwrap_or(1);
    (0..count)
        .map(|i| {
            (
                start + Duration::days(i * step),
                end + Duration::days(i * step),
            )
        })
        .filter(|(s, e)| *s < to && *e > from)
        .map(|(s, e)| {
            let mut out = String::new();
            for l in ics.lines() {
                if l.starts_with("RRULE") {
                    continue;
                }
                if l.starts_with("DTSTART") {
                    let _ = writeln!(out, "RECURRENCE-ID:{}\nDTSTART:{}", stamp(s), stamp(s));
                } else if l.starts_with("DTEND") {
                    let _ = writeln!(out, "DTEND:{}", stamp(e));
                } else {
                    let _ = writeln!(out, "{l}");
                }
            }
            out
        })
        .collect()
}

const PROPS: &str = "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>";

fn dav_response(href: &str, name: &str, calendar: bool, extra: &str) -> String {
    let rt = if calendar {
        PROPS
    } else {
        "<d:resourcetype><d:collection/></d:resourcetype>"
    };
    let comps = if calendar {
        "<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set>"
    } else {
        ""
    };
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:displayname>{name}</d:displayname>{rt}{comps}{extra}</d:prop></d:propstat></d:response>"
    )
}

/// A tiny `CalDAV` server: `PUT /cal/<uid>.ics` (with `If-Match` / `If-None-Match`),
/// `DELETE`, `OPTIONS`, `PROPFIND` for discovery and `REPORT`.
#[allow(clippy::too_many_lines)] // one request handler with a branch per HTTP method
async fn fake_with(opts: Opts) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let events: Events = Arc::default();
    let versions: Arc<Mutex<BTreeMap<String, u64>>> = Arc::default();
    let seen_auth: Arc<Mutex<Vec<String>>> = Arc::default();
    let requests: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
    let (ev, auth, reqs) = (events.clone(), seen_auth.clone(), requests.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (ev, auth, versions, reqs) =
                (ev.clone(), auth.clone(), versions.clone(), reqs.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    if let Some((h, b)) = text.split_once("\r\n\r\n") {
                        let len = h
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if b.len() >= len {
                            break (h.to_owned(), b.to_owned());
                        }
                    }
                };
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or("").split(' ');
                let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
                let headers = lines
                    .map(str::to_ascii_lowercase)
                    .collect::<Vec<_>>()
                    .join("\n");
                for l in headers.lines() {
                    if l.starts_with("authorization:") {
                        auth.lock().unwrap().push(l.to_owned());
                    }
                }
                reqs.lock()
                    .unwrap()
                    .push((method.to_owned(), path.to_owned(), headers.clone()));
                let uid_of = |p: &str| {
                    p.rsplit('/')
                        .next()
                        .unwrap()
                        .trim_end_matches(".ics")
                        .to_owned()
                };
                let mut extra_headers = String::new();
                let (status, payload) = match method {
                    "OPTIONS" => {
                        let dav = if opts.scheduling {
                            "1, calendar-access, calendar-auto-schedule"
                        } else {
                            "1, calendar-access"
                        };
                        extra_headers = format!("DAV: {dav}\r\n");
                        ("200 OK", String::new())
                    }
                    "PUT" => {
                        let uid = uid_of(path);
                        let mut v = versions.lock().unwrap();
                        let current = v.get(&uid).copied();
                        let if_match = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("if-match:"))
                            .map(|s| s.trim().to_owned());
                        let none_match = headers.contains("if-none-match: *");
                        if (none_match && current.is_some())
                            || if_match.is_some_and(|m| {
                                Some(format!("\"v{}\"", current.unwrap_or(0))) != Some(m)
                            })
                        {
                            ("412 Precondition Failed", String::new())
                        } else {
                            *v.entry(uid.clone()).or_insert(0) += 1;
                            ev.lock().unwrap().insert(uid, body);
                            ("201 Created", String::new())
                        }
                    }
                    "DELETE" => match ev.lock().unwrap().remove(&uid_of(path)) {
                        Some(_) => ("204 No Content", String::new()),
                        None => ("404 Not Found", String::new()),
                    },
                    "PROPFIND" => {
                        let depth0 = head.to_ascii_lowercase().contains("depth: 0");
                        let mut xml = String::from(
                            "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">",
                        );
                        match (path, depth0) {
                            ("/", _) => xml.push_str(&dav_response("/", "", false, "<d:current-user-principal><d:href>/principals/me/</d:href></d:current-user-principal>")),
                            ("/principals/me/", _) => xml.push_str(&dav_response("/principals/me/", "me", false, "<c:calendar-home-set><d:href>/home/</d:href></c:calendar-home-set>")),
                            ("/home/", false) => {
                                xml.push_str(&dav_response("/home/", "Home", false, ""));
                                xml.push_str(&dav_response("/home/personal/", "Personal", true, ""));
                                xml.push_str(&dav_response("/home/inbox/", "Inbox", false, ""));
                                xml.push_str(&dav_response("/home/work/", "", true, ""));
                            }
                            ("/home/", true) => xml.push_str(&dav_response("/home/", "Home", false, "")),
                            (p, _) if p.starts_with("/cal") => xml.push_str(&dav_response("/cal/", "Only one", true, "")),
                            _ => {}
                        }
                        xml.push_str("</d:multistatus>");
                        if xml.contains("<d:response>") {
                            ("207 Multi-Status", xml)
                        } else {
                            ("404 Not Found", String::new())
                        }
                    }
                    "REPORT" => {
                        let mut xml = String::from(
                            "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">",
                        );
                        let by_uid = body.contains("text-match");
                        let (from, to) = (attr(&body, "start"), attr(&body, "end"));
                        for (uid, ics) in ev.lock().unwrap().iter() {
                            if by_uid && !body.contains(uid.as_str()) {
                                continue;
                            }
                            let docs: Vec<String> = match (
                                by_uid,
                                from.zip(to),
                                opts.expand && body.contains("expand"),
                            ) {
                                (false, Some((f, t)), true) => expanded(ics, f, t),
                                (false, Some((f, t)), false) if !ics.contains("RRULE") => {
                                    expanded(ics, f, t)
                                }
                                _ => vec![ics.clone()],
                            };
                            let href = if opts.evil_href {
                                format!("http://127.0.0.1:1/cal/{uid}.ics")
                            } else {
                                format!("/cal/{uid}.ics")
                            };
                            let etag = format!(
                                "\"v{}\"",
                                versions.lock().unwrap().get(uid).copied().unwrap_or(0)
                            );
                            for d in docs {
                                let esc = d.replace('&', "&amp;").replace('<', "&lt;");
                                let _ = write!(
                                    xml,
                                    "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><c:calendar-data>{esc}</c:calendar-data></d:prop></d:propstat></d:response>"
                                );
                            }
                        }
                        xml.push_str("</d:multistatus>");
                        ("207 Multi-Status", xml)
                    }
                    _ => ("405 Method Not Allowed", String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/xml\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Fake {
        port,
        events,
        seen_auth,
        requests,
    }
}

async fn fake(evil_href: bool) -> Fake {
    fake_with(Opts {
        evil_href,
        ..Opts::default()
    })
    .await
}

fn account(name: &str, port: u16, path: &str) -> CalendarAccount {
    CalendarAccount {
        name: name.into(),
        url: format!("http://127.0.0.1:{port}{path}"),
        username: "me@example.com".into(),
        password: SecretRef::new(format!("calendar/{name}/password")).unwrap(),
        email: None,
        timeout_secs: 5,
    }
}

fn store(names: &[&str]) -> Arc<dyn SecretStore> {
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    for n in names {
        store
            .set(&format!("calendar/{n}/password"), "s3cret-pw")
            .unwrap();
    }
    store
}

fn connector(port: u16) -> Arc<CalendarConnector> {
    let (c, problems) =
        CalendarConnector::new(vec![account("home", port, "/cal")], &store(&["home"]));
    assert!(problems.is_empty(), "{problems:?}");
    Arc::new(c)
}

/// `(to, subject, method, ics)` of one mail.
type Mail = (Vec<String>, String, String, String);

/// A mailer that records what it is asked and can refuse an address (the owner's mail rules).
#[derive(Default)]
struct Outbox {
    mails: Mutex<Vec<Mail>>,
    /// `(bot, to)` of every rules check.
    checked: Mutex<Vec<(Option<String>, Vec<String>)>>,
    /// `(bot, to, subject)` of every mail the calendar server delivered by itself.
    server: Mutex<Vec<(Option<String>, Vec<String>, String)>>,
    /// An address the rules refuse.
    refuse: Mutex<Option<String>>,
}

impl Outbox {
    fn sent(&self) -> Vec<Mail> {
        self.mails.lock().unwrap().clone()
    }

    fn refusing(addr: &str) -> Arc<Self> {
        let out = Self::default();
        *out.refuse.lock().unwrap() = Some(addr.to_owned());
        Arc::new(out)
    }
}

#[async_trait]
impl InviteMailer for Outbox {
    async fn check(&self, bot: Option<&str>, to: &[String]) -> Result<(), String> {
        self.checked
            .lock()
            .unwrap()
            .push((bot.map(str::to_owned), to.to_vec()));
        match self.refuse.lock().unwrap().as_deref() {
            Some(a) if to.iter().any(|t| t == a) => Err(format!("{a} asked not to be written to")),
            _ => Ok(()),
        }
    }

    async fn send(
        &self,
        _bot: Option<&str>,
        to: &[String],
        subject: &str,
        _body: &str,
        ics: &str,
        method: &str,
    ) -> Result<(), String> {
        self.mails.lock().unwrap().push((
            to.to_vec(),
            subject.to_owned(),
            method.to_owned(),
            ics.to_owned(),
        ));
        Ok(())
    }

    fn record_server_delivery(
        &self,
        bot: Option<&str>,
        _organizer: &str,
        to: &[String],
        subject: &str,
    ) {
        self.server
            .lock()
            .unwrap()
            .push((bot.map(str::to_owned), to.to_vec(), subject.to_owned()));
    }
}

async fn call(c: &Arc<CalendarConnector>, name: &str, input: Value) -> Result<Value, ToolError> {
    call_in(c, &ToolContext::default(), name, input).await
}

/// A call made by the bot `bot`.
async fn call_as(
    c: &Arc<CalendarConnector>,
    bot: &str,
    name: &str,
    input: Value,
) -> Result<Value, ToolError> {
    let ctx = ToolContext {
        bot_id: Some(bot.to_owned()),
        ..ToolContext::default()
    };
    call_in(c, &ctx, name, input).await
}

async fn call_in(
    c: &Arc<CalendarConnector>,
    ctx: &ToolContext,
    name: &str,
    input: Value,
) -> Result<Value, ToolError> {
    let reg = ToolRegistry::new();
    assert_eq!(register(&reg, c), 5);
    let out = reg.get(name).unwrap().call(ctx, input).await?;
    match out.content.into_iter().next() {
        Some(ContentPart::Json { data }) => Ok(data),
        other => panic!("unexpected output {other:?}"),
    }
}

fn the_event(f: &Fake) -> String {
    f.events.lock().unwrap().values().next().unwrap().clone()
}

#[tokio::test]
async fn create_list_and_delete_round_trip_with_the_stored_password() {
    let f = fake(false).await;
    let c = connector(f.port);
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Dentist, 2nd floor; bring card","start":"2026-11-20T09:00:00+01:00",
               "end":"2026-11-20T10:00:00+01:00","location":"Main St","description":"line1\nline2"}),
    )
    .await
    .unwrap();
    let uid = made["uid"].as_str().unwrap().to_owned();
    assert_eq!(made["invitations"]["method"], "none");
    // The ics on the wire is valid and escaped, and invites nobody.
    let raw = the_event(&f);
    assert!(
        raw.contains("SUMMARY:Dentist\\, 2nd floor\\; bring card"),
        "{raw}"
    );
    assert!(raw.contains("DTSTART:20261120T080000Z"), "{raw}");
    assert!(!raw.contains("ATTENDEE"));

    let listed = call(
        &c,
        "calendar_list_events",
        json!({"from":"2026-11-20","to":"2026-11-21"}),
    )
    .await
    .unwrap();
    assert_eq!(listed["untrusted_external_content"], true);
    let ev = &listed["events"][0];
    assert_eq!(ev["uid"], uid);
    assert_eq!(ev["summary"], "Dentist, 2nd floor; bring card");
    assert_eq!(ev["start"], "2026-11-20T08:00:00Z");
    assert_eq!(ev["description"], "line1\nline2");

    let del = call(&c, "calendar_delete_event", json!({"uid": uid}))
        .await
        .unwrap();
    assert_eq!(del["deleted"], true);
    assert!(f.events.lock().unwrap().is_empty());
    let again = call(&c, "calendar_delete_event", json!({"uid": uid}))
        .await
        .unwrap();
    assert_eq!(again["deleted"], false);
    // Basic auth carried the secret-store value (base64 of me@example.com:s3cret-pw).
    assert!(
        f.seen_auth
            .lock()
            .unwrap()
            .iter()
            .all(|l| l.ends_with(&"bWVAZXhhbXBsZS5jb206czNjcmV0LXB3".to_ascii_lowercase()))
    );
}

#[tokio::test]
async fn delete_refuses_an_href_on_another_host() {
    let f = fake(true).await;
    let c = connector(f.port);
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"x","start":"2026-11-20","end":"2026-11-21"}),
    )
    .await
    .unwrap();
    let err = call(&c, "calendar_delete_event", json!({"uid": made["uid"]}))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("outside the configured calendar"),
        "{err}"
    );
    assert_eq!(f.events.lock().unwrap().len(), 1);
    // An update goes through the same lookup and is refused the same way.
    let err = call(
        &c,
        "calendar_update_event",
        json!({"uid": made["uid"], "summary": "y"}),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("outside the configured calendar"),
        "{err}"
    );
}

#[test]
fn plain_http_to_a_remote_host_is_refused_and_effects_follow_the_permission_model() {
    let st = store(&[]);
    let mut a = account("x", 1, "");
    a.url = "http://calendar.example.com/dav/".into();
    let (c, problems) = CalendarConnector::new(vec![a], &st);
    assert_eq!(c.names().len(), 0);
    assert!(problems[0].contains("must be https"), "{problems:?}");

    let c = Arc::new(c);
    let specs: Vec<_> = tools(&c).iter().map(|t| t.spec().clone()).collect();
    let effect = |n: &str| specs.iter().find(|s| s.name == n).unwrap().effect;
    // Reads are Read; create and update are Write; delete stays Destructive (irreversible).
    assert_eq!(effect("calendar_list_events"), Effect::Read);
    assert_eq!(effect("calendar_free_busy"), Effect::Read);
    assert_eq!(effect("calendar_create_event"), Effect::Write);
    assert_eq!(effect("calendar_update_event"), Effect::Write);
    assert_eq!(effect("calendar_delete_event"), Effect::Destructive);
    assert_eq!(specs.len(), crate::TOOL_NAMES.len());
    // The tools stay hidden while no calendar is usable.
    assert!(tools(&c).iter().all(|t| !t.visible_to(None)));
}

#[tokio::test]
async fn inviting_or_notifying_people_always_asks_the_user() {
    let f = fake(false).await;
    let all = tools(&connector(f.port));
    let tool = |n: &str| all.iter().find(|t| t.spec().name == n).unwrap();
    let ask = |n: &str, v: Value| tool(n).always_ask_for(&v);
    assert!(!ask(
        "calendar_create_event",
        json!({"summary":"x","start":"2026-11-20","end":"2026-11-21"})
    ));
    assert!(!ask("calendar_create_event", json!({"attendees":[]})));
    assert!(ask(
        "calendar_create_event",
        json!({"attendees":["a@example.com"]})
    ));
    assert!(!ask(
        "calendar_update_event",
        json!({"uid":"u","summary":"x"})
    ));
    assert!(ask(
        "calendar_update_event",
        json!({"uid":"u","attendees":[]})
    ));
    assert!(ask(
        "calendar_update_event",
        json!({"uid":"u","notify_attendees":true})
    ));
    assert!(!ask("calendar_list_events", json!({})));
    assert!(!ask("calendar_free_busy", json!({})));
    // A delete has no always-ask of its own. Like the other tools that act outward it follows
    // the permission level: lower levels ask, the Full level lets a trusted run through (the
    // owner's rule for Full access), and what the server cancels is checked by the mail rules.
    assert!(!ask("calendar_delete_event", json!({"uid":"u"})));
}

#[tokio::test]
async fn every_tool_that_reaches_a_server_declares_egress() {
    let f = fake(false).await;
    let all = tools(&connector(f.port));
    assert_eq!(all.len(), crate::TOOL_NAMES.len());
    for t in &all {
        let spec = t.spec();
        assert!(
            spec.content.egress,
            "{} sends its request to the calendar server",
            spec.name
        );
    }
    // Free/busy sends the requested range to every calendar but reads no outside text.
    let fb = all
        .iter()
        .find(|t| t.spec().name == "calendar_free_busy")
        .unwrap();
    assert!(!fb.spec().content.ingests_untrusted);
}

#[tokio::test]
async fn create_a_repeating_event_with_attendees_on_a_scheduling_server() {
    let f = fake_with(Opts {
        scheduling: true,
        ..Opts::default()
    })
    .await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Standup","start":"2026-11-23T09:00:00Z","end":"2026-11-23T09:15:00Z",
               "recurrence":"RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=3",
               "attendees":["Bob@Example.com","bob@example.com","carol@example.com"]}),
    )
    .await
    .unwrap();
    let raw = the_event(&f);
    assert!(raw.contains("RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=3"), "{raw}");
    assert!(raw.contains("ORGANIZER:mailto:me@example.com"), "{raw}");
    assert_eq!(raw.matches("ATTENDEE;").count(), 2, "{raw}");
    assert!(raw.contains("mailto:bob@example.com"), "{raw}");
    // The server delivers: no e-mail goes out from here.
    assert_eq!(made["invitations"]["method"], "calendar_server");
    assert!(out.mails.lock().unwrap().is_empty());
    // The server was asked what it supports.
    assert!(f.requests.lock().unwrap().iter().any(|r| r.0 == "OPTIONS"));
}

#[tokio::test]
async fn without_server_scheduling_invitations_go_out_as_email_with_an_ics() {
    let f = fake(false).await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Lunch","start":"2026-11-24T12:00:00Z","end":"2026-11-24T13:00:00Z","location":"Cafe",
               "attendees":["bob@example.com"]}),
    )
    .await
    .unwrap();
    assert_eq!(made["invitations"]["method"], "email");
    assert_eq!(made["invitations"]["sent_to"], json!(["bob@example.com"]));
    let sent = out.sent();
    assert_eq!(sent.len(), 1);
    let (to, subject, method, ics) = &sent[0];
    assert_eq!(to, &["bob@example.com"]);
    assert_eq!(subject, "Invitation: Lunch");
    assert_eq!(method, "REQUEST");
    assert!(ics.contains("METHOD:REQUEST"), "{ics}");
    assert!(
        ics.contains("ATTENDEE;") && ics.contains("mailto:bob@example.com"),
        "{ics}"
    );
    assert!(ics.contains("ORGANIZER:mailto:me@example.com"), "{ics}");
}

#[tokio::test]
async fn without_a_mailer_the_event_is_saved_and_the_answer_says_nobody_was_told() {
    let f = fake(false).await;
    let c = connector(f.port);
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Lunch","start":"2026-11-24T12:00:00Z","end":"2026-11-24T13:00:00Z","attendees":["bob@example.com"]}),
    )
    .await
    .unwrap();
    assert_eq!(made["invitations"]["method"], "not_sent");
    assert!(
        made["invitations"]["note"]
            .as_str()
            .unwrap()
            .contains("nobody was told")
    );
    assert!(the_event(&f).contains("mailto:bob@example.com"));
}

#[tokio::test]
async fn bad_attendees_rules_and_missing_organizer_are_refused_before_anything_is_written() {
    let f = fake(false).await;
    let c = connector(f.port);
    for bad in [
        json!({"attendees":["not an address"]}),
        json!({"attendees":["a@b"]}),
        json!({"attendees":["a@example.com\r\nBcc: x@example.com"]}),
        json!({"recurrence":"FREQ=SOMETIMES"}),
        json!({"recurrence":"FREQ=DAILY;COUNT=2;UNTIL=20261231"}),
        json!({"start":"2026-11-24","end":"2026-11-24T10:00:00Z"}),
    ] {
        let mut input =
            json!({"summary":"x","start":"2026-11-24T09:00:00Z","end":"2026-11-24T10:00:00Z"});
        for (k, v) in bad.as_object().unwrap() {
            input[k] = v.clone();
        }
        let err = call(&c, "calendar_create_event", input).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad}: {err}");
    }
    assert!(f.events.lock().unwrap().is_empty());
    // A login that is not an address, and no `email` set: nobody can be invited.
    let mut a = account("home", f.port, "/cal");
    a.username = "plainlogin".into();
    let (c2, _) = CalendarConnector::new(vec![a], &store(&["home"]));
    let err = call(
        &Arc::new(c2),
        "calendar_create_event",
        json!({"summary":"x","start":"2026-11-24T09:00:00Z","end":"2026-11-24T10:00:00Z","attendees":["a@example.com"]}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("e-mail address"), "{err}");
    assert!(f.events.lock().unwrap().is_empty());
}

async fn seed(f: &Fake, c: &Arc<CalendarConnector>) -> String {
    let made = call(
        c,
        "calendar_create_event",
        json!({"summary":"Review","start":"2026-11-25T14:00:00Z","end":"2026-11-25T15:30:00Z","location":"Room 1","description":"Agenda"}),
    )
    .await
    .unwrap();
    // Properties this client does not know must survive an update.
    let uid = made["uid"].as_str().unwrap().to_owned();
    let mut map = f.events.lock().unwrap();
    let ics = map.get_mut(&uid).unwrap();
    *ics = ics.replace(
        "END:VEVENT",
        "X-CUSTOM:keep me\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT",
    );
    uid
}

#[tokio::test]
async fn update_renames_moves_and_keeps_the_length_and_unknown_properties() {
    let f = fake(false).await;
    let c = connector(f.port);
    let uid = seed(&f, &c).await;
    let done = call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "summary":"Design review","start":"2026-11-26T10:00:00Z","location":""}),
    )
    .await
    .unwrap();
    assert_eq!(done["updated"], true);
    assert_eq!(done["invitations"]["method"], "none");
    let raw = the_event(&f);
    assert!(raw.contains("SUMMARY:Design review"), "{raw}");
    assert!(raw.contains("DTSTART:20261126T100000Z"), "{raw}");
    // 90 minutes long, as before.
    assert!(raw.contains("DTEND:20261126T113000Z"), "{raw}");
    assert!(!raw.contains("LOCATION"), "{raw}");
    assert!(raw.contains("DESCRIPTION:Agenda"), "{raw}");
    assert!(raw.contains("X-CUSTOM:keep me"), "{raw}");
    assert!(
        raw.contains("BEGIN:VALARM") && raw.contains("TRIGGER:-PT15M"),
        "{raw}"
    );
    assert!(raw.contains("SEQUENCE:1"), "{raw}");
    assert_eq!(raw.matches("BEGIN:VEVENT").count(), 1);
    // It was a conditional write against the version that was read.
    let put = f
        .requests
        .lock()
        .unwrap()
        .iter()
        .rfind(|r| r.0 == "PUT")
        .unwrap()
        .2
        .clone();
    assert!(put.contains("if-match: \"v1\""), "{put}");

    let listed = call(
        &c,
        "calendar_list_events",
        json!({"from":"2026-11-26","to":"2026-11-27"}),
    )
    .await
    .unwrap();
    assert_eq!(listed["events"][0]["summary"], "Design review");
    assert_eq!(listed["events"][0]["end"], "2026-11-26T11:30:00Z");
}

#[tokio::test]
async fn update_checks_its_input() {
    let f = fake(false).await;
    let c = connector(f.port);
    let uid = seed(&f, &c).await;
    let before = the_event(&f);
    for bad in [
        json!({"uid": uid}),
        json!({"uid": uid, "end":"2026-11-25T13:00:00Z"}),
        json!({"uid": uid, "start":"2026-11-26"}),
        json!({"uid": uid, "recurrence":"nonsense"}),
    ] {
        let err = call(&c, "calendar_update_event", bad.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad}: {err}");
    }
    assert_eq!(the_event(&f), before);
    let gone = call(
        &c,
        "calendar_update_event",
        json!({"uid":"nope@praxi","summary":"x"}),
    )
    .await
    .unwrap();
    assert_eq!(gone["updated"], false);
}

#[tokio::test]
async fn update_can_make_an_event_repeat_and_stop_repeating() {
    let f = fake(false).await;
    let c = connector(f.port);
    let uid = seed(&f, &c).await;
    call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "recurrence":"FREQ=DAILY;COUNT=3"}),
    )
    .await
    .unwrap();
    assert!(the_event(&f).contains("RRULE:FREQ=DAILY;COUNT=3"));
    call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "recurrence":""}),
    )
    .await
    .unwrap();
    assert!(!the_event(&f).contains("RRULE"));
}

#[tokio::test]
async fn changing_attendees_tells_the_new_and_the_removed_and_a_move_needs_consent() {
    let f = fake(false).await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Sync","start":"2026-11-25T14:00:00Z","end":"2026-11-25T15:00:00Z",
               "attendees":["bob@example.com","carol@example.com"]}),
    )
    .await
    .unwrap();
    let uid = made["uid"].as_str().unwrap().to_owned();
    out.mails.lock().unwrap().clear();
    // Mark Bob as having accepted: that answer must survive the edit.
    {
        let mut map = f.events.lock().unwrap();
        let ics = map.values_mut().next().unwrap();
        *ics = ics.replacen("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED", 1);
    }

    // Moving it would tell Bob and Carol: refused until the caller agrees.
    let err = call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "start":"2026-11-25T16:00:00Z"}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("notify_attendees"), "{err}");
    assert!(out.mails.lock().unwrap().is_empty());

    // Swap Carol for Dave.
    let done = call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "attendees":["bob@example.com","dave@example.com"]}),
    )
    .await
    .unwrap();
    assert_eq!(done["invitations"]["method"], "email");
    let raw = the_event(&f);
    assert!(
        raw.contains("mailto:bob@example.com") && raw.contains("mailto:dave@example.com"),
        "{raw}"
    );
    assert!(!raw.contains("carol@example.com"), "{raw}");
    assert!(
        raw.contains("PARTSTAT=ACCEPTED"),
        "Bob's answer is kept: {raw}"
    );
    let sent = out.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    let request = sent.iter().find(|m| m.2 == "REQUEST").unwrap();
    assert_eq!(request.0, ["bob@example.com", "dave@example.com"]);
    let cancel = sent.iter().find(|m| m.2 == "CANCEL").unwrap();
    assert_eq!(cancel.0, ["carol@example.com"]);
    assert!(
        cancel.3.contains("METHOD:CANCEL") && cancel.3.contains("STATUS:CANCELLED"),
        "{}",
        cancel.3
    );
    assert!(
        cancel.3.contains("carol@example.com") && !cancel.3.contains("bob@example.com"),
        "{}",
        cancel.3
    );

    // With consent, a move goes to everyone who is on the event.
    out.mails.lock().unwrap().clear();
    call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "start":"2026-11-25T16:00:00Z","notify_attendees":true}),
    )
    .await
    .unwrap();
    let sent = out.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, ["bob@example.com", "dave@example.com"]);

    // Even a note changes the `SEQUENCE`, and a scheduling server then tells everyone, so a
    // notes-only edit of an event with attendees needs the same consent as a move.
    out.mails.lock().unwrap().clear();
    let before = the_event(&f);
    let err = call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "description":"new notes"}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("notify_attendees"), "{err}");
    assert_eq!(the_event(&f), before, "a refused edit writes nothing");
    assert!(out.mails.lock().unwrap().is_empty());
    call(
        &c,
        "calendar_update_event",
        json!({"uid": uid, "description":"new notes","notify_attendees":true}),
    )
    .await
    .unwrap();
    assert_eq!(out.sent().len(), 1, "the attendees are told of the edit");
}

fn days_with_hours(list: &Value) -> Vec<String> {
    list["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["start"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_repeating_event_is_listed_per_occurrence_whether_or_not_the_server_expands() {
    for expand in [true, false] {
        let f = fake_with(Opts {
            expand,
            ..Opts::default()
        })
        .await;
        let c = connector(f.port);
        call(
            &c,
            "calendar_create_event",
            json!({"summary":"Standup","start":"2026-11-23T09:00:00Z","end":"2026-11-23T09:30:00Z","recurrence":"FREQ=DAILY;COUNT=5"}),
        )
        .await
        .unwrap();
        let listed = call(
            &c,
            "calendar_list_events",
            json!({"from":"2026-11-24","to":"2026-11-27"}),
        )
        .await
        .unwrap();
        assert_eq!(
            days_with_hours(&listed),
            [
                "2026-11-24T09:00:00Z",
                "2026-11-25T09:00:00Z",
                "2026-11-26T09:00:00Z"
            ],
            "server expands: {expand}"
        );
        // A server that expanded the series dropped the rule; the local expansion keeps it.
        assert_eq!(listed["events"][0].get("recurrence").is_some(), !expand);
        assert_eq!(listed["events"][0]["end"], "2026-11-24T09:30:00Z");
    }
}

#[tokio::test]
async fn free_busy_merges_every_calendar_and_reports_one_that_failed() {
    let (home, work) = (
        fake(false).await,
        fake_with(Opts {
            expand: false,
            ..Opts::default()
        })
        .await,
    );
    let accounts = vec![
        account("home", home.port, "/cal"),
        account("work", work.port, "/cal"),
        account("dead", 1, "/cal"),
    ];
    let (c, problems) = CalendarConnector::new(accounts, &store(&["home", "work", "dead"]));
    assert!(problems.is_empty(), "{problems:?}");
    let c = Arc::new(c);
    let make = |account: &str, summary: &str, start: &str, end: &str| json!({"account":account,"summary":summary,"start":start,"end":end});
    call(
        &c,
        "calendar_create_event",
        make(
            "home",
            "Dentist",
            "2026-11-23T09:00:00Z",
            "2026-11-23T10:00:00Z",
        ),
    )
    .await
    .unwrap();
    call(
        &c,
        "calendar_create_event",
        make(
            "work",
            "Planning",
            "2026-11-23T09:30:00Z",
            "2026-11-23T12:00:00Z",
        ),
    )
    .await
    .unwrap();
    // A series on the server that does not expand: the Tuesday occurrence still blocks.
    call(
        &c,
        "calendar_create_event",
        json!({"account":"work","summary":"Standup","start":"2026-11-23T15:00:00Z","end":"2026-11-23T16:00:00Z","recurrence":"FREQ=DAILY;COUNT=2"}),
    )
    .await
    .unwrap();

    let ask = json!({"accounts":["home","work"],"from":"2026-11-23T08:00:00Z","to":"2026-11-23T17:00:00Z","duration_minutes":60,
                     "working_hours":"09:00-17:00","utc_offset":"+00:00"});
    let out = call(&c, "calendar_free_busy", ask).await.unwrap();
    let free: Vec<(String, i64)> = out["free"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["start"].as_str().unwrap().to_owned(),
                s["minutes"].as_i64().unwrap(),
            )
        })
        .collect();
    // Busy 09:00-12:00 (merged across both calendars) and 15:00-16:00.
    assert_eq!(
        free,
        [
            ("2026-11-23T12:00:00+00:00".to_owned(), 180),
            ("2026-11-23T16:00:00+00:00".to_owned(), 60)
        ],
        "{out}"
    );
    assert!(out.get("incomplete").is_none());

    // The whole of Tuesday only has the series occurrence at 15:00.
    let tue = call(&c, "calendar_free_busy", json!({"from":"2026-11-24","to":"2026-11-25","duration_minutes":120,"working_hours":"14:00-18:00"})).await;
    // `dead` is among all calendars: the answer is still given, marked incomplete.
    let tue = tue.unwrap();
    assert_eq!(tue["incomplete"], json!(["dead"]));
    assert_eq!(tue["free"].as_array().unwrap().len(), 1, "{tue}");
    assert_eq!(tue["free"][0]["start"], "2026-11-24T16:00:00+00:00");

    // Only an unreadable calendar: no answer rather than a wrong one.
    let err = call(
        &c,
        "calendar_free_busy",
        json!({"accounts":["dead"],"from":"2026-11-24","to":"2026-11-25"}),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("no calendar could be read"),
        "{err}"
    );
    let err = call(
        &c,
        "calendar_free_busy",
        json!({"from":"2026-11-24","to":"2026-11-25","working_hours":"17:00-09:00"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err}");
}

#[tokio::test]
async fn an_event_marked_free_does_not_block_time() {
    let f = fake(false).await;
    let c = connector(f.port);
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"FYI","start":"2026-11-23T09:00:00Z","end":"2026-11-23T10:00:00Z"}),
    )
    .await
    .unwrap();
    {
        let mut map = f.events.lock().unwrap();
        let ics = map.get_mut(made["uid"].as_str().unwrap()).unwrap();
        *ics = ics.replace("SUMMARY:", "TRANSP:TRANSPARENT\r\nSUMMARY:");
    }
    let out = call(
        &c,
        "calendar_free_busy",
        json!({"from":"2026-11-23T09:00:00Z","to":"2026-11-23T10:00:00Z","duration_minutes":60}),
    )
    .await
    .unwrap();
    assert_eq!(out["free"].as_array().unwrap().len(), 1, "{out}");
}

#[tokio::test]
async fn accounts_can_be_replaced_while_the_tools_are_registered() {
    let f = fake(false).await;
    let reg = ToolRegistry::new();
    let c = Arc::new(CalendarConnector::empty());
    assert_eq!(register(&reg, &c), 5);
    let tool = reg.get("calendar_list_events").unwrap();
    assert!(!tool.visible_to(None));
    let problems = c.replace_accounts(vec![account("home", f.port, "/cal")], &store(&["home"]));
    assert_eq!(problems.len(), 0);
    assert!(tool.visible_to(None));
    assert_eq!(c.names(), ["home"]);
    c.replace_accounts(Vec::new(), &store(&[]));
    assert!(!tool.visible_to(None));
}

#[tokio::test]
async fn discovery_finds_the_calendars_from_the_server_root_or_one_calendar() {
    let f = fake(false).await;
    // From the server root: principal, then calendar home, then the calendars in it.
    let (c, _) = CalendarConnector::new(vec![account("x", f.port, "/")], &store(&["x"]));
    let found = c.pick(None).unwrap().discover().await.unwrap();
    let names: Vec<_> = found.iter().map(|i| i.name.as_str()).collect();
    // The inbox is not a calendar; a calendar without a name is named after its address.
    assert_eq!(names, ["Personal", "work"]);
    assert!(found[0].url.ends_with("/home/personal/"), "{found:?}");
    // From a calendar itself: just that one.
    let (c, _) = CalendarConnector::new(vec![account("x", f.port, "/cal")], &store(&["x"]));
    let found = c.pick(None).unwrap().discover().await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "Only one");
    // A wrong address is a readable error, never a panic or a password in the text.
    let (c, _) =
        CalendarConnector::new(vec![account("x", f.port, "/nothing-here/")], &store(&["x"]));
    let err = c
        .pick(None)
        .unwrap()
        .discover()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not found"), "{err}");
    assert!(!err.contains("s3cret-pw"));
}

// ---- the owner's mail rules, control characters, deletes ----

fn puts(f: &Fake) -> usize {
    f.requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.0 == "PUT")
        .count()
}

fn requests_of(f: &Fake, method: &str) -> usize {
    f.requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.0 == method)
        .count()
}

async fn scheduling_fake() -> Fake {
    fake_with(Opts {
        scheduling: true,
        ..Opts::default()
    })
    .await
}

#[tokio::test]
async fn invitation_mail_is_one_line_whatever_the_event_title_holds() {
    let f = fake(false).await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Lunch\r\nBcc: evil@example.com\u{7}\u{1b}[31m","location":"Cafe\nBack room",
               "start":"2026-11-24T12:00:00Z","end":"2026-11-24T13:00:00Z","attendees":["bob@example.com"]}),
    )
    .await
    .unwrap();
    assert_eq!(made["invitations"]["method"], "email");
    let raw = the_event(&f);
    assert!(
        !raw.chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r'),
        "no control character reaches the stored event: {raw:?}"
    );
    let sent = out.sent();
    let (_, subject, _, _) = &sent[0];
    assert!(!subject.chars().any(char::is_control), "{subject:?}");
    assert_eq!(subject, "Invitation: Lunch Bcc: evil@example.com[31m");
}

#[tokio::test]
async fn a_title_the_server_holds_with_control_characters_cannot_split_a_mail_header() {
    let f = fake(false).await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Sync","start":"2026-11-25T14:00:00Z","end":"2026-11-25T15:00:00Z",
               "attendees":["bob@example.com","carol@example.com"]}),
    )
    .await
    .unwrap();
    // Someone else's client wrote raw control characters and an escaped line break into the title.
    {
        let mut map = f.events.lock().unwrap();
        let ics = map.values_mut().next().unwrap();
        *ics = ics.replacen(
            "SUMMARY:Sync",
            "SUMMARY:Sy\u{7}nc\\nBcc: x@evil.example\u{b}",
            1,
        );
    }
    out.mails.lock().unwrap().clear();
    call(
        &c,
        "calendar_update_event",
        json!({"uid": made["uid"], "attendees":["bob@example.com","dave@example.com"]}),
    )
    .await
    .unwrap();
    let sent = out.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    for (_, subject, _, _) in &sent {
        assert!(!subject.chars().any(char::is_control), "{subject:?}");
        assert!(subject.contains("Sy nc Bcc: x@evil.example"), "{subject:?}");
    }
}

#[tokio::test]
async fn an_address_the_mail_rules_refuse_stops_the_event_before_it_is_written() {
    // Whoever would deliver the invitation (this daemon by mail, or the server by itself),
    // the owner's opt-out list is consulted first.
    for scheduling in [false, true] {
        let f = fake_with(Opts {
            scheduling,
            ..Opts::default()
        })
        .await;
        let c = connector(f.port);
        let out = Outbox::refusing("bob@example.com");
        c.set_mailer(out.clone());
        let input = |who: &[&str]| {
            json!({"summary":"Sync","start":"2026-11-25T14:00:00Z","end":"2026-11-25T15:00:00Z",
                   "attendees": who})
        };
        let err = call_as(
            &c,
            "bot-7",
            "calendar_create_event",
            input(&["carol@example.com", "bob@example.com"]),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("asked not to be written to"),
            "scheduling={scheduling}: {err}"
        );
        assert!(
            f.events.lock().unwrap().is_empty(),
            "scheduling={scheduling}"
        );
        assert_eq!(puts(&f), 0, "nothing is sent to the server: {scheduling}");
        assert!(out.sent().is_empty());
        assert_eq!(
            out.checked.lock().unwrap()[0].0.as_deref(),
            Some("bot-7"),
            "the rules see which bot acts"
        );

        // Without the refused address it goes through; and what the SERVER delivers is logged.
        let made = call_as(
            &c,
            "bot-7",
            "calendar_create_event",
            input(&["carol@example.com"]),
        )
        .await
        .unwrap();
        assert_eq!(puts(&f), 1);
        let server = out.server.lock().unwrap().clone();
        if scheduling {
            assert_eq!(made["invitations"]["method"], "calendar_server");
            assert_eq!(
                server,
                [(
                    Some("bot-7".to_owned()),
                    vec!["carol@example.com".to_owned()],
                    "Invitation: Sync".to_owned()
                )]
            );
            assert!(out.sent().is_empty());
        } else {
            assert_eq!(made["invitations"]["method"], "email");
            assert!(server.is_empty());
            assert_eq!(out.sent().len(), 1);
        }
    }
}

#[tokio::test]
async fn an_update_that_would_tell_a_refused_address_writes_nothing() {
    for scheduling in [false, true] {
        let f = fake_with(Opts {
            scheduling,
            ..Opts::default()
        })
        .await;
        let c = connector(f.port);
        let out = Arc::new(Outbox::default());
        c.set_mailer(out.clone());
        let made = call(
            &c,
            "calendar_create_event",
            json!({"summary":"Sync","start":"2026-11-25T14:00:00Z","end":"2026-11-25T15:00:00Z",
                   "attendees":["bob@example.com","carol@example.com"]}),
        )
        .await
        .unwrap();
        let uid = made["uid"].as_str().unwrap().to_owned();
        let before = the_event(&f);
        *out.refuse.lock().unwrap() = Some("carol@example.com".into());

        // A move tells everyone, Carol too; removing her tells her it is cancelled.
        for patch in [
            json!({"uid": uid, "start":"2026-11-25T16:00:00Z","notify_attendees":true}),
            json!({"uid": uid, "attendees":["bob@example.com"]}),
        ] {
            let err = call(&c, "calendar_update_event", patch.clone())
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("asked not to be written to"),
                "scheduling={scheduling} {patch}: {err}"
            );
            assert_eq!(the_event(&f), before, "{patch}");
        }
        assert_eq!(puts(&f), 1, "only the creation was written: {scheduling}");

        // Once the rules allow it the same edit goes through.
        *out.refuse.lock().unwrap() = None;
        call(
            &c,
            "calendar_update_event",
            json!({"uid": uid, "start":"2026-11-25T16:00:00Z","notify_attendees":true}),
        )
        .await
        .unwrap();
        assert_eq!(puts(&f), 2);
    }
}

#[tokio::test]
async fn deleting_an_event_applies_the_mail_rules_to_what_the_server_cancels() {
    let f = scheduling_fake().await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Standup","start":"2026-11-25T14:00:00Z","end":"2026-11-25T14:30:00Z",
               "attendees":["bob@example.com"]}),
    )
    .await
    .unwrap();
    out.server.lock().unwrap().clear();

    // The owner's rules say no: the event stays, and no DELETE reaches the server.
    *out.refuse.lock().unwrap() = Some("bob@example.com".into());
    let err = call_as(
        &c,
        "bot-7",
        "calendar_delete_event",
        json!({"uid": made["uid"]}),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("asked not to be written to"),
        "{err}"
    );
    assert_eq!(f.events.lock().unwrap().len(), 1);
    assert_eq!(requests_of(&f, "DELETE"), 0);

    // Allowed: it is deleted, the server cancels, and the send log is told who acted.
    *out.refuse.lock().unwrap() = None;
    let done = call_as(
        &c,
        "bot-7",
        "calendar_delete_event",
        json!({"uid": made["uid"]}),
    )
    .await
    .unwrap();
    assert_eq!(done["deleted"], true);
    assert_eq!(done["invitations"]["method"], "calendar_server");
    assert!(f.events.lock().unwrap().is_empty());
    assert_eq!(
        out.server.lock().unwrap().clone(),
        [(
            Some("bot-7".to_owned()),
            vec!["bob@example.com".to_owned()],
            "Canceled: Standup".to_owned()
        )]
    );
}

#[tokio::test]
async fn deleting_says_plainly_when_nobody_was_told_and_leaves_other_peoples_events_alone() {
    // A server that does not schedule: deleted, and the answer says the attendees were not told.
    let f = fake(false).await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Standup","start":"2026-11-25T14:00:00Z","end":"2026-11-25T14:30:00Z",
               "attendees":["bob@example.com"]}),
    )
    .await
    .unwrap();
    out.checked.lock().unwrap().clear();
    let done = call(&c, "calendar_delete_event", json!({"uid": made["uid"]}))
        .await
        .unwrap();
    assert_eq!(done["deleted"], true);
    assert_eq!(done["invitations"]["method"], "not_sent");
    assert!(
        done["invitations"]["note"]
            .as_str()
            .unwrap()
            .contains("not told"),
        "{done}"
    );
    assert!(out.server.lock().unwrap().is_empty());
    assert!(out.checked.lock().unwrap().is_empty());

    // An event somebody else organises: the server cancels nothing for the others.
    let f = scheduling_fake().await;
    let c = connector(f.port);
    let out = Arc::new(Outbox::default());
    c.set_mailer(out.clone());
    let made = call(
        &c,
        "calendar_create_event",
        json!({"summary":"Board","start":"2026-11-25T14:00:00Z","end":"2026-11-25T15:00:00Z",
               "attendees":["bob@example.com"]}),
    )
    .await
    .unwrap();
    {
        let mut map = f.events.lock().unwrap();
        let ics = map.values_mut().next().unwrap();
        *ics = ics.replace(
            "ORGANIZER:mailto:me@example.com",
            "ORGANIZER:mailto:boss@example.com",
        );
    }
    out.server.lock().unwrap().clear();
    let done = call(&c, "calendar_delete_event", json!({"uid": made["uid"]}))
        .await
        .unwrap();
    assert_eq!(done["deleted"], true);
    assert_eq!(done["invitations"]["method"], "none");
    assert!(out.server.lock().unwrap().is_empty());

    // Nothing to delete is not an error.
    let gone = call(&c, "calendar_delete_event", json!({"uid":"no-such-event"}))
        .await
        .unwrap();
    assert_eq!(gone["deleted"], false);
}

#[tokio::test]
async fn a_uid_is_plain_text_before_it_goes_into_a_query() {
    let f = fake(false).await;
    let c = connector(f.port);
    for bad in ["", "a\u{0}b", "line\nbreak", &"x".repeat(300)] {
        for tool in ["calendar_delete_event", "calendar_update_event"] {
            let err = call(&c, tool, json!({"uid": bad, "summary":"x"}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidInput(_)),
                "{tool} {bad:?}: {err}"
            );
        }
    }
    assert_eq!(
        requests_of(&f, "REPORT"),
        0,
        "nothing was asked of the server"
    );
}

// ---- where a calendar may be dialled ----

#[test]
fn plain_http_is_for_this_machine_only_and_judged_on_the_parsed_host() {
    let st = store(&[]);
    let problems = |url: &str| {
        let mut a = account("x", 1, "");
        a.url = url.into();
        CalendarConnector::new(vec![a], &st).1
    };
    // `127.attacker.com` only starts like loopback; the password must never go there in clear.
    for bad in [
        "http://127.attacker.com/dav/",
        "http://127.0.0.1.nip.io/dav/",
        "http://localhost.attacker.com/dav/",
        "http://10.0.0.5/dav/",
    ] {
        let p = problems(bad);
        assert!(
            p.iter().any(|m| m.contains("must be https")),
            "{bad}: {p:?}"
        );
    }
    for ok in [
        "http://localhost:5232/",
        "http://127.0.0.1:5232/dav/",
        "http://[::1]:5232/",
        "http://dav.localhost/",
    ] {
        assert!(problems(ok).is_empty(), "{ok}");
    }
}

#[test]
fn addresses_in_metadata_and_link_local_space_are_refused_even_over_https() {
    let st = store(&[]);
    for bad in [
        "https://169.254.169.254/latest/meta-data/",
        "https://[::ffff:169.254.169.254]/",
        "https://[fe80::1]/",
        "https://0.0.0.0/",
        "https://2852039166/",
    ] {
        let mut a = account("x", 1, "");
        a.url = bad.into();
        let (c, problems) = CalendarConnector::new(vec![a], &st);
        assert!(c.names().is_empty(), "{bad}");
        assert_eq!(problems.len(), 1, "{bad}: {problems:?}");
    }
}

#[tokio::test]
async fn the_daemons_own_port_is_never_dialled_and_nothing_is_sent_there() {
    // The fake stands in for the daemon's API: its port is the protected one.
    let f = fake(false).await;
    let st = store(&["home"]);
    let policy = calendar_policy(&[std::net::SocketAddr::from(([127, 0, 0, 1], f.port))]);
    let conn = CalendarConnector::with_policy(policy);
    // The address, the name of this machine and the other spellings of it are all refused when
    // the account is built (so `POST /v1/calendar/test` cannot probe the daemon's own port).
    for url in [
        format!("http://127.0.0.1:{}/cal", f.port),
        format!("http://localhost:{}/cal", f.port),
        format!("http://dav.localhost:{}/cal", f.port),
        format!("http://2130706433:{}/cal", f.port),
        format!("http://[::ffff:127.0.0.1]:{}/cal", f.port),
    ] {
        let mut a = account("home", f.port, "/cal");
        a.url = url.clone();
        let problems = conn.replace_accounts(vec![a], &st);
        assert_eq!(problems.len(), 1, "{url}: {problems:?}");
        assert!(conn.names().is_empty(), "{url}");
    }
    // Another port of this machine (a Radicale next door) is still a calendar.
    let mut other = account("home", f.port, "/cal");
    other.url = "http://127.0.0.1:5232/cal".into();
    assert!(conn.replace_accounts(vec![other], &st).is_empty());
    assert_eq!(conn.names(), ["home"]);
    assert!(
        f.requests.lock().unwrap().is_empty(),
        "not one request reached the protected port: {:?}",
        f.requests.lock().unwrap()
    );
}

#[tokio::test]
async fn a_discovery_link_stays_on_the_configured_host() {
    let st = store(&[]);
    let client = |url: &str| {
        let mut a = account("x", 1, "");
        a.url = url.into();
        a.password = SecretRef::new("calendar/x/password").unwrap();
        crate::CalendarClient::new(a, st.clone()).unwrap()
    };
    // (configured address, link the server sent, allowed)
    for (base, link, allowed) in [
        // the same host, a path or a full link
        ("https://dav.example.com/dav/", "/dav/home/", true),
        (
            "https://dav.example.com/dav/",
            "https://dav.example.com/other/",
            true,
        ),
        (
            "https://DAV.example.com./dav/",
            "https://dav.example.com/other/",
            true,
        ),
        // another host is refused, however alike the last labels
        ("https://a.co.uk/dav/", "https://evil.co.uk/dav/", false),
        (
            "https://dav.example.com/dav/",
            "https://evil.example.com/dav/",
            false,
        ),
        (
            "https://dav.example.com/dav/",
            "https://example.com/dav/",
            false,
        ),
        (
            "https://dav.example.com/dav/",
            "https://dav.example.com.evil.net/",
            false,
        ),
        // address literals never match by their last octets
        (
            "https://10.0.169.254/dav/",
            "https://169.254.169.254/dav/",
            false,
        ),
        ("https://10.0.0.5/dav/", "https://10.0.0.6/dav/", false),
        // scheme, port and a login in the link are never changed
        (
            "https://dav.example.com/dav/",
            "http://dav.example.com/dav/",
            false,
        ),
        (
            "https://dav.example.com/dav/",
            "https://dav.example.com:8443/dav/",
            false,
        ),
        (
            "https://dav.example.com/dav/",
            "https://u:p@dav.example.com/dav/",
            false,
        ),
        (
            "https://dav.example.com/dav/",
            "https://u@dav.example.com/dav/",
            false,
        ),
        // a service that spreads calendars over its own hosts may move between them
        (
            "https://caldav.icloud.com/",
            "https://p42-caldav.icloud.com/123/",
            true,
        ),
        (
            "https://p42-caldav.icloud.com/",
            "https://p51-caldav.icloud.com/9/",
            true,
        ),
        // ... but not to a look-alike, and not out of the service
        (
            "https://caldav.icloud.com/",
            "https://p42-caldav.icloud.com.evil.net/",
            false,
        ),
        (
            "https://caldav.icloud.com/",
            "https://evilicloud.com/",
            false,
        ),
        (
            "https://caldav.icloud.com/",
            "https://p42-caldav.fastmail.com/",
            false,
        ),
        (
            "https://dav.example.com/dav/",
            "https://p42-caldav.icloud.com/",
            false,
        ),
        // never into metadata space, whatever the host says
        (
            "https://dav.example.com/dav/",
            "https://169.254.169.254/",
            false,
        ),
    ] {
        let c = client(base);
        let from = url::Url::parse(base).unwrap();
        let got = c.discovered(&from, link);
        assert_eq!(got.is_ok(), allowed, "{base} -> {link}: {got:?}");
    }
}

// ---- a hostile server ----

/// Read one request (head and body) off `sock`.
async fn read_request(sock: &mut tokio::net::TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len = head
                .to_ascii_lowercase()
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0usize);
            if body.len() >= len {
                return;
            }
        }
    }
}

/// A server that answers every request with `reply` as it is (head included).
async fn serve_bytes(reply: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let reply = reply.clone();
            tokio::spawn(async move {
                read_request(&mut sock).await;
                let _ = sock.write_all(&reply).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

/// A server that streams a body without end (up to `limit` bytes, then it gives up).
async fn serve_endless(limit: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                read_request(&mut sock).await;
                let head = "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n<d:multistatus xmlns:d=\"DAV:\">";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let filler = vec![b' '; 64 << 10];
                let mut sent = 0;
                while sent < limit && sock.write_all(&filler).await.is_ok() {
                    sent += filler.len();
                }
            });
        }
    });
    port
}

const LIST_ARGS: &str = r#"{"from":"2026-11-01","to":"2026-12-01"}"#;

#[tokio::test]
async fn a_response_is_read_up_to_the_cap_and_no_further() {
    // A body that never ends: the read stops at the cap with an answer, and does not hang or grow.
    let port = serve_endless(256 << 20).await;
    let c = connector(port);
    let started = std::time::Instant::now();
    let err = call(
        &c,
        "calendar_list_events",
        serde_json::from_str(LIST_ARGS).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("response too large"), "{err}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );

    // A length that announces more than the cap is refused before a byte of it is read.
    let head = "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: 10485760\r\nConnection: close\r\n\r\n";
    let port = serve_bytes(head.as_bytes().to_vec()).await;
    let c = connector(port);
    let err = call(
        &c,
        "calendar_list_events",
        serde_json::from_str(LIST_ARGS).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("response too large"), "{err}");
}

#[tokio::test]
async fn an_answer_that_expands_to_too_many_events_is_refused_not_listed() {
    let mut ics = String::from("BEGIN:VCALENDAR\r\n");
    for i in 0..10_500 {
        ics.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:e{i}\r\nDTSTART:20261120T090000Z\r\nDTEND:20261120T100000Z\r\nEND:VEVENT\r\n"
        ));
    }
    ics.push_str("END:VCALENDAR\r\n");
    let xml = format!(
        "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:response><d:href>/cal/a.ics</d:href><d:propstat><d:prop><c:calendar-data>{ics}</c:calendar-data></d:prop></d:propstat></d:response></d:multistatus>"
    );
    assert!(
        xml.len() < 4 << 20,
        "the body itself is within the cap: {}",
        xml.len()
    );
    let reply = format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
        xml.len()
    );
    let port = serve_bytes(reply.into_bytes()).await;
    let c = connector(port);
    let err = call(
        &c,
        "calendar_list_events",
        serde_json::from_str(LIST_ARGS).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("too many events"), "{err}");
}

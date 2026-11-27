//! Just enough iCalendar (RFC 5545): read `VEVENT`s, write one, and patch an
//! existing one without losing the properties we do not understand.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Serialize;

use crate::rrule;

/// One invitee as shown to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attendee {
    /// Address (`mailto:` removed).
    pub email: String,
    /// Display name, if the event carries one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `NEEDS-ACTION`, `ACCEPTED`, `DECLINED`, `TENTATIVE`, ... when known.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
}

/// One event as shown to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    /// The event's `UID` (what `calendar_update_event` and `calendar_delete_event` take).
    pub uid: String,
    /// Title.
    pub summary: String,
    /// Start: `YYYY-MM-DD` for an all-day event, else `YYYY-MM-DDTHH:MM:SS` (+`Z` for UTC).
    pub start: String,
    /// End, same format; empty when the event has none.
    pub end: String,
    /// Place, if any.
    pub location: String,
    /// Free text, if any (untrusted: written by whoever made the event).
    pub description: String,
    /// The recurrence rule (`FREQ=WEEKLY;BYDAY=MO`) of a series; empty for a single event.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub recurrence: String,
    /// Organizer address, if the event has attendees.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub organizer: String,
    /// Invitees, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<Attendee>,
    /// `false` for an event marked as not blocking time (`TRANSP:TRANSPARENT`) or cancelled.
    #[serde(skip_serializing_if = "is_true")]
    pub busy: bool,
    /// Start and end as instants, for free/busy arithmetic.
    #[serde(skip)]
    pub span: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes `&bool`
fn is_true(v: &bool) -> bool {
    *v
}

/// Largest folded line length in octets (RFC 5545 section 3.1).
const FOLD_AT: usize = 75;

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        lines.push(raw.to_owned());
    }
    lines
}

/// Fold one logical line at 75 octets, never inside a character.
fn fold(line: &str) -> String {
    if line.len() <= FOLD_AT {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + line.len() / FOLD_AT * 3);
    let mut width = 0usize;
    for c in line.chars() {
        // A continuation line starts with one space that counts toward its length.
        if width + c.len_utf8() > FOLD_AT {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

fn escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push(c),
            // Other control characters (and a bare CR) cannot be written in a content line.
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A content line split into name, parameters and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prop {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Prop {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// Split at `sep` outside double quotes.
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut quoted, mut from) = (Vec::new(), false, 0);
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            out.push(&s[from..i]);
            from = i + c.len_utf8();
        }
    }
    out.push(&s[from..]);
    out
}

fn parse_prop(line: &str) -> Option<Prop> {
    // The first colon outside quotes ends the name and parameters.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        (c == ':' && !quoted).then_some(i)
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            (
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_owned(),
            )
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_owned(),
    })
}

/// The top-level properties of one `VEVENT` (sub-components such as alarms are skipped).
#[derive(Debug, Clone, Default)]
pub(crate) struct Vevent {
    pub props: Vec<Prop>,
}

impl Vevent {
    pub fn get(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> {
        self.props.iter().filter(move |p| p.name == name)
    }

    fn text(&self, name: &str) -> String {
        self.get(name)
            .map(|p| unescape(&p.value))
            .unwrap_or_default()
    }

    pub fn uid(&self) -> String {
        self.get("UID")
            .map(|p| p.value.trim().to_owned())
            .unwrap_or_default()
    }

    pub fn start(&self) -> Option<Moment> {
        self.get("DTSTART").and_then(Moment::from_prop)
    }

    /// The end, from `DTEND` or else `DTSTART` + `DURATION` (all-day: one day).
    pub fn end(&self) -> Option<Moment> {
        if let Some(e) = self.get("DTEND").and_then(Moment::from_prop) {
            return Some(e);
        }
        let start = self.start()?;
        if let Some(d) = self.get("DURATION").and_then(|p| parse_duration(&p.value)) {
            return start.plus(d);
        }
        match start {
            Moment::Date(_) => start.plus(Duration::days(1)),
            _ => None,
        }
    }

    /// Whether this event blocks time.
    pub fn busy(&self) -> bool {
        let transparent = self
            .get("TRANSP")
            .is_some_and(|p| p.value.trim().eq_ignore_ascii_case("TRANSPARENT"));
        let cancelled = self
            .get("STATUS")
            .is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED"));
        !transparent && !cancelled
    }

    pub fn attendees(&self) -> Vec<Attendee> {
        self.all("ATTENDEE")
            .map(|p| Attendee {
                email: mailto(&p.value),
                name: p.param("CN").unwrap_or_default().to_owned(),
                status: p.param("PARTSTAT").unwrap_or_default().to_ascii_uppercase(),
            })
            .filter(|a| !a.email.is_empty())
            .collect()
    }

    pub fn to_event(&self) -> Event {
        Event {
            uid: self.uid(),
            summary: self.text("SUMMARY"),
            start: self.start().map(Moment::display).unwrap_or_default(),
            end: self.end().map(Moment::display).unwrap_or_default(),
            location: self.text("LOCATION"),
            description: self.text("DESCRIPTION"),
            recurrence: self
                .get("RRULE")
                .map(|p| p.value.trim().to_owned())
                .unwrap_or_default(),
            organizer: self
                .get("ORGANIZER")
                .map(|p| mailto(&p.value))
                .unwrap_or_default(),
            attendees: self.attendees(),
            busy: self.busy(),
            span: self
                .start()
                .and_then(|s| Some((s.to_utc()?, self.end()?.to_utc()?))),
        }
    }
}

/// `mailto:Bob@Example.com` to `bob@example.com`.
fn mailto(v: &str) -> String {
    let v = v.trim();
    let v = v
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("mailto:"))
        .map_or(v, |_| &v[7..]);
    v.trim().to_ascii_lowercase()
}

/// A date or time as written in an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Moment {
    /// An all-day date.
    Date(NaiveDate),
    /// An absolute instant.
    Utc(DateTime<Utc>),
    /// Wall-clock time in a zone, or floating (no zone) when `None`.
    Local(NaiveDateTime, Option<Tz>),
}

impl Moment {
    pub fn from_prop(p: &Prop) -> Option<Self> {
        let v = p.value.trim();
        if p.param("VALUE")
            .is_some_and(|x| x.eq_ignore_ascii_case("DATE"))
            || v.len() == 8
        {
            return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Self::Date);
        }
        let (body, z) = v.strip_suffix(['Z', 'z']).map_or((v, false), |b| (b, true));
        let n = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
        if z {
            return Some(Self::Utc(n.and_utc()));
        }
        let tz = p.param("TZID").and_then(|t| t.parse::<Tz>().ok());
        Some(Self::Local(n, tz))
    }

    /// The instant a calendar server would use (a day starts at 00:00 UTC; a floating time is read as UTC).
    pub fn to_utc(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Date(d) => Some(d.and_hms_opt(0, 0, 0)?.and_utc()),
            Self::Utc(t) => Some(t),
            Self::Local(n, Some(tz)) => tz
                .from_local_datetime(&n)
                .earliest()
                .map(|t| t.with_timezone(&Utc)),
            Self::Local(n, None) => Some(n.and_utc()),
        }
    }

    pub fn is_date(self) -> bool {
        matches!(self, Self::Date(_))
    }

    /// `self` moved by `d`; `None` when that leaves the range of dates (a hostile
    /// `DURATION` or `DTSTART` must not be able to panic the daemon).
    pub fn plus(self, d: Duration) -> Option<Self> {
        match self {
            Self::Date(x) => x
                .checked_add_signed(Duration::try_days(d.num_days().max(1))?)
                .map(Self::Date),
            Self::Utc(t) => t.checked_add_signed(d).map(Self::Utc),
            Self::Local(n, tz) => n.checked_add_signed(d).map(|n| Self::Local(n, tz)),
        }
    }

    pub fn display(self) -> String {
        match self {
            Self::Date(d) => d.format("%Y-%m-%d").to_string(),
            Self::Utc(t) => t.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            Self::Local(n, tz) => match self.to_utc().filter(|_| tz.is_some()) {
                Some(t) => t.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                None => n.format("%Y-%m-%dT%H:%M:%S").to_string(),
            },
        }
    }
}

/// `P1D`, `PT1H30M`, `-P1W` and friends.
pub(crate) fn parse_duration(v: &str) -> Option<Duration> {
    let v = v.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let (mut secs, mut num, mut in_time) = (0i64, String::new(), false);
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let unit: i64 = match (c, in_time) {
                    ('W', false) => 7 * 86_400,
                    ('D', false) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                secs = secs.checked_add(n.checked_mul(unit)?)?;
            }
            _ => return None,
        }
    }
    if !num.is_empty() {
        return None;
    }
    Duration::try_seconds(if neg { -secs } else { secs })
}

/// Every top-level `VEVENT` of a document, in order.
pub(crate) fn vevents(text: &str) -> Vec<Vevent> {
    let mut out = Vec::new();
    let mut cur: Option<Vevent> = None;
    let mut depth = 0usize;
    for line in unfold(text) {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VEVENT" {
            cur = Some(Vevent::default());
            depth = 0;
            continue;
        }
        if upper == "END:VEVENT" {
            out.extend(cur.take());
            continue;
        }
        let Some(ev) = cur.as_mut() else { continue };
        if upper.starts_with("BEGIN:") {
            depth += 1;
            continue;
        }
        if upper.starts_with("END:") {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 0
            && let Some(p) = parse_prop(&line)
        {
            ev.props.push(p);
        }
    }
    out
}

/// All `VEVENT`s of one iCalendar document, as written (a series is one event).
#[must_use]
pub fn parse_events(text: &str) -> Vec<Event> {
    vevents(text).iter().map(Vevent::to_event).collect()
}

/// What one server answer may cost: how many events it may expand to and how many
/// steps may be spent walking recurrence rules. A hostile `.ics` (a million `VEVENT`s,
/// rules that never produce a date) runs out of it instead of the daemon's memory or CPU.
#[derive(Debug, Clone)]
pub struct Budget {
    events: usize,
    work: u64,
    exhausted: bool,
}

impl Budget {
    /// At most `events` events and `work` rule-walking steps.
    #[must_use]
    pub const fn new(events: usize, work: u64) -> Self {
        Self {
            events,
            work,
            exhausted: false,
        }
    }

    /// Whether anything was cut off.
    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Room for one more event?
    pub(crate) fn take_event(&mut self) -> bool {
        if let Some(left) = self.events.checked_sub(1) {
            self.events = left;
            true
        } else {
            self.exhausted = true;
            false
        }
    }

    /// Pay for `steps` of rule walking; `false` when the budget is gone.
    pub(crate) fn spend(&mut self, steps: u64) -> bool {
        if let Some(left) = self.work.checked_sub(steps) {
            self.work = left;
            true
        } else {
            self.exhausted = true;
            false
        }
    }
}

/// The events of `text` that overlap `[from, to)`. A server that honoured
/// `<c:expand>` already sent one `VEVENT` per occurrence; one that did not sent
/// the series master with its `RRULE`, which is expanded here. `budget` is shared
/// by every document of one answer; check [`Budget::exhausted`] afterwards.
#[must_use]
pub fn events_in_range(
    text: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    budget: &mut Budget,
) -> Vec<Event> {
    let all = vevents(text);
    // The occurrences of a series that an override moved or cancelled, by `UID`.
    let mut moved: HashMap<String, Vec<DateTime<Utc>>> = HashMap::new();
    for v in &all {
        if let Some(id) = v
            .get("RECURRENCE-ID")
            .and_then(Moment::from_prop)
            .and_then(Moment::to_utc)
        {
            moved.entry(v.uid()).or_default().push(id);
        }
    }
    let mut out = Vec::new();
    for v in &all {
        if budget.exhausted() {
            break;
        }
        if v.get("RRULE").is_none() || v.get("RECURRENCE-ID").is_some() {
            if budget.take_event() {
                out.push(v.to_event());
            }
            continue;
        }
        let skip = moved.get(&v.uid()).map_or(&[][..], Vec::as_slice);
        match rrule::occurrences(v, from, to, skip, budget) {
            Some(list) => {
                for (s, e) in list {
                    if !budget.take_event() {
                        break;
                    }
                    let mut ev = v.to_event();
                    ev.start = s.display();
                    ev.end = e.display();
                    ev.span = s.to_utc().zip(e.to_utc());
                    out.push(ev);
                }
            }
            // A rule we cannot expand: the series master is better than nothing.
            None => {
                if budget.take_event() {
                    out.push(v.to_event());
                }
            }
        }
    }
    out
}

/// A moment the agent gave: RFC 3339 or a plain date (all-day).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// `YYYY-MM-DD`.
    Day(NaiveDate),
    /// An instant.
    At(DateTime<Utc>),
}

impl When {
    /// Parse `YYYY-MM-DD` or an RFC 3339 timestamp (any offset).
    ///
    /// # Errors
    /// A message naming the accepted formats.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            return Ok(Self::Day(d));
        }
        DateTime::parse_from_rfc3339(s)
            .map(|d| Self::At(d.with_timezone(&Utc)))
            .map_err(|_| {
                format!(
                    "`{s}` is not a date: use YYYY-MM-DD or RFC 3339 (2026-11-20T09:00:00+01:00)"
                )
            })
    }

    fn ical(self) -> (&'static str, String) {
        match self {
            Self::Day(d) => (";VALUE=DATE", d.format("%Y%m%d").to_string()),
            Self::At(t) => ("", t.format("%Y%m%dT%H%M%SZ").to_string()),
        }
    }

    /// Instant used for `CalDAV` time ranges (a day starts at 00:00 UTC).
    #[must_use]
    pub fn utc(self) -> DateTime<Utc> {
        match self {
            Self::Day(d) => d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc(),
            Self::At(t) => t,
        }
    }

    fn moment(self) -> Moment {
        match self {
            Self::Day(d) => Moment::Date(d),
            Self::At(t) => Moment::Utc(t),
        }
    }
}

/// Check a recurrence rule and return it without a leading `RRULE:`.
///
/// # Errors
/// A message naming what is wrong.
pub fn clean_rrule(raw: &str) -> Result<String, String> {
    let r = raw.trim();
    let r = r.strip_prefix("RRULE:").unwrap_or(r).trim();
    rrule::parse(r).map(|_| r.to_ascii_uppercase())
}

/// Check and normalise an invitee address.
///
/// # Errors
/// A message naming the bad address.
pub fn clean_address(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let a = trimmed
        .strip_prefix("mailto:")
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    let bad = || format!("`{raw}` is not an e-mail address");
    let (local, domain) = a.split_once('@').ok_or_else(bad)?;
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-'=!#$&*/?^`{|}~".contains(c));
    let domain_ok = domain.contains('.')
        && domain.split('.').all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if a.len() > 254 || !local_ok || !domain_ok {
        return Err(bad());
    }
    Ok(a)
}

/// What a new event is made of.
#[derive(Debug, Clone, Copy)]
pub struct EventSpec<'a> {
    /// `UID` of the event.
    pub uid: &'a str,
    /// Title.
    pub summary: &'a str,
    /// Start.
    pub start: When,
    /// End (exclusive).
    pub end: When,
    /// Place.
    pub location: Option<&'a str>,
    /// Notes.
    pub description: Option<&'a str>,
    /// A cleaned `RRULE` value.
    pub rrule: Option<&'a str>,
    /// Organizer address; required when there are attendees.
    pub organizer: Option<&'a str>,
    /// Cleaned invitee addresses.
    pub attendees: &'a [String],
}

fn attendee_line(email: &str) -> String {
    format!(
        "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{email}"
    )
}

fn now_stamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn join(lines: &[String]) -> String {
    let mut s = lines
        .iter()
        .map(|l| fold(l))
        .collect::<Vec<_>>()
        .join("\r\n");
    s.push_str("\r\n");
    s
}

/// Build the `.ics` body of a new event.
#[must_use]
pub fn build_event(e: &EventSpec<'_>) -> String {
    let (sp, sv) = e.start.ical();
    let (ep, ev) = e.end.ical();
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//Praxi//Calendar//EN".to_owned(),
        "BEGIN:VEVENT".to_owned(),
        format!("UID:{}", e.uid),
        format!("DTSTAMP:{}", now_stamp()),
        "SEQUENCE:0".to_owned(),
        format!("DTSTART{sp}:{sv}"),
        format!("DTEND{ep}:{ev}"),
        format!("SUMMARY:{}", escape(e.summary)),
    ];
    if let Some(l) = e.location.filter(|l| !l.is_empty()) {
        lines.push(format!("LOCATION:{}", escape(l)));
    }
    if let Some(d) = e.description.filter(|d| !d.is_empty()) {
        lines.push(format!("DESCRIPTION:{}", escape(d)));
    }
    if let Some(r) = e.rrule.filter(|r| !r.is_empty()) {
        lines.push(format!("RRULE:{r}"));
    }
    if !e.attendees.is_empty() {
        if let Some(o) = e.organizer {
            lines.push(format!("ORGANIZER:mailto:{o}"));
        }
        lines.extend(e.attendees.iter().map(|a| attendee_line(a)));
    }
    lines.push("END:VEVENT".to_owned());
    lines.push("END:VCALENDAR".to_owned());
    join(&lines)
}

/// What to change in an existing event. `None` keeps the stored value; an
/// empty string removes a text field or the recurrence.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    /// New title.
    pub summary: Option<String>,
    /// New place.
    pub location: Option<String>,
    /// New notes.
    pub description: Option<String>,
    /// New start. Without `end` the event keeps its length.
    pub start: Option<When>,
    /// New end.
    pub end: Option<When>,
    /// New cleaned `RRULE` value; empty makes it a single event.
    pub rrule: Option<String>,
    /// The full new invitee list (cleaned addresses).
    pub attendees: Option<Vec<String>>,
    /// Organizer to write when invitees are added to an event that has none.
    pub organizer: Option<String>,
    /// The caller agreed that a change to an event with invitees tells them.
    pub notify: bool,
}

/// An event after [`edit`].
#[derive(Debug, Clone)]
pub struct Edited {
    /// The new `.ics` body.
    pub ics: String,
    /// Invitees now on the event.
    pub attendees: Vec<String>,
    /// Invitees that were on it and no longer are.
    pub removed: Vec<String>,
    /// Invitees added by this edit.
    pub added: Vec<String>,
    /// Whether anything the invitees care about (time, place, title, rule, list) changed.
    pub material: bool,
    /// The organizer written on the event, if any.
    pub organizer: String,
    /// The `UID`.
    pub uid: String,
}

fn same_kind(a: Moment, b: Moment) -> bool {
    a.is_date() == b.is_date()
}

fn length(start: Moment, end: Moment) -> Option<Duration> {
    if !same_kind(start, end) {
        return None;
    }
    match (start, end) {
        (Moment::Local(a, ta), Moment::Local(b, tb)) if ta == tb => Some(b - a),
        _ => Some(end.to_utc()? - start.to_utc()?),
    }
}

/// The `VEVENT` that holds the series: the first one that is not a changed single occurrence.
/// Returns the indexes of its `BEGIN` and `END` lines.
fn master_block(lines: &[String]) -> Result<(usize, usize), String> {
    let mut at = 0;
    while at < lines.len() {
        if lines[at].eq_ignore_ascii_case("BEGIN:VEVENT") {
            let close = (at..lines.len())
                .find(|&i| lines[i].eq_ignore_ascii_case("END:VEVENT"))
                .ok_or("the stored event is cut short")?;
            let block = lines[at..=close].join("\n");
            if !block.to_ascii_uppercase().contains("\nRECURRENCE-ID") {
                return Ok((at, close));
            }
            at = close;
        }
        at += 1;
    }
    Err("the stored calendar has no event that is not a changed single occurrence".into())
}

/// The document with the master block's properties in `drop` removed (except the
/// attendees in `keep`) and `add` written before its `END`. Nested components stay whole.
fn rebuild(
    lines: &[String],
    (open, close): (usize, usize),
    drop: &[&str],
    keep: &[String],
    add: Vec<String>,
) -> Vec<String> {
    let mut out: Vec<String> = lines[..=open].to_vec();
    let mut depth = 0usize;
    for line in &lines[open + 1..close] {
        let up = line.to_ascii_uppercase();
        if up.starts_with("BEGIN:") {
            depth += 1;
        }
        let nested = depth > 0;
        if up.starts_with("END:") {
            depth = depth.saturating_sub(1);
        }
        if nested || up.starts_with("END:") {
            out.push(line.clone());
            continue;
        }
        match parse_prop(line) {
            Some(p) if drop.contains(&p.name.as_str()) => {
                if p.name == "ATTENDEE" && keep.contains(&mailto(&p.value)) {
                    out.push(line.clone());
                }
            }
            _ => out.push(line.clone()),
        }
    }
    out.extend(add);
    out.extend_from_slice(&lines[close..]);
    out
}

/// What an edit removes from the master block and writes instead.
struct Plan {
    drop: Vec<&'static str>,
    add: Vec<String>,
    /// Something the invitees see changes (any edit does: the `SEQUENCE` moves and a
    /// server that schedules tells them).
    material: bool,
}

impl Plan {
    fn text(&mut self, name: &'static str, value: Option<&String>) {
        let Some(v) = value else { return };
        self.drop.push(name);
        if !v.is_empty() {
            self.add.push(format!("{name}:{}", escape(v)));
        }
        self.material = true;
    }

    /// Start and end: the length is kept when only the start moves.
    fn times(&mut self, old: &Vevent, patch: &Patch) -> Result<(), String> {
        if patch.start.is_none() && patch.end.is_none() {
            return Ok(());
        }
        let both = "`start` and `end` must both be dates or both be times";
        let (old_start, old_end) = (old.start(), old.end());
        let new_end = match (patch.start, patch.end) {
            (Some(s), Some(e)) if s.moment().is_date() != e.moment().is_date() => {
                return Err(both.into());
            }
            (_, Some(end)) => Some(end.moment()),
            (Some(start), None) => {
                let same = old_start.is_some_and(|o| same_kind(o, start.moment()));
                if old.get("DTEND").is_none() && old.get("DURATION").is_some() && same {
                    None // the DURATION stays and moves with the start
                } else {
                    let kept = old_start
                        .zip(old_end)
                        .and_then(|(s, e)| length(s, e))
                        .filter(|_| same)
                        .ok_or("`end` is needed here: the start changed between a whole day and a time")?;
                    Some(
                        start
                            .moment()
                            .plus(kept)
                            .ok_or("the new end is outside the dates a calendar can hold")?,
                    )
                }
            }
            (None, None) => None,
        };
        let new_start = patch.start.map(When::moment).or(old_start);
        if let Some((s, e)) = new_start.zip(new_end.or(old_end)) {
            if !same_kind(s, e) {
                return Err(both.into());
            }
            if s.to_utc().zip(e.to_utc()).is_some_and(|(a, z)| z <= a) {
                return Err("`end` must be after `start`".into());
            }
        }
        self.material = true;
        self.drop.extend(["DTSTART", "DTEND", "DURATION"]);
        if let Some(s) = new_start {
            let (p, v) = patch.start.unwrap_or_else(|| as_when(s)).ical();
            self.add.push(format!("DTSTART{p}:{v}"));
        }
        match (new_end, old.get("DURATION"), old_end) {
            (Some(e), _, _) | (None, None, Some(e)) => {
                let (p, v) = as_when(e).ical();
                self.add.push(format!("DTEND{p}:{v}"));
            }
            // Only the start moved and the event is defined by a DURATION: put it back.
            (None, Some(d), _) => self.add.push(format!("DURATION:{}", d.value.trim())),
            (None, None, None) => {}
        }
        Ok(())
    }
}

/// Who is on the event after an edit.
struct People {
    now: Vec<String>,
    removed: Vec<String>,
    added: Vec<String>,
    /// Attendees that stay: their lines (with their answers) are kept as they are.
    keep: Vec<String>,
    organizer: String,
}

fn plan_people(old: &Vevent, patch: &Patch, plan: &mut Plan) -> Result<People, String> {
    let before: Vec<String> = old.attendees().into_iter().map(|a| a.email).collect();
    let organizer = old
        .get("ORGANIZER")
        .map(|p| mailto(&p.value))
        .unwrap_or_default();
    let Some(list) = &patch.attendees else {
        return Ok(People {
            now: before,
            removed: Vec::new(),
            added: Vec::new(),
            keep: Vec::new(),
            organizer,
        });
    };
    let mut now: Vec<String> = Vec::new();
    for a in list {
        if !now.contains(a) {
            now.push(a.clone());
        }
    }
    let removed: Vec<String> = before
        .iter()
        .filter(|a| !now.contains(a))
        .cloned()
        .collect();
    let added: Vec<String> = now
        .iter()
        .filter(|a| !before.contains(a))
        .cloned()
        .collect();
    plan.material |= !removed.is_empty() || !added.is_empty();
    plan.drop.push("ATTENDEE");
    plan.add.extend(added.iter().map(|a| attendee_line(a)));
    let mut organizer = organizer;
    if organizer.is_empty() && !now.is_empty() {
        let o = patch.organizer.clone().ok_or(
            "this event has no organizer yet: the account needs an e-mail address to invite people",
        )?;
        plan.add.push(format!("ORGANIZER:mailto:{o}"));
        organizer = o;
    }
    if now.is_empty() {
        plan.drop.push("ORGANIZER");
        organizer.clear();
    }
    let keep = now.iter().filter(|a| before.contains(a)).cloned().collect();
    Ok(People {
        now,
        removed,
        added,
        keep,
        organizer,
    })
}

/// Apply `patch` to the series (or single event) in `ics`. Other components,
/// per-occurrence overrides and unknown properties are kept as they were.
///
/// # Errors
/// A message when the document has no event or the change makes no sense.
pub fn edit(ics: &str, patch: &Patch) -> Result<Edited, String> {
    let lines = unfold(ics);
    let (open, close) = master_block(&lines)?;
    let old = vevents(&lines[open..=close].join("\n"))
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut plan = Plan {
        drop: vec!["SEQUENCE", "DTSTAMP", "LAST-MODIFIED"],
        add: Vec::new(),
        material: false,
    };
    plan.text("SUMMARY", patch.summary.as_ref());
    plan.text("LOCATION", patch.location.as_ref());
    plan.text("DESCRIPTION", patch.description.as_ref());
    plan.times(&old, patch)?;
    if let Some(r) = &patch.rrule {
        plan.material = true;
        plan.drop.push("RRULE");
        if !r.is_empty() {
            plan.add.push(format!("RRULE:{r}"));
        }
    }
    let people = plan_people(&old, patch, &mut plan)?;
    // Any edit moves the `SEQUENCE`, and a calendar server that schedules then tells the
    // attendees whatever changed (a note included): so every edit of an event that has
    // attendees needs the caller's agreement, not only a change of time or place.
    let told = old.attendees().len();
    if told > 0 && patch.attendees.is_none() && !patch.notify {
        return Err(format!(
            "this event has {told} attendee(s) and any change tells them: \
             call again with notify_attendees=true (the user may be asked first)"
        ));
    }
    let seq = old
        .get("SEQUENCE")
        .and_then(|p| p.value.trim().parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    plan.add.push(format!("SEQUENCE:{seq}"));
    plan.add.push(format!("DTSTAMP:{}", now_stamp()));
    let out = rebuild(&lines, (open, close), &plan.drop, &people.keep, plan.add);
    Ok(Edited {
        ics: join(&out),
        attendees: people.now,
        removed: people.removed,
        added: people.added,
        material: plan.material,
        organizer: people.organizer,
        uid: old.uid(),
    })
}

fn as_when(m: Moment) -> When {
    match m {
        Moment::Date(d) => When::Day(d),
        other => When::At(other.to_utc().unwrap_or_default()),
    }
}

/// `ics` as an iTIP message (`REQUEST`, `CANCEL`) for e-mail: a `METHOD`
/// line is added; a cancellation is marked cancelled and addressed to
/// `only` alone.
#[must_use]
pub fn itip(ics: &str, method: &str, only: Option<&[String]>) -> String {
    let mut out = Vec::new();
    let mut placed = false;
    let mut depth = 0usize;
    for line in unfold(ics) {
        let up = line.to_ascii_uppercase();
        if up.starts_with("METHOD:") {
            continue;
        }
        if up.starts_with("BEGIN:") && up != "BEGIN:VCALENDAR" && up != "BEGIN:VEVENT" {
            depth += 1;
        }
        if depth == 0 {
            if let Some(p) = parse_prop(&line) {
                // A cancellation goes to the people who were taken off, who are no longer
                // on the stored event: they are written back as its only attendees.
                if p.name == "ATTENDEE" && only.is_some() {
                    continue;
                }
                if p.name == "STATUS" && method == "CANCEL" {
                    continue;
                }
            }
            if up == "END:VEVENT" {
                out.extend(only.unwrap_or_default().iter().map(|a| attendee_line(a)));
                if method == "CANCEL" {
                    out.push("STATUS:CANCELLED".to_owned());
                }
            }
        }
        if up.starts_with("END:") && up != "END:VCALENDAR" && up != "END:VEVENT" {
            depth = depth.saturating_sub(1);
        }
        out.push(line.clone());
        if !placed && up == "VERSION:2.0" {
            out.push(format!("METHOD:{method}"));
            placed = true;
        }
    }
    join(&out)
}

#[cfg(test)]
mod unit {
    use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};

    use super::{
        Budget, Moment, escape, events_in_range, parse_duration, parse_events, unescape, unfold,
    };

    #[test]
    fn unfold_joins_space_and_tab_continuations() {
        // The leading WSP is a fold marker, not content: it is removed on join.
        assert_eq!(unfold("SUMMARY:Hello\n world"), ["SUMMARY:Helloworld"]);
        assert_eq!(unfold("SUMMARY:Hello\n\tworld"), ["SUMMARY:Helloworld"]);
        assert_eq!(
            unfold("SUMMARY:Hello\n world\n again\n and again"),
            ["SUMMARY:Helloworldagainand again"]
        );
    }

    #[test]
    fn unfold_handles_crlf_and_removes_only_one_leading_whitespace() {
        assert_eq!(
            unfold("BEGIN:VCALENDAR\r\nSUMMARY:Hello\r\n world\r\nEND:VCALENDAR"),
            ["BEGIN:VCALENDAR", "SUMMARY:Helloworld", "END:VCALENDAR"]
        );
        // Only the single continuation marker is stripped; the second space stays.
        assert_eq!(
            unfold("SUMMARY:Hello\n  indented"),
            ["SUMMARY:Hello indented"]
        );
        // A continuation with no preceding line is kept as-is.
        assert_eq!(unfold(" leading"), [" leading"]);
    }

    #[test]
    fn unescape_decodes_known_escapes_and_keeps_unknown_characters() {
        for (input, expected) in [
            (r"Line\nTwo", "Line\nTwo"),
            (r"Line\NTwo", "Line\nTwo"),
            (r"a\,b", "a,b"),
            (r"a\;b", "a;b"),
            (r"a\\b", "a\\b"),
            (r"a\xb", "axb"),
            (r"plain text", "plain text"),
        ] {
            assert_eq!(unescape(input), expected, "input: {input}");
        }
    }

    #[test]
    fn unescape_drops_a_trailing_lone_backslash() {
        assert_eq!(unescape("trailing\\"), "trailing");
        // An escaped backslash before `n` is a backslash then a plain `n`.
        assert_eq!(unescape(r"a\\nb"), "a\\nb");
    }

    #[test]
    fn parse_events_unfolds_and_unescapes_a_description() {
        let ics = concat!(
            "BEGIN:VCALENDAR\n",
            "BEGIN:VEVENT\n",
            "UID:fold-1\n",
            "SUMMARY:Team\\nMeeting\n",
            // A real fold keeps content spaces on the previous line; the WSP
            // starting the continuation is only the fold marker.
            "DESCRIPTION:Line one\\nLine \n",
            " two\\, with comma\\; semicolon and a \\\\ slash\n",
            "DTSTART:20261120T090000Z\n",
            "END:VEVENT\n",
            "END:VCALENDAR\n",
        );
        let events = parse_events(ics);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.uid, "fold-1");
        assert_eq!(event.summary, "Team\nMeeting");
        assert_eq!(
            event.description,
            "Line one\nLine two, with comma; semicolon and a \\ slash"
        );
        assert_eq!(event.start, "2026-11-20T09:00:00Z");
        assert_eq!(event.location, "");
        assert_eq!(event.end, "");
    }

    #[test]
    fn a_hostile_duration_is_refused_not_computed() {
        for bad in [
            "P99999999999999999999D",
            "P9223372036854775807W",
            "PT9223372036854775807H",
            "PT9223372036854775807S",
            "P106751991167301D",
            "-P9223372036854775807D",
            "P1D1D9223372036854775807D",
        ] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("-P1W"), Some(Duration::weeks(-1)));
    }

    #[test]
    fn moving_a_moment_past_the_last_date_is_none_not_a_panic() {
        let tz = chrono_tz::UTC;
        assert!(
            Moment::Date(NaiveDate::MAX)
                .plus(Duration::days(1))
                .is_none()
        );
        assert!(
            Moment::Utc(DateTime::<Utc>::MAX_UTC)
                .plus(Duration::seconds(1))
                .is_none()
        );
        assert!(
            Moment::Local(NaiveDateTime::MAX, Some(tz))
                .plus(Duration::seconds(1))
                .is_none()
        );
        assert!(Moment::Date(NaiveDate::MAX).plus(Duration::MAX).is_none());
        assert!(
            Moment::Utc(DateTime::<Utc>::MIN_UTC)
                .plus(Duration::MIN)
                .is_none()
        );
        // Ordinary moves still work.
        let d = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        assert_eq!(
            Moment::Date(d).plus(Duration::days(2)),
            Some(Moment::Date(NaiveDate::from_ymd_opt(2026, 11, 22).unwrap()))
        );
    }

    #[test]
    fn a_crafted_event_cannot_panic_the_parser_or_the_expansion() {
        let from: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let to: DateTime<Utc> = "2027-01-01T00:00:00Z".parse().unwrap();
        for body in [
            "DTSTART;VALUE=DATE:99991231\nDURATION:P99999999999W",
            "DTSTART:99991231T235959Z\nDURATION:PT9223372036854775807S",
            "DTSTART;VALUE=DATE:99991231",
            "DTSTART:20261120T090000Z\nDURATION:P999999999999999999999D",
            "DTSTART:20261120T090000Z\nDURATION:PT9223372036854775807S\nRRULE:FREQ=DAILY",
            "DTSTART;VALUE=DATE:99991231\nRRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU",
        ] {
            let ics = format!(
                "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\n{body}\nEND:VEVENT\nEND:VCALENDAR\n"
            );
            assert_eq!(parse_events(&ics).len(), 1, "{body}");
            let _ = events_in_range(&ics, from, to, &mut Budget::new(100, 100_000));
        }
    }

    #[test]
    fn a_document_cannot_expand_past_its_event_budget() {
        let events: String = (0..50)
            .map(|i| {
                format!(
                    "BEGIN:VEVENT\nUID:e{i}\nDTSTART:20261120T090000Z\nDTEND:20261120T100000Z\nEND:VEVENT\n"
                )
            })
            .collect::<Vec<_>>()
            .concat();
        let ics = format!("BEGIN:VCALENDAR\n{events}END:VCALENDAR\n");
        let from: DateTime<Utc> = "2026-11-01T00:00:00Z".parse().unwrap();
        let to: DateTime<Utc> = "2026-12-01T00:00:00Z".parse().unwrap();
        let mut budget = Budget::new(20, 1_000);
        let got = events_in_range(&ics, from, to, &mut budget);
        assert_eq!(got.len(), 20);
        assert!(budget.exhausted());
        // One budget is shared by the documents of one answer.
        let more = events_in_range(&ics, from, to, &mut budget);
        assert_eq!(more, Vec::new());
        // A roomy budget is not touched.
        let mut roomy = Budget::new(100, 1_000);
        assert_eq!(events_in_range(&ics, from, to, &mut roomy).len(), 50);
        assert!(!roomy.exhausted());
    }

    #[test]
    fn control_characters_never_reach_a_content_line() {
        assert_eq!(escape("a\u{7}b\rc\u{1b}d\u{0}e"), "abcde");
        assert_eq!(escape("one\ntwo\tthree"), "one\\ntwo\tthree");
        assert_eq!(escape("semi;comma,back\\"), "semi\\;comma\\,back\\\\");
    }
}

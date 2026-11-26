//! Free/busy arithmetic: merge what is booked, hand back what is left.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveTime, TimeZone, Utc, Weekday};
use serde::Serialize;

use crate::ical::Event;

/// Most days of working hours walked in one call.
const MAX_DAYS: i64 = 92;
/// Most slots handed back.
pub const MAX_SLOTS: usize = 200;

/// What to look for.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// Range start.
    pub from: DateTime<Utc>,
    /// Range end.
    pub to: DateTime<Utc>,
    /// Shortest useful slot.
    pub min: Duration,
    /// Daily working hours as local times, if the slots must fall inside them.
    pub hours: Option<(NaiveTime, NaiveTime)>,
    /// Skip Saturdays and Sundays (only with `hours`).
    pub weekdays_only: bool,
    /// The local time zone for `hours`.
    pub offset: FixedOffset,
}

/// A stretch of time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Slot {
    /// Start, RFC 3339 in the requested offset.
    pub start: String,
    /// End, RFC 3339 in the requested offset.
    pub end: String,
    /// Length in minutes.
    pub minutes: i64,
}

type Span = (DateTime<Utc>, DateTime<Utc>);

/// The merged stretches the events block inside `[from, to)`.
#[must_use]
pub fn busy(events: &[Event], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Span> {
    let mut spans: Vec<Span> = events
        .iter()
        .filter(|e| e.busy)
        .filter_map(|e| e.span)
        .filter(|(s, e)| e > s)
        .map(|(s, e)| (s.max(from), e.min(to)))
        .filter(|(s, e)| e > s)
        .collect();
    spans.sort();
    let mut out: Vec<Span> = Vec::new();
    for (s, e) in spans {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

fn gaps(busy: &[Span], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Span> {
    let mut out = Vec::new();
    let mut at = from;
    for &(s, e) in busy {
        if s > at {
            out.push((at, s));
        }
        at = at.max(e);
    }
    if at < to {
        out.push((at, to));
    }
    out
}

/// The local working windows of every day that touches `[from, to)`.
fn working(w: &Window, (open, close): (NaiveTime, NaiveTime)) -> Vec<Span> {
    let first = w.from.with_timezone(&w.offset).date_naive();
    let mut out = Vec::new();
    for i in 0..=MAX_DAYS {
        let Some(day) = first.checked_add_signed(Duration::days(i)) else {
            break;
        };
        if w.weekdays_only && matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            continue;
        }
        let at = |t: NaiveTime| {
            w.offset
                .from_local_datetime(&day.and_time(t))
                .single()
                .map(|d| d.with_timezone(&Utc))
        };
        if let (Some(s), Some(e)) = (at(open), at(close))
            && e > s
            && s < w.to
        {
            out.push((s, e));
        }
        if day.and_time(open).and_utc() > w.to + Duration::days(2) {
            break;
        }
    }
    out
}

fn intersect(a: &[Span], b: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    for &(s1, e1) in a {
        for &(s2, e2) in b {
            let (s, e) = (s1.max(s2), e1.min(e2));
            if e > s {
                out.push((s, e));
            }
        }
    }
    out.sort();
    out
}

/// The free slots of `w` given everything booked, longest first is not promised: earliest first.
#[must_use]
pub fn free_slots(events: &[Event], w: &Window) -> Vec<Slot> {
    let mut free = gaps(&busy(events, w.from, w.to), w.from, w.to);
    if let Some(h) = w.hours {
        free = intersect(&free, &working(w, h));
    }
    let fmt = |t: DateTime<Utc>| {
        t.with_timezone(&w.offset)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    };
    free.into_iter()
        .filter(|(s, e)| *e - *s >= w.min)
        .take(MAX_SLOTS)
        .map(|(s, e)| Slot {
            start: fmt(s),
            end: fmt(e),
            minutes: (e - s).num_minutes(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ev(s: &str, e: &str, busy: bool) -> Event {
        Event {
            uid: "u".into(),
            summary: String::new(),
            start: s.into(),
            end: e.into(),
            location: String::new(),
            description: String::new(),
            recurrence: String::new(),
            organizer: String::new(),
            attendees: Vec::new(),
            busy,
            span: Some((t(s), t(e))),
        }
    }

    #[test]
    fn overlapping_bookings_merge_and_free_events_do_not_block() {
        let events = [
            ev("2026-11-20T09:00:00Z", "2026-11-20T10:00:00Z", true),
            ev("2026-11-20T09:30:00Z", "2026-11-20T11:00:00Z", true),
            ev("2026-11-20T12:00:00Z", "2026-11-20T13:00:00Z", false),
        ];
        let b = busy(
            &events,
            t("2026-11-20T00:00:00Z"),
            t("2026-11-21T00:00:00Z"),
        );
        assert_eq!(b, [(t("2026-11-20T09:00:00Z"), t("2026-11-20T11:00:00Z"))]);
    }

    #[test]
    fn slots_respect_the_minimum_length_and_working_hours() {
        let events = [
            ev("2026-11-20T09:00:00Z", "2026-11-20T10:00:00Z", true),
            ev("2026-11-20T10:30:00Z", "2026-11-20T15:30:00Z", true),
        ];
        let w = Window {
            from: t("2026-11-20T00:00:00Z"),
            to: t("2026-11-21T00:00:00Z"),
            min: Duration::minutes(45),
            hours: Some((
                NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            )),
            weekdays_only: false,
            offset: FixedOffset::east_opt(0).unwrap(),
        };
        let s = free_slots(&events, &w);
        // 10:00-10:30 is too short; 15:30-17:00 fits; nothing outside 09-17.
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(s[0].start, "2026-11-20T15:30:00+00:00");
        assert_eq!(s[0].minutes, 90);
    }

    #[test]
    fn weekends_are_skipped_and_the_offset_shifts_the_day() {
        // 2026-11-21 is a Saturday.
        let w = Window {
            from: t("2026-11-20T00:00:00Z"),
            to: t("2026-11-24T00:00:00Z"),
            min: Duration::minutes(60),
            hours: Some((
                NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            )),
            weekdays_only: true,
            offset: FixedOffset::east_opt(3600).unwrap(),
        };
        let s = free_slots(&[], &w);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[0].start, "2026-11-20T09:00:00+01:00");
        assert_eq!(s[1].start, "2026-11-23T09:00:00+01:00");
    }
}

//! Recurrence rules (RFC 5545 section 3.3.10).
//!
//! [`parse`] checks a rule the agent wrote. [`occurrences`] expands a series
//! whose server did not do it (`<c:expand>` is optional in practice); it covers
//! the rules people actually write: `FREQ` DAILY/WEEKLY/MONTHLY/YEARLY with
//! `INTERVAL`, `COUNT`, `UNTIL`, `BYDAY` (with a position for monthly rules),
//! `BYMONTHDAY`, `BYMONTH` and `WKST`. Anything else is reported as not
//! expandable so the caller can fall back to the series master.

use std::collections::HashSet;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday};

use crate::ical::{Budget, Moment, Vevent, parse_duration};

/// Most occurrences one expansion returns.
const MAX_OCCURRENCES: usize = 2000;
/// Most periods (days, weeks, months, years) one expansion walks.
const MAX_PERIODS: u32 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A checked recurrence rule.
#[derive(Debug, Clone)]
pub struct Rule {
    freq: Freq,
    interval: u32,
    count: Option<u32>,
    until: Option<(NaiveDateTime, bool)>,
    by_day: Vec<(Option<i32>, Weekday)>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
    wkst: Weekday,
    /// `false` when the rule uses parts [`occurrences`] does not expand.
    expandable: bool,
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn numbers(v: &str, key: &str, range: std::ops::RangeInclusive<i32>) -> Result<Vec<i32>, String> {
    v.split(',')
        .map(|n| {
            n.trim()
                .parse::<i32>()
                .ok()
                .filter(|n| *n != 0 && range.contains(n))
                .ok_or_else(|| format!("{key} has a bad value `{n}`"))
        })
        .collect()
}

/// Check a rule such as `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10`.
///
/// # Errors
/// A message naming the problem.
pub fn parse(rule: &str) -> Result<Rule, String> {
    let rule = rule.trim();
    if rule.is_empty() || rule.len() > 400 || rule.contains(['\r', '\n']) {
        return Err("the recurrence rule is empty or too long".into());
    }
    // A rule is plain ASCII; anything else is not one (and `BYDAY` is cut by byte below).
    if !rule.is_ascii() {
        return Err("the recurrence rule may only use plain ASCII characters".into());
    }
    let mut r = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: Vec::new(),
        by_month_day: Vec::new(),
        by_month: Vec::new(),
        wkst: Weekday::Mon,
        expandable: true,
    };
    let mut freq = None;
    for part in rule.split(';').filter(|p| !p.trim().is_empty()) {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("`{part}` is not KEY=VALUE"))?;
        let (k, v) = (k.trim().to_ascii_uppercase(), v.trim().to_ascii_uppercase());
        match k.as_str() {
            "FREQ" => {
                freq = Some(match v.as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return Err("FREQ must be DAILY, WEEKLY, MONTHLY or YEARLY".into()),
                });
            }
            "INTERVAL" => {
                r.interval = v
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or("INTERVAL must be 1 to 1000")?;
            }
            "COUNT" => {
                r.count = Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| (1..=10_000).contains(n))
                        .ok_or("COUNT must be 1 to 10000")?,
                );
            }
            "UNTIL" => r.until = Some(until(&v)?),
            "BYDAY" => {
                for d in v.split(',') {
                    let split = d.len().saturating_sub(2);
                    let (n, day) = d.split_at(split);
                    let day = weekday(day).ok_or_else(|| format!("BYDAY has a bad value `{d}`"))?;
                    let n = if n.is_empty() {
                        None
                    } else {
                        Some(
                            n.parse::<i32>()
                                .ok()
                                .filter(|n| *n != 0 && (-53..=53).contains(n))
                                .ok_or_else(|| format!("BYDAY has a bad value `{d}`"))?,
                        )
                    };
                    r.by_day.push((n, day));
                }
            }
            "BYMONTHDAY" => r.by_month_day = numbers(&v, "BYMONTHDAY", -31..=31)?,
            "BYMONTH" => {
                r.by_month = numbers(&v, "BYMONTH", 1..=12)?
                    .into_iter()
                    .map(i32::unsigned_abs)
                    .collect();
            }
            "WKST" => r.wkst = weekday(&v).ok_or("WKST must be MO to SU")?,
            "BYSETPOS" | "BYWEEKNO" | "BYYEARDAY" | "BYHOUR" | "BYMINUTE" | "BYSECOND" => {
                r.expandable = false;
            }
            other => return Err(format!("`{other}` is not a recurrence rule part")),
        }
    }
    r.freq = freq.ok_or("the recurrence rule needs FREQ")?;
    if r.count.is_some() && r.until.is_some() {
        return Err("COUNT and UNTIL cannot be used together".into());
    }
    // Ordinal weekdays only make sense for monthly (and yearly) rules.
    if matches!(r.freq, Freq::Daily | Freq::Weekly) && r.by_day.iter().any(|(n, _)| n.is_some()) {
        return Err("a weekday with a number (like 2MO) needs FREQ=MONTHLY or YEARLY".into());
    }
    if r.freq == Freq::Yearly && !r.by_day.is_empty() {
        r.expandable = false;
    }
    // A repeated value must not repeat the work (or the occurrence).
    let mut seen = Vec::new();
    r.by_day.retain(|d| {
        let new = !seen.contains(d);
        if new {
            seen.push(*d);
        }
        new
    });
    r.by_month_day.sort_unstable();
    r.by_month_day.dedup();
    r.by_month.sort_unstable();
    r.by_month.dedup();
    never_matches(&r)?;
    Ok(r)
}

/// Longest each month can be (February 29 counts).
const LONGEST_MONTH: [i32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Reject a rule whose `BYMONTH` and `BYMONTHDAY` can never meet (the 31st of
/// February): it would be walked for every period and never produce a date.
fn never_matches(r: &Rule) -> Result<(), String> {
    if r.freq == Freq::Weekly || r.by_month_day.is_empty() {
        return Ok(());
    }
    let possible = |m: u32| {
        let longest = LONGEST_MONTH[usize::try_from(m - 1).unwrap_or(0).min(11)];
        r.by_month_day.iter().any(|d| d.abs() <= longest)
    };
    let any = if r.by_month.is_empty() {
        (1..=12).any(possible)
    } else {
        r.by_month.iter().copied().any(possible)
    };
    if any {
        Ok(())
    } else {
        Err("the recurrence rule never produces a date (for example the 31st of February)".into())
    }
}

/// `UNTIL`: a date, a local time or a UTC time. The flag says it is UTC.
fn until(v: &str) -> Result<(NaiveDateTime, bool), String> {
    let bad = || "UNTIL must look like 20261231 or 20261231T235959Z".to_owned();
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y%m%d") {
        let end = d.and_time(NaiveTime::from_hms_opt(23, 59, 59).ok_or_else(bad)?);
        return Ok((end, false));
    }
    let (body, z) = v.strip_suffix('Z').map_or((v, false), |b| (b, true));
    NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .map(|n| (n, z))
        .map_err(|_| bad())
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y.saturating_add(1), 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1)
        .zip(next)
        .map_or(30, |(a, b)| u32::try_from((b - a).num_days()).unwrap_or(30))
}

/// The days of month `m` that match a `BYDAY` entry.
fn weekdays_of_month(y: i32, m: u32, n: Option<i32>, wd: Weekday) -> Vec<u32> {
    let all: Vec<u32> = (1..=days_in_month(y, m))
        .filter(|d| NaiveDate::from_ymd_opt(y, m, *d).is_some_and(|x| x.weekday() == wd))
        .collect();
    match n {
        None => all,
        Some(n) if n > 0 => all
            .get(usize::try_from(n - 1).unwrap_or(0))
            .copied()
            .into_iter()
            .collect(),
        Some(n) => all
            .len()
            .checked_sub(usize::try_from(-n).unwrap_or(usize::MAX))
            .and_then(|i| all.get(i).copied())
            .into_iter()
            .collect(),
    }
}

fn month_day(y: i32, m: u32, d: i32) -> Option<u32> {
    let dim = i32::try_from(days_in_month(y, m)).ok()?;
    let d = if d < 0 { dim + 1 + d } else { d };
    (1..=dim)
        .contains(&d)
        .then(|| u32::try_from(d).unwrap_or(1))
}

fn add_months(y: i32, m0: u32, add: u32) -> (i32, u32) {
    let total = i64::from(y) * 12 + i64::from(m0) + i64::from(add);
    (
        i32::try_from(total.div_euclid(12)).unwrap_or(y),
        u32::try_from(total.rem_euclid(12)).unwrap_or(0) + 1,
    )
}

fn add_days(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    d.checked_add_signed(Duration::try_days(n)?)
}

impl Rule {
    /// Walk the dates of the series from `start` (local time); `visit` returns `false` to stop.
    /// Every period costs `budget`; the walk ends quietly when it is gone.
    #[allow(clippy::too_many_lines)] // one arm per frequency, each short; splitting hides the shared `emit`
    fn walk(
        &self,
        start: NaiveDateTime,
        budget: &mut Budget,
        mut visit: impl FnMut(NaiveDateTime) -> bool,
    ) {
        // A period looks at every listed value: that is what it costs.
        let per_period =
            1 + (self.by_day.len() + self.by_month_day.len() + self.by_month.len()) as u64;
        let time = start.time();
        let first = start.date();
        let day_ok = |d: NaiveDate| {
            (self.by_month.is_empty() || self.by_month.contains(&d.month()))
                && (self.by_month_day.is_empty()
                    || self
                        .by_month_day
                        .iter()
                        .any(|n| month_day(d.year(), d.month(), *n) == Some(d.day())))
                && (self.by_day.is_empty() || self.by_day.iter().any(|(_, w)| *w == d.weekday()))
        };
        let mut emitted = 0u32;
        let mut emit = |d: NaiveDate| -> bool {
            if d < first {
                return true;
            }
            if self.count.is_some_and(|c| emitted >= c) {
                return false;
            }
            emitted += 1;
            visit(d.and_time(time))
        };
        for k in 0..MAX_PERIODS {
            if !budget.spend(per_period) {
                return;
            }
            let step = i64::from(k) * i64::from(self.interval);
            match self.freq {
                Freq::Daily => {
                    let Some(d) = add_days(first, step) else {
                        return;
                    };
                    if day_ok(d) && !emit(d) {
                        return;
                    }
                }
                Freq::Weekly => {
                    let back = i64::from(first.weekday().days_since(self.wkst));
                    let Some(week) = add_days(first, step * 7 - back) else {
                        return;
                    };
                    let mut days: Vec<NaiveDate> = if self.by_day.is_empty() {
                        add_days(week, back).into_iter().collect()
                    } else {
                        self.by_day
                            .iter()
                            .filter_map(|(_, w)| add_days(week, i64::from(w.days_since(self.wkst))))
                            .collect()
                    };
                    days.sort();
                    days.dedup();
                    for d in days {
                        if (self.by_month.is_empty() || self.by_month.contains(&d.month()))
                            && !emit(d)
                        {
                            return;
                        }
                    }
                }
                Freq::Monthly => {
                    let (y, m) = add_months(first.year(), first.month0(), k * self.interval);
                    if NaiveDate::from_ymd_opt(y, m, 1).is_none() {
                        return; // past the last year a date can name
                    }
                    let mut days: Vec<u32> = if !self.by_day.is_empty() {
                        self.by_day
                            .iter()
                            .flat_map(|(n, w)| weekdays_of_month(y, m, *n, *w))
                            .filter(|d| {
                                self.by_month_day.is_empty()
                                    || self
                                        .by_month_day
                                        .iter()
                                        .any(|n| month_day(y, m, *n) == Some(*d))
                            })
                            .collect()
                    } else if self.by_month_day.is_empty() {
                        vec![first.day()]
                    } else {
                        self.by_month_day
                            .iter()
                            .filter_map(|n| month_day(y, m, *n))
                            .collect()
                    };
                    days.sort_unstable();
                    days.dedup();
                    for d in days {
                        if let Some(date) = NaiveDate::from_ymd_opt(y, m, d)
                            && (self.by_month.is_empty() || self.by_month.contains(&m))
                            && !emit(date)
                        {
                            return;
                        }
                    }
                }
                Freq::Yearly => {
                    let y = first
                        .year()
                        .saturating_add(i32::try_from(step).unwrap_or(i32::MAX));
                    if y > NaiveDate::MAX.year() {
                        return;
                    }
                    let months = if self.by_month.is_empty() {
                        vec![first.month()]
                    } else {
                        let mut m = self.by_month.clone();
                        m.sort_unstable();
                        m
                    };
                    for m in months {
                        let mut days: Vec<u32> = if self.by_month_day.is_empty() {
                            vec![first.day()]
                        } else {
                            self.by_month_day
                                .iter()
                                .filter_map(|n| month_day(y, m, *n))
                                .collect()
                        };
                        days.sort_unstable();
                        days.dedup();
                        for d in days {
                            if let Some(date) = NaiveDate::from_ymd_opt(y, m, d)
                                && !emit(date)
                            {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The occurrences of `master` that overlap `[from, to)`, as `(start, end)`.
/// `skip` holds instants that were moved or cancelled by an override.
/// `None` when the rule cannot be expanded here.
pub(crate) fn occurrences(
    master: &Vevent,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    skip: &[DateTime<Utc>],
    budget: &mut Budget,
) -> Option<Vec<(Moment, Moment)>> {
    let rule = parse(&master.get("RRULE")?.value)
        .ok()
        .filter(|r| r.expandable)?;
    let start = master.start()?;
    let length = match (master.get("DTEND"), master.end()) {
        (_, Some(end)) if end.is_date() == start.is_date() => end.to_utc()? - start.to_utc()?,
        _ => master
            .get("DURATION")
            .and_then(|p| parse_duration(&p.value))
            .unwrap_or_else(Duration::zero),
    };
    let excluded: HashSet<DateTime<Utc>> = master
        .all("EXDATE")
        .flat_map(|p| {
            p.value.split(',').filter_map(move |v| {
                let one = crate::ical::Prop {
                    name: p.name.clone(),
                    params: p.params.clone(),
                    value: v.to_owned(),
                };
                Moment::from_prop(&one)?.to_utc()
            })
        })
        .chain(skip.iter().copied())
        .collect();
    let (naive, tz) = match start {
        Moment::Date(d) => (d.and_time(NaiveTime::MIN), None),
        Moment::Utc(t) => (t.naive_utc(), None),
        Moment::Local(n, tz) => (n, tz),
    };
    let make = |n: NaiveDateTime| match start {
        Moment::Date(_) => Moment::Date(n.date()),
        Moment::Utc(_) => Moment::Utc(n.and_utc()),
        Moment::Local(_, _) => Moment::Local(n, tz),
    };
    let mut out = Vec::new();
    rule.walk(naive, budget, |n| {
        let s = make(n);
        let Some(s_utc) = s.to_utc() else {
            return true;
        };
        let past_until = rule
            .until
            .is_some_and(|(u, utc)| if utc { s_utc.naive_utc() > u } else { n > u });
        if past_until || s_utc >= to || out.len() >= MAX_OCCURRENCES {
            return false;
        }
        let Some(e) = s.plus(length) else {
            return true;
        };
        let e_utc = e.to_utc().unwrap_or(s_utc);
        let overlaps = e_utc > from || (length.is_zero() && s_utc >= from);
        if overlaps && !excluded.contains(&s_utc) {
            out.push((s, e));
        }
        true
    });
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::vevents;

    fn series(extra: &str) -> Vevent {
        let ics = format!(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:s\nDTSTART:20261102T090000Z\nDTEND:20261102T100000Z\n{extra}\nEND:VEVENT\nEND:VCALENDAR\n"
        );
        vevents(&ics).remove(0)
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn starts(v: &Vevent, from: &str, to: &str) -> Vec<String> {
        occurrences(
            v,
            utc(from),
            utc(to),
            &[],
            &mut Budget::new(100_000, 10_000_000),
        )
        .unwrap()
        .into_iter()
        .map(|(s, _)| s.display())
        .collect()
    }

    #[test]
    fn weekly_rule_with_days_and_count() {
        // 2026-11-02 is a Monday.
        let v = series("RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=5");
        assert_eq!(
            starts(&v, "2026-11-01T00:00:00Z", "2026-12-31T00:00:00Z"),
            [
                "2026-11-02T09:00:00Z",
                "2026-11-04T09:00:00Z",
                "2026-11-09T09:00:00Z",
                "2026-11-11T09:00:00Z",
                "2026-11-16T09:00:00Z"
            ]
        );
    }

    #[test]
    fn interval_exdate_and_range_clip() {
        let v = series("RRULE:FREQ=DAILY;INTERVAL=2\nEXDATE:20261106T090000Z");
        assert_eq!(
            starts(&v, "2026-11-03T00:00:00Z", "2026-11-11T00:00:00Z"),
            [
                "2026-11-04T09:00:00Z",
                "2026-11-08T09:00:00Z",
                "2026-11-10T09:00:00Z"
            ]
        );
    }

    #[test]
    fn monthly_by_position_and_negative_day() {
        let v = series("RRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=3");
        assert_eq!(
            starts(&v, "2026-11-01T00:00:00Z", "2027-03-01T00:00:00Z"),
            [
                "2026-11-10T09:00:00Z",
                "2026-12-08T09:00:00Z",
                "2027-01-12T09:00:00Z"
            ]
        );
        let v = series("RRULE:FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=2");
        assert_eq!(
            starts(&v, "2026-11-01T00:00:00Z", "2027-03-01T00:00:00Z"),
            ["2026-11-30T09:00:00Z", "2026-12-31T09:00:00Z"]
        );
    }

    #[test]
    fn until_ends_the_series_and_yearly_skips_missing_days() {
        let v = series("RRULE:FREQ=WEEKLY;UNTIL=20261116T090000Z");
        assert_eq!(
            starts(&v, "2026-11-01T00:00:00Z", "2027-01-01T00:00:00Z"),
            [
                "2026-11-02T09:00:00Z",
                "2026-11-09T09:00:00Z",
                "2026-11-16T09:00:00Z"
            ]
        );
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:l\nDTSTART;VALUE=DATE:20240229\nRRULE:FREQ=YEARLY;COUNT=3\nEND:VEVENT\nEND:VCALENDAR\n";
        let v = vevents(ics).remove(0);
        assert_eq!(
            starts(&v, "2024-01-01T00:00:00Z", "2040-01-01T00:00:00Z"),
            ["2024-02-29", "2028-02-29", "2032-02-29"]
        );
    }

    #[test]
    fn a_zoned_series_keeps_its_wall_clock_time_across_daylight_saving() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:z\nDTSTART;TZID=Europe/Berlin:20261026T090000\nDTEND;TZID=Europe/Berlin:20261026T100000\nRRULE:FREQ=WEEKLY;COUNT=2\nEND:VEVENT\nEND:VCALENDAR\n";
        let v = vevents(ics).remove(0);
        // The clocks go back on 2026-10-25, so the first is already +01:00; use a
        // series that crosses it instead.
        let ics2 = ics.replace("20261026", "20261019");
        let v2 = vevents(&ics2).remove(0);
        assert_eq!(
            starts(&v2, "2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z"),
            ["2026-10-19T07:00:00Z", "2026-10-26T08:00:00Z"]
        );
        assert_eq!(
            starts(&v, "2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z")[0],
            "2026-10-26T08:00:00Z"
        );
    }

    #[test]
    fn rules_are_checked() {
        for ok in [
            "FREQ=DAILY",
            "FREQ=WEEKLY;BYDAY=MO,TU;COUNT=4",
            "FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20271231",
        ] {
            assert!(parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "BYDAY=MO",
            "FREQ=HOURLY",
            "FREQ=DAILY;COUNT=2;UNTIL=20261231",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=WEEKLY;BYDAY=2MO",
            "FREQ=DAILY;FOO=1",
            "FREQ=DAILY\nX:1",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        // Valid but not expanded here: the master is shown instead.
        assert!(
            !parse("FREQ=MONTHLY;BYDAY=MO;BYSETPOS=1")
                .unwrap()
                .expandable
        );
    }

    #[test]
    fn a_rule_is_plain_ascii_and_a_weekday_is_never_cut_inside_a_character() {
        for bad in [
            "FREQ=WEEKLY;BYDAY=\u{e9}a",
            "FREQ=MONTHLY;BYDAY=1\u{e9}a",
            "FREQ=MONTHLY;BYDAY=\u{e9}\u{e9}",
            "FREQ=WEEKLY;BYDAY=M\u{d6}",
            "FREQ=DAILY;BYMONTH=\u{ff11}",
            "FREQ=WEEKLY;BYDAY=\u{1F600}",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn repeated_values_do_not_repeat_the_work_or_the_occurrence() {
        let r = parse("FREQ=MONTHLY;BYMONTH=3,3,3,11;BYMONTHDAY=5,5,-1,-1;BYDAY=MO,MO").unwrap();
        assert_eq!(r.by_month, [3, 11]);
        assert_eq!(r.by_month_day, [-1, 5]);
        assert_eq!(r.by_day.len(), 1);
        // Each date comes out once, not once per repeat.
        let v = series("RRULE:FREQ=YEARLY;BYMONTH=11,11;BYMONTHDAY=2,2,2;COUNT=3");
        assert_eq!(
            starts(&v, "2026-01-01T00:00:00Z", "2031-01-01T00:00:00Z"),
            [
                "2026-11-02T09:00:00Z",
                "2027-11-02T09:00:00Z",
                "2028-11-02T09:00:00Z"
            ]
        );
        // Two hundred copies of one weekday are one weekday.
        let many = format!("FREQ=WEEKLY;BYDAY={}", vec!["MO"; 90].join(","));
        assert_eq!(parse(&many).unwrap().by_day.len(), 1);
    }

    #[test]
    fn a_rule_that_can_never_produce_a_date_is_refused() {
        for bad in [
            "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=31",
            "FREQ=DAILY;BYMONTH=4,6;BYMONTHDAY=31",
            "FREQ=MONTHLY;BYMONTH=2;BYMONTHDAY=30,-31",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        for ok in [
            "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29",
            "FREQ=MONTHLY;BYMONTHDAY=31",
            "FREQ=YEARLY;BYMONTH=2,3;BYMONTHDAY=31",
            "FREQ=MONTHLY;BYMONTHDAY=-1",
        ] {
            assert!(parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_work_budget_ends_a_walk_that_keeps_finding_nothing() {
        // The 29th of February: nothing for years at a time, so the walk would run for ever.
        let v = series("RRULE:FREQ=DAILY;BYMONTH=2;BYMONTHDAY=29");
        let mut budget = Budget::new(10, 2_000);
        let found = occurrences(
            &v,
            utc("2026-01-01T00:00:00Z"),
            utc("2090-01-01T00:00:00Z"),
            &[],
            &mut budget,
        )
        .unwrap();
        assert!(budget.exhausted());
        assert!(found.len() <= 1, "{}", found.len());
        // With room it finds them all.
        let mut roomy = Budget::new(1_000, 20_000_000);
        let found = occurrences(
            &v,
            utc("2026-01-01T00:00:00Z"),
            utc("2040-01-01T00:00:00Z"),
            &[],
            &mut roomy,
        )
        .unwrap();
        assert!(!roomy.exhausted());
        assert_eq!(found.len(), 3); // 2028, 2032, 2036
    }

    #[test]
    fn dates_past_the_last_year_end_the_walk_instead_of_panicking() {
        for rule in [
            "FREQ=WEEKLY;INTERVAL=1000;BYDAY=MO,TU",
            "FREQ=WEEKLY;INTERVAL=1000",
            "FREQ=DAILY;INTERVAL=1000",
            "FREQ=MONTHLY;INTERVAL=1000",
            "FREQ=YEARLY;INTERVAL=1000",
            "FREQ=YEARLY;INTERVAL=1000;BYMONTH=2,3;BYMONTHDAY=1,-1",
        ] {
            let v = series(&format!("RRULE:{rule}"));
            // A window in the far future keeps the walk going through every period.
            let _ = occurrences(
                &v,
                DateTime::<Utc>::MAX_UTC - Duration::days(1),
                DateTime::<Utc>::MAX_UTC,
                &[],
                &mut Budget::new(100, 100_000_000),
            );
        }
        // A series that starts on the last day of the calendar.
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:s\nDTSTART:99991231T090000Z\nDTEND:99991231T100000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO,SU\nEND:VEVENT\nEND:VCALENDAR\n";
        let v = vevents(ics).remove(0);
        let got = occurrences(
            &v,
            utc("9999-12-01T00:00:00Z"),
            utc("9999-12-31T23:59:59Z"),
            &[],
            &mut Budget::new(100, 10_000_000),
        );
        assert!(got.is_some());
    }
}

//! Deterministic reading of "until Friday", "this week", "for 3 days" ...
//!
//! A time-limited fact ("I am in Berlin until Friday") should stop being
//! recalled when the time has passed.  The model only *copies* the phrase (or
//! proposes an ISO date); the date arithmetic is done here, in code, so a wrong
//! calendar guess by the model cannot silently expire or immortalise a fact.
//! Everything is UTC and an expiry is the END of the named day.  Phrases are
//! understood in English, German and Turkish; anything else yields `None`
//! (the memory then simply does not expire).

const DAY_MS: i64 = 86_400_000;
/// Longest expiry accepted: two years.
const MAX_AHEAD_MS: i64 = 730 * DAY_MS;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = Monday ... 6 = Sunday for a day number since 1970-01-01 (a Thursday).
fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

fn end_of_day(days: i64) -> i64 {
    (days + 1) * DAY_MS - 1
}

const WEEKDAYS: [(&[&str], i64); 7] = [
    (&["monday", "montag", "pazartesi"], 0),
    (&["tuesday", "dienstag", "sal\u{131}", "sali"], 1),
    (
        &["wednesday", "mittwoch", "\u{e7}ar\u{15f}amba", "carsamba"],
        2,
    ),
    (&["thursday", "donnerstag", "per\u{15f}embe", "persembe"], 3),
    (&["friday", "freitag", "cuma"], 4),
    (&["saturday", "samstag", "cumartesi"], 5),
    (&["sunday", "sonntag", "pazar"], 6),
];

fn weekday_of(token: &str) -> Option<i64> {
    // Turkish attaches endings ("cumaya kadar"): match the name as a prefix,
    // longest first so "cumartesi" is not read as "cuma".
    let mut best: Option<(usize, i64)> = None;
    for (names, idx) in WEEKDAYS {
        for n in names {
            if token == *n || (token.starts_with(n) && token.len() <= n.len() + 3) {
                let better = best.is_none_or(|(l, _)| n.len() > l);
                if better {
                    best = Some((n.len(), idx));
                }
            }
        }
    }
    best.map(|(_, i)| i)
}

fn parse_iso(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    for start in 0..b.len().saturating_sub(9) {
        let w = &text.get(start..start + 10)?;
        let ok = w.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
        if !ok {
            continue;
        }
        let y: i64 = w[0..4].parse().ok()?;
        let m: i64 = w[5..7].parse().ok()?;
        let d: i64 = w[8..10].parse().ok()?;
        if (1970..=2200).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) {
            return Some(end_of_day(days_from_civil(y, m, d)));
        }
    }
    None
}

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\u{131}')
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

fn has(toks: &[String], word: &str) -> bool {
    toks.iter().any(|t| t == word)
}

fn has_seq(toks: &[String], seq: &[&str]) -> bool {
    toks.windows(seq.len())
        .any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
}

/// The end of the period named in `text`, as Unix ms, or `None` when no known
/// phrase is present or the result is not in the future (within two years).
#[must_use]
pub fn parse_expiry(text: &str, now_ms: i64) -> Option<i64> {
    let today = now_ms.div_euclid(DAY_MS);
    let toks = tokens(text);
    let end_of_week = |days: i64| days + (6 - weekday(days));

    let found = parse_iso(text).or_else(|| {
        if has_seq(&toks, &["this", "weekend"])
            || has_seq(&toks, &["diesem", "wochenende"])
            || has_seq(&toks, &["bu", "hafta", "sonu"])
            || has_seq(&toks, &["this", "week"])
            || has_seq(&toks, &["diese", "woche"])
            || has_seq(&toks, &["bu", "hafta"])
        {
            return Some(end_of_day(end_of_week(today)));
        }
        if has_seq(&toks, &["next", "week"])
            || has_seq(&toks, &["n\u{e4}chste", "woche"])
            || has_seq(&toks, &["gelecek", "hafta"])
        {
            return Some(end_of_day(end_of_week(today) + 7));
        }
        if has(&toks, "tomorrow") || has(&toks, "morgen") || has(&toks, "yar\u{131}n") {
            return Some(end_of_day(today + 1));
        }
        if has(&toks, "today")
            || has(&toks, "tonight")
            || has(&toks, "heute")
            || has(&toks, "bug\u{fc}n")
        {
            return Some(end_of_day(today));
        }
        // "for 3 days / weeks": counted from now.
        for (i, t) in toks.iter().enumerate() {
            let Ok(n) = t.parse::<i64>() else { continue };
            let unit = toks.get(i + 1).map(String::as_str).unwrap_or_default();
            let per = match unit {
                "day" | "days" | "tag" | "tage" | "tagen" | "g\u{fc}n" => 1,
                "week" | "weeks" | "woche" | "wochen" | "hafta" => 7,
                _ => continue,
            };
            if (1..=365).contains(&n) {
                return Some(end_of_day(today + n * per - 1));
            }
        }
        // "until Friday": the next such weekday (today counts).
        toks.iter().find_map(|t| weekday_of(t)).map(|wd| {
            let ahead = (wd - weekday(today)).rem_euclid(7);
            end_of_day(today + ahead)
        })
    });
    found.filter(|&ms| ms > now_ms && ms <= now_ms + MAX_AHEAD_MS)
}

/// Like [`parse_expiry`] for the user's own sentence: only when it carries an
/// explicit limiting word ("until", "for 3 days", "this week"), so a plain date
/// in a fact ("my birthday is 2026-12-01") never expires it.
#[must_use]
pub(crate) fn parse_expiry_in_sentence(text: &str, now_ms: i64) -> Option<i64> {
    const TRIGGERS: [&str; 9] = [
        "until", "till", "through", "for", "this", "bis", "f\u{fc}r", "diese", "kadar",
    ];
    let toks = tokens(text);
    TRIGGERS
        .iter()
        .any(|t| has(&toks, t))
        .then(|| parse_expiry(text, now_ms))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-24 is a Tuesday.
    fn now() -> i64 {
        days_from_civil(2026, 11, 24) * DAY_MS + 10 * 3_600_000
    }

    fn date(ms: i64) -> (i64, i64) {
        // days since epoch of the day containing ms, plus weekday
        let d = ms.div_euclid(DAY_MS);
        (d - days_from_civil(2026, 11, 24), weekday(d))
    }

    #[test]
    fn weekdays_and_periods() {
        assert_eq!(weekday(days_from_civil(2026, 11, 24)), 1);
        // until Friday = +3 days, end of that day
        let f = parse_expiry("I am in Berlin until Friday", now()).unwrap();
        assert_eq!(date(f), (3, 4));
        assert_eq!(f % DAY_MS, DAY_MS - 1);
        // this week = Sunday
        assert_eq!(date(parse_expiry("busy this week", now()).unwrap()), (5, 6));
        assert_eq!(date(parse_expiry("diese Woche", now()).unwrap()), (5, 6));
        assert_eq!(date(parse_expiry("bu hafta", now()).unwrap()), (5, 6));
        assert_eq!(date(parse_expiry("next week", now()).unwrap()), (12, 6));
        assert_eq!(date(parse_expiry("tomorrow", now()).unwrap()), (1, 2));
        assert_eq!(date(parse_expiry("cumaya kadar", now()).unwrap()), (3, 4));
        // "cumartesi" is Saturday, not Friday ("cuma").
        assert_eq!(date(parse_expiry("cumartesi", now()).unwrap()), (4, 5));
        assert_eq!(
            date(parse_expiry("until 2026-12-01", now()).unwrap()),
            (7, 1)
        );
    }

    #[test]
    fn counted_and_rejected() {
        assert_eq!(date(parse_expiry("for 3 days", now()).unwrap()), (2, 3));
        assert_eq!(date(parse_expiry("for 2 weeks", now()).unwrap()).0, 13);
        // Unknown phrase, past date, or absurdly far: no expiry.
        assert!(parse_expiry("for the trip", now()).is_none());
        assert!(parse_expiry("until 2020-01-01", now()).is_none());
        assert!(parse_expiry("until 2099-01-01", now()).is_none());
        assert!(parse_expiry("", now()).is_none());
        // A plain date in a sentence is not a limit; "until <date>" is.
        assert!(parse_expiry_in_sentence("my birthday is 2026-12-01", now()).is_none());
        assert!(parse_expiry_in_sentence("I am away until 2026-12-01", now()).is_some());
    }
}

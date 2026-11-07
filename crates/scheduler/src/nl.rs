//! Pure, deterministic table-driven natural-language schedule parser for
//! English and Turkish phrases (no LLM).
//!
//! Converts common phrases such as `"her gün 09:00"`, `"every monday at 8"`,
//! `"2 saat sonra"`, and `"in 30 minutes"` into a [`ScheduleSpec`].

use crate::model::ScheduleSpec;

#[derive(Clone, Copy)]
enum ExactSpec {
    EverySecs(u64),
    AtOffsetSecs(i64),
    Cron(&'static str),
}

/// Table of fixed (parameter-free) phrases in English and ASCII-folded Turkish.
const EXACT_PHRASE_TABLE: &[(&str, ExactSpec)] = &[
    // Intervals
    ("every second", ExactSpec::EverySecs(1)),
    ("her saniye", ExactSpec::EverySecs(1)),
    ("saniyede bir", ExactSpec::EverySecs(1)),
    ("every minute", ExactSpec::EverySecs(60)),
    ("her dakika", ExactSpec::EverySecs(60)),
    ("dakikada bir", ExactSpec::EverySecs(60)),
    ("hourly", ExactSpec::EverySecs(3_600)),
    ("every hour", ExactSpec::EverySecs(3_600)),
    ("her saat", ExactSpec::EverySecs(3_600)),
    ("saatte bir", ExactSpec::EverySecs(3_600)),
    ("saat basi", ExactSpec::EverySecs(3_600)),
    ("her saat basi", ExactSpec::EverySecs(3_600)),
    // Relative one-offs
    ("in a minute", ExactSpec::AtOffsetSecs(60)),
    ("bir dakika sonra", ExactSpec::AtOffsetSecs(60)),
    ("in half an hour", ExactSpec::AtOffsetSecs(1_800)),
    ("yarim saat sonra", ExactSpec::AtOffsetSecs(1_800)),
    ("in an hour", ExactSpec::AtOffsetSecs(3_600)),
    ("bir saat sonra", ExactSpec::AtOffsetSecs(3_600)),
    ("tomorrow", ExactSpec::AtOffsetSecs(86_400)),
    ("yarin", ExactSpec::AtOffsetSecs(86_400)),
    // Recurring calendar shortcuts
    ("daily", ExactSpec::Cron("0 9 * * *")),
    ("every day", ExactSpec::Cron("0 9 * * *")),
    ("everyday", ExactSpec::Cron("0 9 * * *")),
    ("her gun", ExactSpec::Cron("0 9 * * *")),
    ("gunluk", ExactSpec::Cron("0 9 * * *")),
    ("every morning", ExactSpec::Cron("0 9 * * *")),
    ("her sabah", ExactSpec::Cron("0 9 * * *")),
    ("every evening", ExactSpec::Cron("0 18 * * *")),
    ("her aksam", ExactSpec::Cron("0 18 * * *")),
    ("every night", ExactSpec::Cron("0 22 * * *")),
    ("her gece", ExactSpec::Cron("0 22 * * *")),
    ("midnight", ExactSpec::Cron("0 0 * * *")),
    ("at midnight", ExactSpec::Cron("0 0 * * *")),
    ("every midnight", ExactSpec::Cron("0 0 * * *")),
    ("gece yarisi", ExactSpec::Cron("0 0 * * *")),
    ("her gece yarisi", ExactSpec::Cron("0 0 * * *")),
    ("noon", ExactSpec::Cron("0 12 * * *")),
    ("at noon", ExactSpec::Cron("0 12 * * *")),
    ("every noon", ExactSpec::Cron("0 12 * * *")),
    ("oglen", ExactSpec::Cron("0 12 * * *")),
    ("her oglen", ExactSpec::Cron("0 12 * * *")),
    ("every weekday", ExactSpec::Cron("0 9 * * 1-5")),
    ("weekdays", ExactSpec::Cron("0 9 * * 1-5")),
    ("on weekdays", ExactSpec::Cron("0 9 * * 1-5")),
    ("hafta ici", ExactSpec::Cron("0 9 * * 1-5")),
    ("hafta ici her gun", ExactSpec::Cron("0 9 * * 1-5")),
    ("her hafta ici", ExactSpec::Cron("0 9 * * 1-5")),
    ("is gunleri", ExactSpec::Cron("0 9 * * 1-5")),
    ("every weekend", ExactSpec::Cron("0 9 * * 0,6")),
    ("weekends", ExactSpec::Cron("0 9 * * 0,6")),
    ("on weekends", ExactSpec::Cron("0 9 * * 0,6")),
    ("hafta sonu", ExactSpec::Cron("0 9 * * 0,6")),
    ("hafta sonu her gun", ExactSpec::Cron("0 9 * * 0,6")),
    ("her hafta sonu", ExactSpec::Cron("0 9 * * 0,6")),
    ("hafta sonlari", ExactSpec::Cron("0 9 * * 0,6")),
    ("weekly", ExactSpec::Cron("0 9 * * 1")),
    ("every week", ExactSpec::Cron("0 9 * * 1")),
    ("her hafta", ExactSpec::Cron("0 9 * * 1")),
    ("haftalik", ExactSpec::Cron("0 9 * * 1")),
    ("monthly", ExactSpec::Cron("0 9 1 * *")),
    ("every month", ExactSpec::Cron("0 9 1 * *")),
    ("her ay", ExactSpec::Cron("0 9 1 * *")),
    ("ayda bir", ExactSpec::Cron("0 9 1 * *")),
    ("aylik", ExactSpec::Cron("0 9 1 * *")),
];

/// Table mapping English and Turkish time unit tokens to seconds.
const UNIT_SECONDS_TABLE: &[(&str, u64)] = &[
    ("second", 1),
    ("seconds", 1),
    ("sec", 1),
    ("secs", 1),
    ("s", 1),
    ("saniye", 1),
    ("saniyede", 1),
    ("sn", 1),
    ("minute", 60),
    ("minutes", 60),
    ("min", 60),
    ("mins", 60),
    ("m", 60),
    ("dakika", 60),
    ("dakikada", 60),
    ("dk", 60),
    ("hour", 3_600),
    ("hours", 3_600),
    ("hr", 3_600),
    ("hrs", 3_600),
    ("h", 3_600),
    ("saat", 3_600),
    ("saatte", 3_600),
    ("sa", 3_600),
    ("day", 86_400),
    ("days", 86_400),
    ("d", 86_400),
    ("gun", 86_400),
    ("gunde", 86_400),
    ("week", 604_800),
    ("weeks", 604_800),
    ("w", 604_800),
    ("hafta", 604_800),
    ("haftada", 604_800),
];

/// Table of English and Turkish number words.
const NUMBER_WORD_TABLE: &[(&str, u64)] = &[
    ("a", 1),
    ("an", 1),
    ("one", 1),
    ("bir", 1),
    ("two", 2),
    ("iki", 2),
    ("three", 3),
    ("uc", 3),
    ("four", 4),
    ("dort", 4),
    ("five", 5),
    ("bes", 5),
    ("six", 6),
    ("alti", 6),
    ("seven", 7),
    ("yedi", 7),
    ("eight", 8),
    ("sekiz", 8),
    ("nine", 9),
    ("dokuz", 9),
    ("ten", 10),
    ("on", 10),
    ("fifteen", 15),
    ("twenty", 20),
    ("yirmi", 20),
    ("thirty", 30),
    ("otuz", 30),
    ("forty", 40),
    ("kirk", 40),
    ("forty-five", 45),
    ("fifty", 50),
    ("elli", 50),
    ("sixty", 60),
    ("altmis", 60),
];

/// Table mapping cadence prefixes (after stripping the trailing time) to the
/// last 3 cron fields (`<dom> <month> <dow>`).
const CADENCE_CRON_TABLE: &[(&str, &str)] = &[
    ("every day", "* * *"),
    ("everyday", "* * *"),
    ("daily", "* * *"),
    ("her gun", "* * *"),
    ("gunluk", "* * *"),
    ("her sabah", "* * *"),
    ("every morning", "* * *"),
    ("her aksam", "* * *"),
    ("every evening", "* * *"),
    ("every weekday", "* * 1-5"),
    ("every weekdays", "* * 1-5"),
    ("weekdays", "* * 1-5"),
    ("on weekdays", "* * 1-5"),
    ("hafta ici", "* * 1-5"),
    ("hafta ici her gun", "* * 1-5"),
    ("her hafta ici", "* * 1-5"),
    ("is gunleri", "* * 1-5"),
    ("every weekend", "* * 0,6"),
    ("every weekends", "* * 0,6"),
    ("weekends", "* * 0,6"),
    ("on weekends", "* * 0,6"),
    ("hafta sonu", "* * 0,6"),
    ("hafta sonu her gun", "* * 0,6"),
    ("her hafta sonu", "* * 0,6"),
    ("hafta sonlari", "* * 0,6"),
    ("every month", "1 * *"),
    ("monthly", "1 * *"),
    ("her ay", "1 * *"),
    ("ayda bir", "1 * *"),
    ("aylik", "1 * *"),
    ("her ayin 1", "1 * *"),
    ("her ayin 1i", "1 * *"),
    ("her ayin biri", "1 * *"),
];

/// Table mapping English and Turkish day-of-week names to standard 5-field
/// POSIX cron day numbers (`0` = Sunday, `1` = Monday, ..., `6` = Saturday).
const WEEKDAY_TABLE: &[(&str, &str)] = &[
    ("sunday", "0"),
    ("sundays", "0"),
    ("sun", "0"),
    ("pazar", "0"),
    ("paz", "0"),
    ("monday", "1"),
    ("mondays", "1"),
    ("mon", "1"),
    ("pazartesi", "1"),
    ("pzt", "1"),
    ("tuesday", "2"),
    ("tuesdays", "2"),
    ("tue", "2"),
    ("tues", "2"),
    ("sali", "2"),
    ("sal", "2"),
    ("wednesday", "3"),
    ("wednesdays", "3"),
    ("wed", "3"),
    ("carsamba", "3"),
    ("car", "3"),
    ("thursday", "4"),
    ("thursdays", "4"),
    ("thu", "4"),
    ("thur", "4"),
    ("thurs", "4"),
    ("persembe", "4"),
    ("per", "4"),
    ("friday", "5"),
    ("fridays", "5"),
    ("fri", "5"),
    ("cuma", "5"),
    ("cum", "5"),
    ("saturday", "6"),
    ("saturdays", "6"),
    ("sat", "6"),
    ("cumartesi", "6"),
    ("cmt", "6"),
];

/// Parse a natural-language schedule phrase (English or Turkish) into a
/// [`ScheduleSpec`], using `now_ms` (Unix epoch milliseconds) as the anchor for
/// relative one-off phrases (`"in 30 minutes"`, `"2 saat sonra"`).
///
/// Returns `None` if the phrase does not match any supported pattern.
#[must_use]
pub fn parse_natural_language(phrase: &str, now_ms: i64) -> Option<ScheduleSpec> {
    parse_natural_language_with_tz(phrase, now_ms, None)
}

/// Parse a natural-language schedule phrase with an optional IANA timezone
/// attached to resulting [`ScheduleSpec::Cron`] expressions.
#[must_use]
pub fn parse_natural_language_with_tz(
    phrase: &str,
    now_ms: i64,
    tz: Option<&str>,
) -> Option<ScheduleSpec> {
    let norm = normalize_phrase(phrase);
    if norm.is_empty() {
        return None;
    }

    // 1. Exact phrase table lookup.
    for &(pat, ref spec) in EXACT_PHRASE_TABLE {
        if norm == pat {
            return Some(match *spec {
                ExactSpec::EverySecs(seconds) => ScheduleSpec::Every { seconds },
                ExactSpec::AtOffsetSecs(secs) => ScheduleSpec::At {
                    unix_ms: now_ms.saturating_add(secs.saturating_mul(1_000)),
                },
                ExactSpec::Cron(expr) => make_cron(expr, tz),
            });
        }
    }

    // 2. Relative one-off phrases ("in 30 minutes", "after 2 hours", "2 saat sonra").
    if let Some(spec) = parse_relative_at(&norm, now_ms) {
        return Some(spec);
    }

    // 3. Fixed-interval phrases ("every 15 minutes", "her 10 dakikada bir").
    if let Some(spec) = parse_interval_every(&norm) {
        return Some(spec);
    }

    // 4. Calendar / cron phrases with time ("her gün 09:00", "every monday at 8").
    if let Some(expr) = parse_calendar_cron(&norm) {
        return Some(make_cron(&expr, tz));
    }

    // 5. Bare recurring weekday without explicit time ("every monday", "her pazartesi" -> 09:00).
    if let Some(dow) = match_weekday_phrase(&norm) {
        return Some(make_cron(&format!("0 9 * * {dow}"), tz));
    }

    None
}

/// Convenience wrapper over [`parse_natural_language`] using `storage::now_ms()`.
#[must_use]
pub fn parse_nl(phrase: &str) -> Option<ScheduleSpec> {
    parse_natural_language(phrase, storage::now_ms())
}

fn make_cron(expr: &str, tz: Option<&str>) -> ScheduleSpec {
    match tz.map(str::trim).filter(|s| !s.is_empty()) {
        Some(tz_name) => ScheduleSpec::cron_tz(expr, tz_name),
        None => ScheduleSpec::Cron(expr.to_owned()),
    }
}

fn parse_relative_at(norm: &str, now_ms: i64) -> Option<ScheduleSpec> {
    let body = norm
        .strip_prefix("in ")
        .or_else(|| norm.strip_prefix("after "))
        .or_else(|| norm.strip_suffix(" sonra"))
        .or_else(|| norm.strip_suffix(" icinde"))?;

    let secs = parse_quantity_and_unit(body.trim())?;
    let delta_ms = i64::try_from(secs).ok()?.checked_mul(1_000)?;
    Some(ScheduleSpec::At {
        unix_ms: now_ms.saturating_add(delta_ms),
    })
}

fn parse_interval_every(norm: &str) -> Option<ScheduleSpec> {
    if let Some(rest) = norm.strip_prefix("every ") {
        let secs = parse_quantity_and_unit(rest.trim())?;
        return Some(ScheduleSpec::Every { seconds: secs });
    }

    // Turkish: "her 10 dakikada bir", "her 2 saatte bir", "her 15 dakika", "10 dakikada bir"
    let stripped_bir = norm.strip_suffix(" bir").unwrap_or(norm);
    if let Some(rest) = stripped_bir.strip_prefix("her ") {
        let secs = parse_quantity_and_unit(rest.trim())?;
        return Some(ScheduleSpec::Every { seconds: secs });
    }
    if let Some(rest) = norm.strip_suffix(" bir") {
        let secs = parse_quantity_and_unit(rest.trim())?;
        return Some(ScheduleSpec::Every { seconds: secs });
    }
    None
}

fn parse_quantity_and_unit(s: &str) -> Option<u64> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() != 2 {
        return None;
    }
    let count = parse_number(tokens[0])?;
    if count == 0 {
        return None;
    }
    let unit_secs = lookup_unit_seconds(tokens[1])?;
    count.checked_mul(unit_secs)
}

fn parse_number(tok: &str) -> Option<u64> {
    if let Ok(n) = tok.parse::<u64>() {
        return Some(n);
    }
    NUMBER_WORD_TABLE
        .iter()
        .find(|&&(w, _)| w == tok)
        .map(|&(_, n)| n)
}

fn lookup_unit_seconds(tok: &str) -> Option<u64> {
    UNIT_SECONDS_TABLE
        .iter()
        .find(|&&(u, _)| u == tok)
        .map(|&(_, s)| s)
}

fn parse_calendar_cron(norm: &str) -> Option<String> {
    let (prefix, hour, minute) = split_cadence_and_time(norm)?;
    let prefix = prefix.trim();

    for &(cadence, tail_fields) in CADENCE_CRON_TABLE {
        if prefix == cadence {
            let adjusted_hour = if (cadence == "her aksam" || cadence == "every evening")
                && (1..12).contains(&hour)
            {
                hour + 12
            } else {
                hour
            };
            return Some(format!("{minute} {adjusted_hour} {tail_fields}"));
        }
    }

    if let Some(dow) = match_weekday_phrase(prefix) {
        return Some(format!("{minute} {hour} * * {dow}"));
    }

    None
}

fn match_weekday_phrase(prefix: &str) -> Option<&'static str> {
    let cleaned = prefix
        .strip_prefix("every ")
        .or_else(|| prefix.strip_prefix("on "))
        .or_else(|| prefix.strip_prefix("her hafta "))
        .or_else(|| prefix.strip_prefix("her "))
        .unwrap_or(prefix)
        .trim();
    let cleaned = cleaned
        .strip_suffix(" gunleri")
        .or_else(|| cleaned.strip_suffix(" gunu"))
        .or_else(|| cleaned.strip_suffix(" sabah"))
        .or_else(|| cleaned.strip_suffix(" sabahi"))
        .unwrap_or(cleaned)
        .trim();

    // Require an explicit recurring marker ("every ...", "on ...", "her ...", "... gunleri", or plural "...s")
    let has_recurring_marker = prefix.starts_with("every ")
        || prefix.starts_with("on ")
        || prefix.starts_with("her ")
        || prefix.ends_with(" gunleri")
        || prefix.ends_with(" gunu")
        || cleaned.ends_with('s');
    if !has_recurring_marker {
        return None;
    }

    WEEKDAY_TABLE
        .iter()
        .find(|&&(name, _)| name == cleaned)
        .map(|&(_, dow)| dow)
}

/// Split a normalized phrase into `(cadence_prefix, hour, minute)` by parsing
/// a trailing time specification such as `"09:00"`, `"at 8"`, `"at 8:30 pm"`,
/// `"saat 09:00"`, or `"saat 8"`.
fn split_cadence_and_time(norm: &str) -> Option<(&str, u8, u8)> {
    let tokens: Vec<&str> = norm.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }

    // Check if the last two tokens are `<time> am` or `<time> pm`.
    let (time_tok_idx, am_pm) = match tokens.last().copied() {
        Some("am") if tokens.len() >= 3 => (tokens.len() - 2, Some(false)),
        Some("pm") if tokens.len() >= 3 => (tokens.len() - 2, Some(true)),
        _ => (tokens.len() - 1, None),
    };

    let (hour, minute) = parse_time_token(tokens[time_tok_idx], am_pm)?;

    // Strip optional connector token ("at" or "saat") immediately before the time token.
    let prefix_end_idx = if time_tok_idx > 0 && matches!(tokens[time_tok_idx - 1], "at" | "saat") {
        time_tok_idx - 1
    } else {
        time_tok_idx
    };

    if prefix_end_idx == 0 {
        return None;
    }

    // Reconstruct the prefix slice in `norm`.
    let prefix_len: usize = tokens[..prefix_end_idx]
        .iter()
        .map(|t| t.len())
        .sum::<usize>()
        + (prefix_end_idx - 1);
    Some((&norm[..prefix_len], hour, minute))
}

fn parse_time_token(tok: &str, external_pm: Option<bool>) -> Option<(u8, u8)> {
    let (core, inline_pm) = if let Some(stripped) = tok.strip_suffix("am") {
        (stripped, Some(false))
    } else if let Some(stripped) = tok.strip_suffix("pm") {
        (stripped, Some(true))
    } else {
        (tok, None)
    };

    let is_pm = external_pm.or(inline_pm);
    let (raw_h, raw_m) =
        if let Some((h_str, m_str)) = core.split_once(':').or_else(|| core.split_once('.')) {
            if m_str.len() != 2 {
                return None;
            }
            (h_str.parse::<u8>().ok()?, m_str.parse::<u8>().ok()?)
        } else {
            (core.parse::<u8>().ok()?, 0_u8)
        };

    if raw_m > 59 {
        return None;
    }

    let hour = match is_pm {
        Some(false) => {
            if !(1..=12).contains(&raw_h) {
                return None;
            }
            if raw_h == 12 { 0 } else { raw_h }
        }
        Some(true) => {
            if !(1..=12).contains(&raw_h) {
                return None;
            }
            if raw_h == 12 { 12 } else { raw_h + 12 }
        }
        None => {
            if raw_h > 23 {
                return None;
            }
            raw_h
        }
    };

    Some((hour, raw_m))
}

/// Normalize an English or Turkish phrase by ASCII-folding Turkish characters,
/// lowercasing, stripping Turkish time suffixes (`'da`, `'de`, `'ta`, `'te`),
/// and collapsing whitespace.
fn normalize_phrase(input: &str) -> String {
    let mut folded = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            'İ' | 'I' | 'ı' | 'i' => folded.push('i'),
            'Ğ' | 'ğ' => folded.push('g'),
            'Ü' | 'ü' => folded.push('u'),
            'Ş' | 'ş' => folded.push('s'),
            'Ö' | 'ö' => folded.push('o'),
            'Ç' | 'ç' => folded.push('c'),
            'Â' | 'â' => folded.push('a'),
            '’' => folded.push('\''),
            ',' | ';' => folded.push(' '),
            c => folded.push(c.to_ascii_lowercase()),
        }
    }

    let mut tokens = Vec::new();
    for raw_tok in folded.split_whitespace() {
        let tok = strip_turkish_apostrophe_suffix(raw_tok);
        if !tok.is_empty() {
            tokens.push(tok);
        }
    }
    tokens.join(" ")
}

fn strip_turkish_apostrophe_suffix(tok: &str) -> &str {
    if let Some((stem, suffix)) = tok.split_once('\'')
        && !stem.is_empty()
        && matches!(
            suffix,
            "da" | "de" | "ta" | "te" | "nda" | "nde" | "dan" | "den" | "inde" | "unda"
        )
    {
        return stem;
    }
    tok
}

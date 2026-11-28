//! Conservative, deterministic hints for outreach replies. These hints are not proof of
//! identity or delivery status and must not be used alone to alter a recipient's state.

/// A first-pass classification of untrusted message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyClass {
    /// A delivery-status report explicitly says delivery failed permanently.
    HardBounce,
    /// A delivery-status report explicitly says delivery is temporarily delayed.
    SoftBounce,
    /// An unquoted line asks to stop future messages.
    Stop,
    /// An automatic response indicates the sender is away.
    OutOfOffice,
    /// No sufficiently specific signal was found.
    Other,
}

/// The lower-cased lines of the new part of a reply: everything before the first quote marker,
/// signature separator or "-----Original Message-----" line.
fn unquoted_lines(body: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('>')
            || line == "--"
            || line.starts_with("-----Original Message-----")
            || (line.starts_with("On ") && line.ends_with(" wrote:"))
        {
            break;
        }
        lines.push(line.to_lowercase());
    }
    lines
}

/// Words an out-of-office reply is recognised by.
const MARKERS: [&str; 5] = [
    "out of office",
    "automatic reply",
    "auto reply",
    "ofis dışında",
    "otomatik yanıt",
];

/// Classify a reply without a model. Only the unquoted part of the message is examined.
/// A result is a hint; the caller must correlate it with an outgoing message and recipient
/// before taking any action. Delivery-status text is untrusted and can be forged.
#[must_use]
pub fn classify_reply(subject: &str, body: &str, auto_submitted: Option<&str>) -> ReplyClass {
    let lines = unquoted_lines(body);
    // Each RFC 3464 recipient has its own field block. Never combine the action
    // for one recipient with another recipient's status.
    let mut bounce = None;
    for block in lines.split(String::is_empty) {
        let failed = block.iter().any(|line| line == "action: failed");
        let delayed = block.iter().any(|line| line == "action: delayed");
        let status = block
            .iter()
            .find_map(|line| line.strip_prefix("status:").map(str::trim));
        if failed && status.is_some_and(|value| value.starts_with("5.")) {
            return ReplyClass::HardBounce;
        }
        if delayed && status.is_some_and(|value| value.starts_with("4.")) {
            bounce = Some(ReplyClass::SoftBounce);
        }
    }
    if let Some(class) = bounce {
        return class;
    }
    if lines.iter().any(|l| {
        matches!(
            l.trim_matches(|c: char| c.is_whitespace() || c == '.' || c == '!'),
            "stop" | "durdur" | "unsubscribe"
        )
    }) {
        return ReplyClass::Stop;
    }
    let automatic = auto_submitted.is_some_and(|a| {
        let a = a.trim();
        !a.is_empty() && !a.eq_ignore_ascii_case("no")
    });
    let subject = subject.to_lowercase();
    let marked = |text: &str| MARKERS.iter().any(|m| text.contains(m));
    // A mailbox that cannot show the `Auto-Submitted` header (`None`, as opposed to a header
    // that is empty or says "no") is judged by the subject alone, and only by the prefix mail
    // programs write ("Automatic reply: ...", "Out of Office: ...").
    let away = match auto_submitted {
        Some(_) => automatic && (marked(&subject) || lines.iter().any(|l| marked(l))),
        None => MARKERS.iter().any(|m| subject.trim_start().starts_with(m)),
    };
    if away {
        return ReplyClass::OutOfOffice;
    }
    ReplyClass::Other
}

const MONTHS: [(&str, u32); 37] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("sept", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
    ("ocak", 1),
    ("şubat", 2),
    ("mart", 3),
    ("nisan", 4),
    ("mayıs", 5),
    ("haziran", 6),
    ("temmuz", 7),
    ("ağustos", 8),
    ("eylül", 9),
    ("ekim", 10),
    ("kasım", 11),
    ("aralık", 12),
    ("subat", 2),
];

/// Words that say a date on the same line is the day the person is back.
const RETURN_MARKERS: [&str; 8] = [
    "return",
    "back",
    "until",
    "dönüş",
    "döne",
    "kadar",
    "tarihinde",
    "tarihine",
];

/// How far ahead a stated return date is believed (the text is untrusted: a date years away
/// would park a follow-up for ever).
pub const MAX_RETURN_DAYS: u64 = 60;

/// Days since 1970-01-01 of a civil date (proleptic Gregorian), `None` for an impossible one.
fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + u32::from(leap),
        _ => return None,
    };
    if d == 0 || d > dim {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn month_named(word: &str) -> Option<u32> {
    MONTHS.iter().find(|(n, _)| *n == word).map(|(_, m)| *m)
}

fn number(word: &str) -> Option<u32> {
    let digits = word.trim_end_matches(char::is_alphabetic);
    (!digits.is_empty() && digits.len() <= 4 && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The first calendar date in `line` (lower case): `2026-12-05`, `05.12.2026`, `5 december 2026`,
/// `december 5, 2026`, `5 aralık 2026`. A year is required. `a/b/yyyy` counts only when one
/// side is above 12 (otherwise day-first and month-first cannot be told apart).
fn date_in(line: &str) -> Option<(i64, u32, u32)> {
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '.' || c == '/'))
        .filter(|w| !w.is_empty())
        .collect();
    for (i, w) in words.iter().enumerate() {
        let w = w.trim_matches('.');
        let parts: Vec<&str> = w.split(['-', '.', '/']).collect();
        if parts.len() == 3 {
            let n: Vec<Option<u32>> = parts.iter().map(|p| number(p)).collect();
            if let [Some(a), Some(b), Some(c)] = n[..] {
                if parts[0].len() == 4 && w.contains('-') {
                    return Some((i64::from(a), b, c));
                }
                if parts[2].len() == 4 {
                    if w.contains('.') || (w.contains('/') && a > 12) {
                        return Some((i64::from(c), b, a));
                    }
                    if w.contains('/') && b > 12 {
                        return Some((i64::from(c), a, b));
                    }
                }
            }
        }
        let next = |k: usize| words.get(i + k).map(|x| x.trim_matches('.'));
        if let Some(day) = number(w)
            && let Some(m) = next(1).and_then(month_named)
            && let Some(y) = next(2).and_then(number)
            && y >= 1000
        {
            return Some((i64::from(y), m, day));
        }
        if let Some(m) = month_named(w)
            && let Some(day) = next(1).and_then(number)
            && let Some(y) = next(2).and_then(number)
            && y >= 1000
        {
            return Some((i64::from(y), m, day));
        }
    }
    None
}

/// The day an out-of-office reply says the person is back, as Unix seconds (09:00 UTC that day),
/// or `None` when no date is stated on a line with a word like "return", "back" or "until", the
/// date is not in the future, or it is further than [`MAX_RETURN_DAYS`] away. Only the new part
/// of the reply is read. The text is untrusted: the caller still decides what to do with it.
#[must_use]
pub fn stated_return_date(body: &str, now_secs: u64) -> Option<u64> {
    for line in unquoted_lines(body) {
        if !RETURN_MARKERS.iter().any(|m| line.contains(m)) {
            continue;
        }
        let Some((y, m, d)) = date_in(&line) else {
            continue;
        };
        let Some(days) = days_from_civil(y, m, d) else {
            continue;
        };
        let Some(at) = u64::try_from(days)
            .ok()
            .and_then(|d| d.checked_mul(86_400))
            .map(|s| s + 9 * 3600)
        else {
            continue;
        };
        if at > now_secs && at <= now_secs + MAX_RETURN_DAYS * 86_400 {
            return Some(at);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_only_in_the_new_reply() {
        assert_eq!(
            classify_reply("Re: Hello", "STOP!\n\n> reply STOP", None),
            ReplyClass::Stop
        );
        assert_eq!(
            classify_reply("Re: Hello", "Thanks\n> reply STOP", None),
            ReplyClass::Other
        );
        assert_eq!(classify_reply("", "DURDUR", None), ReplyClass::Stop);
        assert_eq!(
            classify_reply("", "Please stop by tomorrow", None),
            ReplyClass::Other
        );
    }

    #[test]
    fn only_permanent_explicit_delivery_failures_count() {
        assert_eq!(
            classify_reply("", "Action: failed\nStatus: 5.1.1", None),
            ReplyClass::HardBounce
        );
        assert_eq!(
            classify_reply("", "Action: delayed\nStatus: 4.2.0", None),
            ReplyClass::SoftBounce
        );
        assert_eq!(
            classify_reply("", "Action: failed\nStatus: 4.2.0", None),
            ReplyClass::Other
        );
    }

    #[test]
    fn delivery_status_fields_from_different_recipients_do_not_mix() {
        assert_eq!(
            classify_reply(
                "",
                "Action: failed\nStatus: 4.2.0\n\nAction: delayed\nStatus: 5.1.1",
                None
            ),
            ReplyClass::Other
        );
        assert_eq!(
            classify_reply(
                "",
                "Action: delayed\nStatus: 4.2.0\n\nAction: failed\nStatus: 5.1.1",
                None
            ),
            ReplyClass::HardBounce
        );
    }

    #[test]
    fn away_requires_an_automatic_response_signal() {
        assert_eq!(
            classify_reply("Out of office", "Back next week", Some("auto-replied")),
            ReplyClass::OutOfOffice
        );
        assert_eq!(
            classify_reply("Out of office", "I saw your email", Some("no")),
            ReplyClass::Other
        );
        // A header that is present but empty is "not automatic" too.
        assert_eq!(
            classify_reply("Out of office", "Back next week", Some("")),
            ReplyClass::Other
        );
    }

    #[test]
    fn a_mailbox_without_the_header_is_judged_by_the_subject_prefix() {
        assert_eq!(
            classify_reply("Automatic reply: Hello", "Away.", None),
            ReplyClass::OutOfOffice
        );
        assert_eq!(
            classify_reply("Re: our out of office policy", "Hi", None),
            ReplyClass::Other
        );
        assert_eq!(
            classify_reply("Re: Hello", "I am out of office today", None),
            ReplyClass::Other
        );
    }

    // 2026-11-28 12:00:00 UTC
    const NOW: u64 = 1_795_867_200;

    #[test]
    fn a_stated_return_date_is_read_in_the_common_formats() {
        let at = |text: &str| stated_return_date(text, NOW);
        let dec5 = 20_792 * 86_400 + 9 * 3600; // 2026-12-05
        assert_eq!(days_from_civil(2026, 12, 5), Some(20_792));
        assert_eq!(
            at("I am out of office and will return on 2026-12-05."),
            Some(dec5)
        );
        assert_eq!(at("Back in the office on 5 December 2026"), Some(dec5));
        assert_eq!(at("I will be back December 5, 2026."), Some(dec5));
        assert_eq!(at("5 Aralık 2026 tarihinde dönüş yapacağım"), Some(dec5));
        assert_eq!(at("Out until 05.12.2026"), Some(dec5));
        assert_eq!(at("Out until 25/12/2026"), Some(20_812 * 86_400 + 9 * 3600));
    }

    #[test]
    fn an_unusable_return_date_is_not_believed() {
        let at = |text: &str| stated_return_date(text, NOW);
        // Ambiguous, no year, in the past, too far away, impossible, not a return date, quoted.
        assert_eq!(at("back on 05/12/2026"), None);
        assert_eq!(at("back on 5 December"), None);
        assert_eq!(at("back on 2026-11-01"), None);
        assert_eq!(at("back on 2030-01-01"), None);
        assert_eq!(at("back on 2026-02-30"), None);
        assert_eq!(at("The meeting is on 2026-12-05"), None);
        assert_eq!(at("Thanks\n> I will be back on 2026-12-05"), None);
    }
}

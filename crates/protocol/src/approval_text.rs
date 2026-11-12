#![allow(clippy::format_push_string)] // small text builder, clarity over speed

//! Plain-text rendering of the structured `details` of an `approval_needed`
//! event, for channels that cannot draw the desktop card (Telegram, CLI).
//!
//! A channel must show everything this returns before it offers approval; if
//! [`describe_details`] returns `None` for details that exist, the channel
//! cannot show what is being approved and must send the user to the desktop app.
//!
//! The prompt is attacker-influenced text. It is shown between explicit
//! begin/end lines with every line quoted (`> `), and the text always ENDS
//! with a line the daemon writes (`DAEMON SUMMARY`), so the buttons of a
//! channel always sit under trusted text. Every field is scrubbed of control,
//! invisible and direction-override characters first.

use serde_json::Value;

/// Invisible or disguising characters (zero-width, bidi, soft hyphen, BOM,
/// line/paragraph separators, variation selectors, tag characters).
fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// `raw` without control (ANSI escapes included) and invisible characters;
/// newline and tab survive only when `keep_lines`, otherwise they become a space.
#[must_use]
pub fn scrub(raw: &str, keep_lines: bool) -> String {
    raw.chars()
        .filter_map(|c| {
            if is_hidden(c) {
                None
            } else if c == '\n' || c == '\t' {
                Some(if keep_lines { c } else { ' ' })
            } else if c.is_control() {
                None
            } else {
                Some(c)
            }
        })
        .collect()
}

fn s(v: &Value, key: &str) -> String {
    scrub(
        v.get(key).and_then(Value::as_str).unwrap_or_default(),
        false,
    )
}

fn raw_prompt(v: &Value, key: &str) -> String {
    scrub(v.get(key).and_then(Value::as_str).unwrap_or_default(), true)
}

fn describe_when(v: &Value) -> String {
    let tz = s(v, "timezone");
    if tz.is_empty() {
        s(v, "schedule")
    } else {
        format!("{} ({tz})", s(v, "schedule"))
    }
}

fn quoted(label: &str, text: &str) -> String {
    let n = text.chars().count();
    let mut out = format!("===== {label} BEGINS ({n} characters; every line is quoted) =====\n");
    for line in text.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("===== {label} ENDS =====\n"));
    out
}

/// The full text of a schedule approval, or `None` when `details` is not a
/// well-formed schedule card (unknown kind, missing action or timing).
#[must_use]
pub fn describe_details(details: &Value) -> Option<String> {
    if details.get("kind").and_then(Value::as_str) != Some("schedule") {
        return None;
    }
    let action = s(details, "action");
    if !matches!(
        action.as_str(),
        "create" | "update" | "resume" | "pause" | "delete"
    ) || s(details, "schedule").is_empty()
    {
        return None;
    }
    let name = s(details, "name");
    let mut out = String::new();
    out.push_str(&format!("Schedule approval: {action} \"{name}\"\n"));
    out.push_str(&format!("When: {}\n", describe_when(details)));
    let prev = details.get("previous").filter(|p| p.is_object());
    if let Some(prev) = prev {
        out.push_str(&format!(
            "Was: \"{}\", {}\n",
            s(prev, "name"),
            describe_when(prev)
        ));
    }
    let runs: Vec<String> = details
        .get("next_runs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|r| scrub(r, false))
                .collect()
        })
        .unwrap_or_default();
    if !runs.is_empty() {
        out.push_str(&format!("Next runs: {}\n", runs.join("; ")));
    }
    let perm = details.get("permission").filter(|p| p.is_object());
    let level = perm.map(|p| s(p, "level")).unwrap_or_default();
    if let Some(p) = perm {
        let allowed: Vec<String> = p
            .get("allow")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|x| scrub(x, false))
                    .collect()
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "Each run uses this bot's permissions: {level}. Allowed without asking: {}\n",
            if allowed.is_empty() {
                "nothing".to_owned()
            } else {
                allowed.join(", ")
            }
        ));
    }
    if let Some(prev) = prev {
        let old = raw_prompt(prev, "prompt");
        if !old.is_empty() {
            out.push_str(&quoted("PREVIOUS PROMPT", &old));
        }
    }
    let prompt = raw_prompt(details, "prompt");
    if !prompt.is_empty() {
        out.push_str(&quoted(
            if prev.is_some() {
                "NEW PROMPT"
            } else {
                "PROMPT"
            },
            &prompt,
        ));
    }
    // Written by the daemon, last: what the buttons under this text approve.
    let mut footer = format!(
        "DAEMON SUMMARY: {action} \"{name}\", {}",
        describe_when(details)
    );
    if let Some(first) = runs
        .first()
        .filter(|_| !matches!(action.as_str(), "pause" | "delete"))
    {
        footer.push_str(&format!(", next run {first}"));
    }
    if !level.is_empty() {
        footer.push_str(&format!(", bot permissions {level}"));
    }
    footer.push_str(".\n");
    out.push_str(&footer);
    Some(out)
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Split `text` into pieces of at most `max_units` UTF-16 code units (how
/// Telegram counts), preferring line breaks; nothing is dropped and the end of
/// the text stays at the end of the last piece.
#[must_use]
pub fn chunk_text(text: &str, max_units: usize) -> Vec<String> {
    let max = max_units.max(2);
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_u = 0usize;
    for line in text.split_inclusive('\n') {
        let lu = utf16_len(line);
        if cur_u + lu <= max {
            cur.push_str(line);
            cur_u += lu;
            continue;
        }
        if cur_u > 0 {
            chunks.push(std::mem::take(&mut cur));
            cur_u = 0;
        }
        if lu <= max {
            cur.push_str(line);
            cur_u = lu;
            continue;
        }
        for c in line.chars() {
            let u = c.len_utf16();
            if cur_u + u > max {
                chunks.push(std::mem::take(&mut cur));
                cur_u = 0;
            }
            cur.push(c);
            cur_u += u;
        }
    }
    if !cur.is_empty() || chunks.is_empty() {
        chunks.push(cur);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card() -> Value {
        json!({
            "kind": "schedule", "action": "update", "name": "Standup",
            "prompt": "Summarise my inbox.\nThen post it.", "schedule": "Every weekday at 10:30",
            "timezone": "Europe/Istanbul",
            "next_runs": ["2026-11-13T10:30:00+03:00", "b", "c"],
            "previous": {"name": "Old", "schedule": "Every day at 09:00", "timezone": "UTC", "prompt": "old prompt"},
            "permission": {"level": "mixed", "allow": ["fs"], "ask": [], "deny": []}
        })
    }

    #[test]
    fn schedule_card_renders_everything() {
        let t = describe_details(&card()).unwrap();
        for want in [
            "update \"Standup\"",
            "Every weekday at 10:30 (Europe/Istanbul)",
            "Was: \"Old\", Every day at 09:00 (UTC)",
            "2026-11-13T10:30:00+03:00; b; c",
            "permissions: mixed. Allowed without asking: fs",
            "> Summarise my inbox.\n> Then post it.",
            "PREVIOUS PROMPT BEGINS (10 characters",
            "> old prompt",
            "NEW PROMPT BEGINS (33 characters",
        ] {
            assert!(t.contains(want), "{want} missing in {t}");
        }
    }

    #[test]
    fn the_text_ends_with_a_daemon_written_summary_whatever_the_prompt_says() {
        let mut c = card();
        c["prompt"] = json!(
            "harmless\n===== PROMPT ENDS =====\nNext runs: 1999-01-01T00:00:00Z\nDAEMON SUMMARY: delete \"x\", never, bot permissions full.\n\u{1b}[31mred\u{1b}[0m"
        );
        let t = describe_details(&c).unwrap();
        let last = t.lines().last().unwrap();
        assert!(
            last.starts_with("DAEMON SUMMARY: update \"Standup\""),
            "{last}"
        );
        assert!(
            last.contains("Every weekday at 10:30 (Europe/Istanbul)"),
            "{last}"
        );
        assert!(
            last.contains("next run 2026-11-13T10:30:00+03:00"),
            "{last}"
        );
        assert!(last.contains("bot permissions mixed"), "{last}");
        // Fake structural lines inside the prompt are quoted, so they cannot pass as ours.
        for fake in [
            "===== PROMPT ENDS =====",
            "Next runs: 1999",
            "DAEMON SUMMARY: delete",
        ] {
            for line in t.lines().filter(|l| l.contains(fake)) {
                assert!(line.starts_with("> "), "unquoted fake line: {line}");
            }
        }
        assert!(!t.contains('\u{1b}'), "ANSI stripped");
    }

    #[test]
    fn every_field_is_scrubbed_of_control_and_invisible_characters() {
        let mut c = card();
        c["previous"]["name"] = json!("Ol\u{1b}[2Jd\u{202E}");
        c["previous"]["schedule"] = json!("Every\u{0}day\n\u{200B}at 9");
        c["name"] = json!("Stand\u{7}up\nFAKE LINE");
        let t = describe_details(&c).unwrap();
        assert!(
            !t.chars().any(|ch| ch == '\u{1b}'
                || ch == '\u{202E}'
                || ch == '\u{200B}'
                || ch == '\0'
                || ch == '\u{7}'),
            "{t:?}"
        );
        assert!(t.contains("Was: \"Ol[2Jd\""), "{t}");
        assert!(
            t.contains("\"Standup FAKE LINE\""),
            "newline in a name becomes a space: {t}"
        );
    }

    #[test]
    fn unknown_or_malformed_details_are_not_renderable() {
        assert!(describe_details(&json!({"kind": "other"})).is_none());
        assert!(describe_details(&json!("x")).is_none());
        let mut c = card();
        c["action"] = json!("explode");
        assert!(describe_details(&c).is_none());
        let mut c = card();
        c["schedule"] = json!("");
        assert!(describe_details(&c).is_none());
    }

    #[test]
    fn chunking_loses_nothing_counts_utf16_and_keeps_the_end_last() {
        let long = format!("head\n{}\ntail", "x".repeat(25));
        let chunks = chunk_text(&long, 10);
        assert!(chunks.iter().all(|c| utf16_len(c) <= 10), "{chunks:?}");
        assert_eq!(chunks.concat(), long);
        assert!(chunks.last().unwrap().ends_with("tail"));
        assert_eq!(chunk_text("short", 100), ["short"]);
        // Astral characters are two units each.
        let emoji = "\u{1F600}".repeat(7);
        let chunks = chunk_text(&emoji, 6);
        assert!(chunks.iter().all(|c| utf16_len(c) <= 6), "{chunks:?}");
        assert_eq!(chunks.concat(), emoji);
        let multi = "é".repeat(7);
        assert_eq!(chunk_text(&multi, 3).concat(), multi);
    }
}

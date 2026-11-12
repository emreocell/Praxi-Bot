#![allow(clippy::format_push_string)] // small text builder, clarity over speed

//! Plain-text rendering of the structured `details` of an `approval_needed`
//! event, for channels that cannot draw the desktop card (Telegram, CLI).
//!
//! A channel must show everything this returns before it offers approval; if
//! [`describe_details`] returns `None` for details that exist, the channel
//! cannot show what is being approved and must send the user to the desktop app.

use serde_json::Value;

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn describe_when(v: &Value) -> String {
    let tz = s(v, "timezone");
    if tz.is_empty() {
        s(v, "schedule").to_owned()
    } else {
        format!("{} ({tz})", s(v, "schedule"))
    }
}

/// The full text of a schedule approval, or `None` when `details` is not a
/// well-formed schedule card (unknown kind, missing action or timing).
#[must_use]
pub fn describe_details(details: &Value) -> Option<String> {
    if details.get("kind").and_then(Value::as_str) != Some("schedule") {
        return None;
    }
    let action = s(details, "action");
    if !matches!(action, "create" | "update" | "resume" | "pause" | "delete")
        || s(details, "schedule").is_empty()
    {
        return None;
    }
    let mut out = String::new();
    let name = s(details, "name");
    out.push_str(&format!("Schedule: {action} \"{name}\"\n"));
    out.push_str(&format!("When: {}\n", describe_when(details)));
    if let Some(prev) = details.get("previous").filter(|p| p.is_object()) {
        out.push_str(&format!(
            "Was: \"{}\", {}\n",
            s(prev, "name"),
            describe_when(prev)
        ));
    }
    let runs: Vec<&str> = details
        .get("next_runs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !runs.is_empty() {
        out.push_str(&format!("Next runs: {}\n", runs.join("; ")));
    }
    if let Some(p) = details.get("permission").filter(|p| p.is_object()) {
        let list = |k: &str| -> String {
            p.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        out.push_str(&format!(
            "Each run uses this bot's permissions: {}. Allowed without asking: {}\n",
            s(p, "level"),
            match list("allow") {
                a if a.is_empty() => "nothing".to_owned(),
                a => a,
            }
        ));
    }
    let prompt = s(details, "prompt");
    if !prompt.is_empty() {
        out.push_str(&format!(
            "Prompt ({} characters, runs every time):\n{prompt}\n",
            prompt.chars().count()
        ));
    }
    if let Some(prev) = details.get("previous").filter(|p| p.is_object()) {
        let old = s(prev, "prompt");
        if !old.is_empty() {
            out.push_str(&format!(
                "Previous prompt ({} characters):\n{old}\n",
                old.chars().count()
            ));
        }
    }
    Some(out)
}

/// Split `text` into pieces of at most `max_chars` characters, preferring line
/// breaks; nothing is dropped.
#[must_use]
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for line in text.split_inclusive('\n') {
        let mut rest = line;
        while !rest.is_empty() {
            let len = rest.chars().count();
            if cur_len + len <= max {
                cur.push_str(rest);
                cur_len += len;
                break;
            }
            if cur_len > 0 && len <= max {
                chunks.push(std::mem::take(&mut cur));
                cur_len = 0;
                continue;
            }
            let take = max - cur_len;
            let end = rest.char_indices().nth(take).map_or(rest.len(), |(i, _)| i);
            cur.push_str(&rest[..end]);
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
            rest = &rest[end..];
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
    fn schedule_card_renders_everything_verbatim() {
        let t = describe_details(&card()).unwrap();
        for want in [
            "update \"Standup\"",
            "Every weekday at 10:30 (Europe/Istanbul)",
            "Was: \"Old\", Every day at 09:00 (UTC)",
            "2026-11-13T10:30:00+03:00; b; c",
            "permissions: mixed. Allowed without asking: fs",
            "Summarise my inbox.\nThen post it.",
            "Previous prompt (10 characters):\nold prompt",
        ] {
            assert!(t.contains(want), "{want} missing in {t}");
        }
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
    fn chunking_loses_nothing_and_respects_the_limit() {
        let long = format!("head\n{}\ntail", "x".repeat(25));
        let chunks = chunk_text(&long, 10);
        assert!(chunks.iter().all(|c| c.chars().count() <= 10), "{chunks:?}");
        assert_eq!(chunks.concat(), long);
        assert_eq!(chunk_text("short", 100), ["short"]);
        let multi = "é".repeat(7);
        assert_eq!(chunk_text(&multi, 3).concat(), multi);
    }
}

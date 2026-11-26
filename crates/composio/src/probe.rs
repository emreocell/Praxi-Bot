//! "Test connection": one fixed, read-only call per well-known app that proves
//! the account works and tells the owner which account it is (Drive: the
//! account and three file names; Gmail: the address and three subjects;
//! Calendar: the calendars; GitHub: the user).
//!
//! The owner presses the button, so this runs outside any bot run: the result
//! goes to the owner's screen only, never to a model.  Which action runs is
//! fixed here, never chosen by a caller, and only well-known read-only actions
//! are listed.
//!
//! UNVERIFIED against the live API: the action slugs and the field names of
//! their answers come from Composio's public tool pages.  Everything is read
//! tolerantly (any of several field names, anywhere in the answer) and only
//! arguments the discovered schema declares are sent, so a renamed argument
//! degrades the sample instead of failing the test.

use serde_json::{Value, json};

use crate::schema::clean_text;

/// One call of a probe.
pub(crate) struct Step {
    pub action: &'static str,
    /// Candidate arguments; only those the action's schema declares are sent.
    pub args: Value,
}

/// The fixed probe of one app.
pub(crate) struct Probe {
    /// Answers "which account": its address or user name.  Best effort.
    pub identity: Option<Step>,
    /// Field names that hold the account in the identity answer, best first.
    pub identity_keys: &'static [&'static str],
    /// Answers "what is in it": a few items.  Best effort when `identity` exists.
    pub sample: Option<Step>,
    /// Field names that name one item, best first.
    pub item_keys: &'static [&'static str],
}

/// The probe of `slug`, when Praxi knows a safe one.
pub(crate) fn probe_for(slug: &str) -> Option<Probe> {
    Some(match slug {
        "googledrive" => Probe {
            identity: Some(Step {
                action: "GOOGLEDRIVE_GET_ABOUT",
                args: json!({"fields": "user"}),
            }),
            identity_keys: &["emailAddress", "email", "displayName"],
            sample: Some(Step {
                action: "GOOGLEDRIVE_LIST_FILES",
                args: json!({"pageSize": 3, "page_size": 3, "orderBy": "modifiedTime desc"}),
            }),
            item_keys: &["name", "title"],
        },
        "gmail" => Probe {
            identity: Some(Step {
                action: "GMAIL_GET_PROFILE",
                args: json!({"user_id": "me"}),
            }),
            identity_keys: &["emailAddress", "email"],
            sample: Some(Step {
                action: "GMAIL_FETCH_EMAILS",
                args: json!({"max_results": 3, "user_id": "me", "include_payload": false}),
            }),
            item_keys: &["subject"],
        },
        "googlecalendar" => Probe {
            identity: None,
            identity_keys: &["id"],
            sample: Some(Step {
                action: "GOOGLECALENDAR_LIST_CALENDARS",
                args: json!({"max_results": 3}),
            }),
            item_keys: &["summary", "name"],
        },
        "github" => Probe {
            identity: Some(Step {
                action: "GITHUB_GET_THE_AUTHENTICATED_USER",
                args: json!({}),
            }),
            identity_keys: &["login", "email", "name"],
            sample: None,
            item_keys: &[],
        },
        _ => return None,
    })
}

/// What a test found, as plain text for the owner's screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// The account the app is connected as.
    pub account: Option<String>,
    /// Up to three sample names (file names, subjects, calendars).
    pub items: Vec<String>,
}

/// Keep only the arguments an action's schema declares.
pub(crate) fn fit_args(schema: &Value, candidate: &Value) -> Value {
    let props = schema.get("properties").and_then(Value::as_object);
    let kept: serde_json::Map<String, Value> = candidate
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| props.is_some_and(|p| p.contains_key(k.as_str())))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Value::Object(kept)
}

/// First string stored under any of `keys` (in the order of `keys`), anywhere.
pub(crate) fn find_string(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| first_under(v, key, 0))
}

fn first_under(v: &Value, key: &str, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    match v {
        Value::Object(map) => {
            if let Some(s) = map.get(key).and_then(Value::as_str)
                && !s.trim().is_empty()
            {
                return Some(clean_text(s, 120));
            }
            map.values().find_map(|c| first_under(c, key, depth + 1))
        }
        Value::Array(a) => a.iter().find_map(|c| first_under(c, key, depth + 1)),
        _ => None,
    }
}

/// Up to three item names: the first array of objects that carry one of `keys`.
pub(crate) fn sample_items(v: &Value, keys: &[&str]) -> Vec<String> {
    fn walk(v: &Value, keys: &[&str], depth: usize) -> Option<Vec<String>> {
        if depth > 8 {
            return None;
        }
        match v {
            Value::Array(a) => {
                let names: Vec<String> = a
                    .iter()
                    .filter_map(|i| {
                        keys.iter().find_map(|k| {
                            i.get(*k)
                                .and_then(Value::as_str)
                                .filter(|s| !s.trim().is_empty())
                                .map(|s| clean_text(s, 80))
                        })
                    })
                    .take(3)
                    .collect();
                if names.is_empty() {
                    a.iter().find_map(|c| walk(c, keys, depth + 1))
                } else {
                    Some(names)
                }
            }
            Value::Object(m) => m.values().find_map(|c| walk(c, keys, depth + 1)),
            _ => None,
        }
    }
    if keys.is_empty() {
        return Vec::new();
    }
    walk(v, keys, 0).unwrap_or_default()
}

/// The calendar owner of a calendar list: the entry marked `primary`.
pub(crate) fn primary_calendar(v: &Value) -> Option<String> {
    fn walk(v: &Value, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match v {
            Value::Array(a) => a.iter().find_map(|c| walk(c, depth + 1)),
            Value::Object(m) => {
                if m.get("primary").and_then(Value::as_bool) == Some(true)
                    && let Some(id) = m.get("id").and_then(Value::as_str)
                {
                    return Some(clean_text(id, 120));
                }
                m.values().find_map(|c| walk(c, depth + 1))
            }
            _ => None,
        }
    }
    walk(v, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_well_known_apps_have_a_probe_and_all_of_them_read() {
        for slug in ["googledrive", "gmail", "googlecalendar", "github"] {
            let p = probe_for(slug).expect(slug);
            for step in p.identity.iter().chain(p.sample.iter()) {
                let a = step.action;
                assert!(
                    a.starts_with(&slug.to_ascii_uppercase()),
                    "{a} belongs to {slug}"
                );
                assert!(
                    ["GET", "LIST", "FETCH"].iter().any(|v| a.contains(v)),
                    "{a} must be a read"
                );
                assert!(
                    ![
                        "SEND", "DELETE", "CREATE", "UPDATE", "TRASH", "REMOVE", "PATCH"
                    ]
                    .iter()
                    .any(|v| a.contains(v)),
                    "{a} must not change anything"
                );
            }
        }
        assert!(probe_for("slack").is_none());
        assert!(probe_for("../x").is_none());
    }

    #[test]
    fn arguments_are_cut_down_to_what_the_schema_declares() {
        let schema = json!({"type":"object","properties":{"pageSize":{"type":"integer"}}});
        let got = fit_args(&schema, &json!({"pageSize": 3, "page_size": 3, "x": 1}));
        assert_eq!(got, json!({"pageSize": 3}));
        assert_eq!(fit_args(&json!(false), &json!({"a": 1})), json!({}));
    }

    #[test]
    fn the_account_is_found_anywhere_in_the_answer() {
        let v = json!({"data":{"response_data":{"user":{"emailAddress":"me@example.com"}}}});
        assert_eq!(
            find_string(&v, &["emailAddress", "email"]).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(find_string(&v, &["login"]), None);
    }

    #[test]
    fn three_names_come_from_the_first_list_that_has_them() {
        let v = json!({"data":{"files":[
            {"id":"1","name":"Budget"},{"id":"2","name":"Plan"},
            {"id":"3","name":"Notes"},{"id":"4","name":"Four"}]}});
        assert_eq!(sample_items(&v, &["name"]), ["Budget", "Plan", "Notes"]);
        assert_eq!(sample_items(&v, &["subject"]), Vec::<String>::new());
        assert_eq!(sample_items(&v, &[]), Vec::<String>::new());
    }

    #[test]
    fn the_primary_calendar_names_the_account() {
        let v = json!({"items":[{"id":"x@group","primary":false},{"id":"me@example.com","primary":true}]});
        assert_eq!(primary_calendar(&v).as_deref(), Some("me@example.com"));
        assert_eq!(primary_calendar(&json!({"items":[]})), None);
    }

    #[test]
    fn control_characters_in_third_party_text_are_flattened() {
        let v = json!({"files":[{"name":"a\u{1b}[31mred"}]});
        let got = sample_items(&v, &["name"]);
        assert!(!got[0].contains('\u{1b}'));
    }
}

//! Validator for skill index documents (format version 1, see `docs/skills-index.md`).
//!
//! Unlike [`super::source::parse_index`], which stops at the first problem the
//! installer cares about, this collects every problem so an index author can fix
//! them in one pass. It never touches the network; `check_content` in the CLI
//! does the optional fingerprint fetch.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::source::{MAX_INDEX_BYTES, validate_commit_pin, validate_git_url, validate_subdir};
use crate::skill::{MAX_DESCRIPTION_LEN, validate_name};

/// Index format version this validator understands.
pub const INDEX_VERSION: u64 = 1;
/// Most entries one index may list.
pub const MAX_INDEX_ENTRIES: usize = 1000;
/// Most tags on one entry.
pub const MAX_TAGS: usize = 10;
/// Longest tag.
pub const MAX_TAG_LEN: usize = 32;

/// Top-level keys of version 1.
pub const TOP_LEVEL_KEYS: &[&str] = &["version", "name", "description", "homepage", "skills"];
/// Entry keys of version 1.
pub const ENTRY_KEYS: &[&str] = &[
    "name",
    "description",
    "git",
    "commit",
    "subdir",
    "sha256",
    "tags",
    "author",
    "license",
    "homepage",
    "min_praxi_version",
];

/// Everything found in one index document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexReport {
    /// Number of entries seen.
    pub entries: usize,
    /// Problems that make the index invalid.
    pub errors: Vec<String>,
    /// Things that are accepted but probably unintended (unknown fields, no description).
    pub warnings: Vec<String>,
}

impl IndexReport {
    /// True when there is no error (warnings are allowed).
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate the bytes of an index document.
#[must_use]
pub fn validate_index(bytes: &[u8]) -> IndexReport {
    let mut r = IndexReport::default();
    if bytes.len() > MAX_INDEX_BYTES {
        r.errors.push(format!(
            "document is {} bytes, the limit is {MAX_INDEX_BYTES}",
            bytes.len()
        ));
        return r;
    }
    let doc: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            r.errors.push(format!("not valid JSON: {e}"));
            return r;
        }
    };
    let Some(top) = doc.as_object() else {
        r.errors.push("the document must be a JSON object".into());
        return r;
    };
    match top.get("version").and_then(Value::as_u64) {
        Some(INDEX_VERSION) => {}
        Some(v) => r.errors.push(format!(
            "unsupported index version {v}; this validator knows version {INDEX_VERSION}"
        )),
        None => r
            .errors
            .push("`version` is required and must be the integer 1".into()),
    }
    unknown_keys(top, TOP_LEVEL_KEYS, "the index", &mut r);
    for key in ["name", "description"] {
        if let Some(v) = top.get(key)
            && !v.is_string()
        {
            r.errors.push(format!("`{key}` must be a string"));
        }
    }
    if let Some(h) = top.get("homepage") {
        check_https(h, "`homepage`", &mut r.errors);
    }
    let Some(skills) = top.get("skills").and_then(Value::as_array) else {
        r.errors
            .push("`skills` is required and must be an array".into());
        return r;
    };
    r.entries = skills.len();
    if skills.len() > MAX_INDEX_ENTRIES {
        r.errors.push(format!(
            "{} entries, the limit is {MAX_INDEX_ENTRIES}",
            skills.len()
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, entry) in skills.iter().enumerate().take(MAX_INDEX_ENTRIES) {
        check_entry(i, entry, &mut seen, &mut r);
    }
    r
}

fn unknown_keys(map: &Map<String, Value>, known: &[&str], who: &str, r: &mut IndexReport) {
    for key in map.keys().filter(|k| !known.contains(&k.as_str())) {
        r.warnings.push(format!(
            "{who}: unknown field `{key}` is ignored (readers must ignore fields they do not know)"
        ));
    }
}

fn check_https(v: &Value, what: &str, errors: &mut Vec<String>) {
    match v.as_str() {
        Some(s)
            if s.starts_with("https://")
                && s.len() <= 200
                && !s.chars().any(char::is_whitespace) => {}
        _ => errors.push(format!(
            "{what} must be an https:// URL of at most 200 characters"
        )),
    }
}

#[allow(clippy::too_many_lines)]
fn check_entry(i: usize, entry: &Value, seen: &mut BTreeSet<String>, r: &mut IndexReport) {
    let Some(map) = entry.as_object() else {
        r.errors.push(format!("entry #{i}: must be an object"));
        return;
    };
    let label = map
        .get("name")
        .and_then(Value::as_str)
        .map_or_else(|| format!("entry #{i}"), |n| format!("`{n}`"));
    let mut err = |msg: String| r.errors.push(format!("{label}: {msg}"));

    match map.get("name").and_then(Value::as_str) {
        Some(n) => {
            if let Err(e) = validate_name(n) {
                err(e);
            }
            if !seen.insert(n.to_string()) {
                err("duplicate name".into());
            }
        }
        None => err("`name` is required".into()),
    }
    match map.get("description") {
        Some(Value::String(d)) if d.chars().count() > MAX_DESCRIPTION_LEN => {
            err(format!(
                "`description` is longer than {MAX_DESCRIPTION_LEN} characters"
            ));
        }
        Some(Value::String(d)) if !d.trim().is_empty() => {}
        Some(Value::String(_)) | None => {
            r.warnings.push(format!("{label}: has no description"));
        }
        Some(_) => err("`description` must be a string".into()),
    }
    match map.get("git").and_then(Value::as_str) {
        Some(g) if !g.to_ascii_lowercase().starts_with("https://") => {
            err("`git` must be an https:// URL".into());
        }
        Some(g) => {
            if validate_git_url(g).is_err() {
                err("`git` is not an acceptable clone URL".into());
            }
        }
        None => err("`git` is required".into()),
    }
    match map.get("commit").and_then(Value::as_str) {
        Some(c) => {
            if validate_commit_pin(c).is_err() {
                err("`commit` must be a full 40- or 64-character hexadecimal hash".into());
            }
        }
        None => err(
            "`commit` is required (a full commit hash; branches and tags are not allowed)".into(),
        ),
    }
    if let Some(s) = map.get("subdir") {
        match s.as_str() {
            Some(s) if validate_subdir(s).is_ok() => {}
            _ => err("`subdir` must be a relative path without `..`".into()),
        }
    }
    if let Some(h) = map.get("sha256") {
        match h.as_str() {
            Some(h) if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => err("`sha256` must be 64 hexadecimal characters".into()),
        }
    }
    if let Some(t) = map.get("tags") {
        match t.as_array() {
            Some(tags) if tags.len() <= MAX_TAGS => {
                for tag in tags {
                    let ok = tag.as_str().is_some_and(|s| {
                        !s.is_empty()
                            && s.len() <= MAX_TAG_LEN
                            && s.bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    });
                    if !ok {
                        err(format!(
                            "tags must be 1-{MAX_TAG_LEN} characters of a-z, 0-9 and `-`"
                        ));
                        break;
                    }
                }
            }
            _ => err(format!(
                "`tags` must be an array of at most {MAX_TAGS} strings"
            )),
        }
    }
    for (key, max) in [("author", 100), ("license", 64)] {
        if let Some(v) = map.get(key)
            && !v
                .as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= max)
        {
            err(format!(
                "`{key}` must be a non-empty string of at most {max} characters"
            ));
        }
    }
    if let Some(h) = map.get("homepage") {
        let mut errs = Vec::new();
        check_https(h, "`homepage`", &mut errs);
        for e in errs {
            err(e);
        }
    }
    if let Some(v) = map.get("min_praxi_version") {
        let ok = v.as_str().is_some_and(|s| {
            let parts: Vec<&str> = s.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        });
        if !ok {
            err("`min_praxi_version` must look like 0.4.0".into());
        }
    }
    unknown_keys(map, ENTRY_KEYS, &label, r);
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    fn entry(name: &str) -> String {
        format!(
            r#"{{"name":"{name}","description":"d","git":"https://example.com/r.git","commit":"{C}"}}"#
        )
    }
    fn doc(version: &str, entries: &[String]) -> Vec<u8> {
        format!(
            r#"{{"version":{version},"skills":[{}]}}"#,
            entries.join(",")
        )
        .into_bytes()
    }
    fn errors(bytes: &[u8]) -> String {
        validate_index(bytes).errors.join("\n")
    }

    #[test]
    fn valid_index_with_all_metadata_passes_and_unknown_fields_only_warn() {
        let full = format!(
            r#"{{"version":1,"future":1,"skills":[{{"name":"a","description":"d","git":"https://e.com/r.git","commit":"{C}","subdir":"x/y","sha256":"{}","tags":["web"],"author":"me","license":"MIT","homepage":"https://e.com","min_praxi_version":"0.4.0","extra":true}}]}}"#,
            "ab".repeat(32)
        );
        let r = validate_index(full.as_bytes());
        assert!(r.is_ok(), "{:?}", r.errors);
        assert_eq!(r.entries, 1);
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    }

    #[test]
    fn rejects_unknown_version_short_commit_duplicates_and_oversize() {
        assert!(errors(&doc("2", &[entry("a")])).contains("unsupported index version 2"));
        assert!(errors(br#"{"skills":[]}"#).contains("`version` is required"));
        let short = entry("a").replace(C, "abc123");
        assert!(errors(&doc("1", &[short])).contains("full 40- or 64-character"));
        let dup = errors(&doc("1", &[entry("a"), entry("a")]));
        assert!(dup.contains("duplicate name"), "{dup}");
        let big = vec![b' '; MAX_INDEX_BYTES + 1];
        assert!(errors(&big).contains("limit"));
        let many: Vec<String> = (0..=MAX_INDEX_ENTRIES)
            .map(|i| entry(&format!("s{i}")))
            .collect();
        assert!(errors(&doc("1", &many)).contains("entries, the limit"));
    }

    #[test]
    fn rejects_non_https_branch_pins_and_bad_names() {
        let ssh = entry("a").replace("https://example.com/r.git", "ssh://git@e.com/r.git");
        assert!(errors(&doc("1", &[ssh])).contains("https://"));
        let http = entry("a").replace("https://example.com", "http://example.com");
        assert!(!validate_index(&doc("1", &[http])).is_ok());
        let nocommit = entry("a").replace(&format!(r#","commit":"{C}""#), "");
        assert!(errors(&doc("1", &[nocommit])).contains("`commit` is required"));
        assert!(errors(&doc("1", &[entry("../x")])).contains("name"));
        assert!(!validate_index(b"[]").is_ok());
        assert!(!validate_index(b"not json").is_ok());
    }

    #[test]
    fn json_schema_lists_the_same_fields_as_the_validator() {
        let schema: Value =
            serde_json::from_str(include_str!("../../../../docs/skills-index.schema.json"))
                .unwrap();
        let keys = |v: &Value| -> Vec<String> {
            let mut k: Vec<String> = v["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            k.sort();
            k
        };
        let sorted = |l: &[&str]| {
            let mut k: Vec<String> = l.iter().map(|s| (*s).to_string()).collect();
            k.sort();
            k
        };
        assert_eq!(keys(&schema), sorted(TOP_LEVEL_KEYS));
        assert_eq!(keys(&schema["$defs"]["entry"]), sorted(ENTRY_KEYS));
        assert_eq!(
            schema["$defs"]["entry"]["required"],
            serde_json::json!(["name", "git", "commit"])
        );
    }
}

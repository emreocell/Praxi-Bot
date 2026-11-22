//! Secret scanning: find API-key-looking text in arbitrary strings.
//!
//! Two layers:
//! * the hand-tuned [`crate::redact::SHAPES`] the redactor always had, and
//! * the generated gitleaks rule table in [`crate::rules`] (MIT, see
//!   `THIRD_PARTY_NOTICES.md`), run through a keyword prefilter: one
//!   Aho-Corasick pass over the text decides which rules can possibly match,
//!   and each rule's regex is compiled lazily, the first time it is needed.
//!
//! A [`Finding`] holds a rule id, a description and a byte span, never the
//! matched text, so printing one cannot leak the value. Findings are meant for
//! *warning* ("this looks like an API key, keep it out of the chat?"), never
//! for blocking: false positives are possible.

use std::fmt;
use std::ops::Range;
use std::sync::OnceLock;

use aho_corasick::{AhoCorasick, MatchKind};
use regex::{Regex, RegexBuilder};

use crate::redact::SHAPES;
use crate::rules::RULES;

/// One converted gitleaks rule (see `rules.rs`, generated).
pub(crate) struct Rule {
    pub id: &'static str,
    pub description: &'static str,
    pub regex: &'static str,
    /// Lower-case literals; the rule only runs when the text contains one.
    /// Empty: always runs.
    pub keywords: &'static [&'static str],
    /// Minimum Shannon entropy (bits per byte) of the secret; 0 = no check.
    pub entropy: f32,
    /// Capture group holding the secret; 0 = first group, else whole match.
    pub secret_group: usize,
    /// A secret matching one of these is a known placeholder, not a finding.
    pub allow: &'static [&'static str],
}

/// A suspected secret: which rule saw it and where. Never the value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Finding {
    /// Stable rule id (`github-pat`, `aws-access-token`, `shape:jwt`, ...).
    pub rule_id: &'static str,
    /// English description of what the rule looks for. The UI localises by id.
    pub description: &'static str,
    /// Byte offset of the first byte of the secret (inclusive).
    pub start: usize,
    /// Byte offset one past the last byte of the secret (exclusive).
    pub end: usize,
}

impl Finding {
    /// The secret's byte range in the scanned text.
    #[must_use]
    pub fn span(&self) -> Range<usize> {
        self.start..self.end
    }
}

impl fmt::Debug for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Finding({} @{}..{})", self.rule_id, self.start, self.end)
    }
}

/// Compiled form of a [`Rule`], built on first use.
struct Compiled {
    re: Regex,
    allow: Vec<Regex>,
}

struct Engine {
    keywords: AhoCorasick,
    /// Aho-Corasick pattern index -> rule index.
    keyword_rule: Vec<usize>,
    /// Rules without keywords.
    always: Vec<usize>,
    compiled: Vec<OnceLock<Option<Compiled>>>,
    shapes: Vec<(&'static str, Regex)>,
}

fn build(pattern: &str) -> Option<Regex> {
    RegexBuilder::new(pattern).size_limit(32 << 20).build().ok()
}

fn engine() -> &'static Engine {
    static CELL: OnceLock<Engine> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut words: Vec<&str> = Vec::new();
        let mut keyword_rule = Vec::new();
        let mut always = Vec::new();
        for (i, r) in RULES.iter().enumerate() {
            if r.keywords.is_empty() {
                always.push(i);
            }
            for k in r.keywords {
                words.push(k);
                keyword_rule.push(i);
            }
        }
        let keywords = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::Standard)
            .build(&words)
            .expect("keyword table is valid");
        Engine {
            keywords,
            keyword_rule,
            always,
            compiled: RULES.iter().map(|_| OnceLock::new()).collect(),
            shapes: SHAPES
                .iter()
                .map(|(id, p)| (*id, Regex::new(p).expect("built-in pattern is valid")))
                .collect(),
        }
    })
}

fn compiled(e: &'static Engine, i: usize) -> Option<&'static Compiled> {
    e.compiled[i]
        .get_or_init(|| {
            let r = &RULES[i];
            // A rule whose regex this engine cannot build is skipped; the
            // `every_rule_compiles` test keeps that from happening unnoticed.
            let re = build(r.regex)?;
            let allow = r.allow.iter().filter_map(|a| build(a)).collect();
            Some(Compiled { re, allow })
        })
        .as_ref()
}

fn entropy(s: &str) -> f32 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[usize::from(b)] += 1;
    }
    let len = f64::from(u32::try_from(s.len()).unwrap_or(u32::MAX));
    let mut h = 0.0f64;
    for c in counts.iter().filter(|c| **c > 0) {
        let p = f64::from(*c) / len;
        h -= p * p.log2();
    }
    #[allow(clippy::cast_possible_truncation)]
    {
        h as f32
    }
}

/// Indexes of the rules whose keywords occur in `text` (plus the rules that
/// have none): the only ones worth running a regex for.
fn candidate_rules(text: &str) -> Vec<usize> {
    let e = engine();
    let mut wanted = vec![false; RULES.len()];
    for &i in &e.always {
        wanted[i] = true;
    }
    for m in e.keywords.find_overlapping_iter(text) {
        wanted[e.keyword_rule[m.pattern().as_usize()]] = true;
    }
    (0..RULES.len()).filter(|&i| wanted[i]).collect()
}

/// Find suspected secrets in `text`. Sorted by start; overlapping hits are
/// merged into one finding (so masking every span leaves no half secret).
///
/// Pure and local: no I/O, no network, no logging.
#[must_use]
pub fn scan_text(text: &str) -> Vec<Finding> {
    let e = engine();
    let mut out: Vec<Finding> = Vec::new();

    for (id, re) in &e.shapes {
        for m in re.find_iter(text) {
            out.push(Finding {
                rule_id: shape_id(id),
                description: "Looks like an API key or token with a well-known prefix.",
                start: m.start(),
                end: m.end(),
            });
        }
    }

    for i in candidate_rules(text) {
        let rule = &RULES[i];
        let Some(c) = compiled(e, i) else { continue };
        for caps in c.re.captures_iter(text) {
            let group = if rule.secret_group > 0 {
                rule.secret_group
            } else {
                usize::from(caps.len() > 1)
            };
            let Some(m) = caps.get(group).or_else(|| caps.get(0)) else {
                continue;
            };
            let secret = m.as_str();
            if rule.entropy > 0.0 && entropy(secret) < rule.entropy {
                continue;
            }
            if c.allow.iter().any(|a| a.is_match(secret)) {
                continue;
            }
            out.push(Finding {
                rule_id: rule.id,
                description: rule.description,
                start: m.start(),
                end: m.end(),
            });
        }
    }

    // Same extent: the specific gitleaks id beats the generic shape id.
    out.sort_by_key(|f| {
        (
            f.start,
            std::cmp::Reverse(f.end),
            f.rule_id.starts_with("shape:"),
        )
    });
    let mut merged: Vec<Finding> = Vec::with_capacity(out.len());
    for f in out {
        match merged.last_mut() {
            Some(last) if f.start < last.end => last.end = last.end.max(f.end),
            _ => merged.push(f),
        }
    }
    merged
}

/// Stable `shape:<id>` names without allocating.
fn shape_id(id: &str) -> &'static str {
    match id {
        "openai_style" => "shape:openai",
        "google_api" => "shape:google-api",
        "github" => "shape:github",
        "slack" => "shape:slack",
        "nvidia" => "shape:nvidia",
        _ => "shape:jwt",
    }
}

/// Replace every finding in `text` with `mask`.
#[must_use]
pub(crate) fn mask(text: &str, mask: &str) -> String {
    let findings = scan_text(text);
    if findings.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for f in findings {
        out.push_str(&text[at..f.start]);
        out.push_str(mask);
        at = f.end;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;

//! Leak detection: mask secrets in arbitrary text.

use regex::Regex;

use crate::secret::SecretStore;

/// Replacement text for masked secrets.
pub const REDACTED: &str = "[REDACTED]";

/// Values shorter than this are not registered (masking "abc" would wreck text).
const MIN_SECRET_LEN: usize = 6;

/// Always-on shapes of common credentials.
const PATTERNS: &[&str] = &[
    // OpenAI / Anthropic / OpenRouter style keys.
    r"sk-[A-Za-z0-9_-]{16,}",
    // Google API keys.
    r"AIza[0-9A-Za-z_-]{30,}",
    // GitHub tokens.
    r"gh[pousr]_[A-Za-z0-9]{30,}",
    r"github_pat_[A-Za-z0-9_]{30,}",
    // Slack tokens.
    r"xox[abprs]-[A-Za-z0-9-]{10,}",
    // NVIDIA keys.
    r"nvapi-[A-Za-z0-9_-]{20,}",
    // JSON Web Tokens.
    r"eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
];

/// `value` plus its JSON-escaped, URL-encoded and base64 (standard and
/// URL-safe, at each of the three byte alignments) renderings.
#[must_use]
pub fn secret_variants(value: &str) -> Vec<String> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    let mut out = vec![value.to_owned()];
    if let Ok(j) = serde_json::to_string(value) {
        out.push(j[1..j.len() - 1].to_owned());
    }
    let mut url = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            url.push(char::from(b));
        } else {
            {
                use std::fmt::Write as _;
                let _ = write!(url, "%{b:02X}");
            }
        }
    }
    out.push(url);
    // Base64 of a substring depends on its alignment in the encoded text; the
    // stable middle of each of the 3 alignments is what appears in practice.
    for k in 0..3usize {
        let mut bytes = vec![0u8; k];
        bytes.extend_from_slice(value.as_bytes());
        let lead = [0, 2, 3][k];
        let trail = usize::from(bytes.len() % 3 != 0);
        for enc in [
            STANDARD_NO_PAD.encode(&bytes),
            URL_SAFE_NO_PAD.encode(&bytes),
        ] {
            if enc.len() > lead + trail {
                out.push(enc[lead..enc.len() - trail].to_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Masks known secret values and common key shapes.
#[derive(Debug, Clone)]
pub struct Redactor {
    values: Vec<String>,
    patterns: Vec<Regex>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    /// A redactor with only the built-in patterns.
    ///
    /// # Panics
    /// Never in practice: the built-in patterns are valid regexes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            patterns: PATTERNS
                .iter()
                .map(|p| Regex::new(p).expect("built-in pattern is valid"))
                .collect(),
        }
    }

    /// Register a concrete secret value (ignored when very short).
    pub fn add_secret(&mut self, value: &str) {
        if value.len() >= MIN_SECRET_LEN && !self.values.iter().any(|v| v == value) {
            self.values.push(value.to_string());
            // Longest first so a secret containing another is masked whole.
            self.values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        }
    }

    /// Build a redactor from every value currently in `store`.
    ///
    /// # Errors
    /// Backend failures while reading the store.
    pub fn from_store(store: &dyn SecretStore) -> Result<Self, crate::SecretError> {
        let mut r = Self::new();
        for name in store.list_names()? {
            if let Some(v) = store.get(&name)? {
                r.add_secret(v.expose());
            }
        }
        Ok(r)
    }

    /// Mask all known values and patterns in `text`.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for v in &self.values {
            if out.contains(v.as_str()) {
                out = out.replace(v.as_str(), REDACTED);
            }
        }
        for re in &self.patterns {
            if re.is_match(&out) {
                out = re.replace_all(&out, REDACTED).into_owned();
            }
        }
        out
    }

    /// Whether `text` contains anything that would be masked.
    #[must_use]
    pub fn contains_secret(&self, text: &str) -> bool {
        self.values.iter().any(|v| text.contains(v.as_str()))
            || self.patterns.iter().any(|re| re.is_match(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    #[test]
    fn masks_registered_values() {
        let mut r = Redactor::new();
        r.add_secret("correct-horse-battery");
        let out = r.redact("token=correct-horse-battery; again correct-horse-battery");
        assert_eq!(out, format!("token={REDACTED}; again {REDACTED}"));
        assert!(!r.contains_secret(&out));
    }

    #[test]
    fn longest_value_first_and_short_ignored() {
        let mut r = Redactor::new();
        r.add_secret("abc"); // too short, ignored
        r.add_secret("secretvalue");
        r.add_secret("secretvalue-extended");
        assert_eq!(
            r.redact("abc secretvalue-extended"),
            format!("abc {REDACTED}")
        );
    }

    #[test]
    fn masks_known_shapes() {
        let r = Redactor::new();
        let cases = [
            "sk-abcdefghijklmnopqrstuvwxyz123456",
            "sk-or-v1-abcdefghijklmnopqrstuvwxyz",
            "AIzaSyA1234567890abcdefghijklmnopqrstuv",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "xoxb-1234567890-abcdefghij",
            "nvapi-abcdefghijklmnopqrstuvwxyz",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop",
        ];
        for c in cases {
            let out = r.redact(&format!("key: {c} end"));
            assert_eq!(out, format!("key: {REDACTED} end"), "case {c}");
        }
    }

    #[test]
    fn leaves_normal_text_alone() {
        let r = Redactor::new();
        let t = "The task-runner skipped sk-1 and asked: how do I tokenize 'ghp'? Visit https://example.com/a.b.c";
        assert_eq!(r.redact(t), t);
        assert!(!r.contains_secret(t));
    }

    #[test]
    fn from_store_collects_values() {
        let s = MemoryStore::new();
        s.set("a", "value-one-123").unwrap();
        s.set("b", "x").unwrap(); // too short
        let r = Redactor::from_store(&s).unwrap();
        assert_eq!(r.redact("v=value-one-123 x"), format!("v={REDACTED} x"));
    }
}

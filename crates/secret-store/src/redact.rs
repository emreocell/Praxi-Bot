//! Leak detection: mask secrets in arbitrary text.

use crate::scan;
use crate::secret::SecretStore;

/// Replacement text for masked secrets.
pub const REDACTED: &str = "[REDACTED]";

/// Values shorter than this are not registered (masking "abc" would wreck text).
const MIN_SECRET_LEN: usize = 6;

/// Always-on shapes of common credentials as `(id, regex)`. The ids are
/// stable names that [`crate::detect`] maps to kinds. [`crate::scan`] runs
/// them next to the generated gitleaks rule table (lazily compiled, behind a
/// keyword prefilter), so the redactor masks both.
pub(crate) const SHAPES: &[(&str, &str)] = &[
    // OpenAI / Anthropic / OpenRouter style keys.
    ("openai_style", r"sk-[A-Za-z0-9_-]{16,}"),
    // Google API keys.
    ("google_api", r"AIza[0-9A-Za-z_-]{30,}"),
    // GitHub tokens.
    ("github", r"gh[pousr]_[A-Za-z0-9]{30,}"),
    ("github", r"github_pat_[A-Za-z0-9_]{30,}"),
    // Slack tokens.
    ("slack", r"xox[abprs]-[A-Za-z0-9-]{10,}"),
    // NVIDIA keys.
    ("nvidia", r"nvapi-[A-Za-z0-9_-]{20,}"),
    // JSON Web Tokens.
    (
        "jwt",
        r"eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ),
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
    // Percent-encodings: RFC 3986 (`.` and `~` kept), strict (every
    // non-alphanumeric byte but `-` / `_` escaped, as older encoders do) and
    // HTML form (space as `+`), each with upper- and lower-case hex.
    for lower in [false, true] {
        for strict in [false, true] {
            out.push(percent_encode(value, strict, false, lower));
        }
        out.push(percent_encode(value, false, true, lower));
    }
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

/// Percent-encode `value`: unreserved bytes stay (`.` and `~` too unless
/// `strict`), a space becomes `+` when `form`, everything else is `%XX` with
/// upper- or `lower`-case hex.
fn percent_encode(value: &str, strict: bool, form: bool, lower: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_')
            || (!strict && matches!(b, b'.' | b'~'))
        {
            out.push(char::from(b));
        } else if form && b == b' ' {
            out.push('+');
        } else if lower {
            let _ = write!(out, "%{b:02x}");
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Masks known secret values and common key shapes.
#[derive(Debug, Clone)]
pub struct Redactor {
    values: Vec<String>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    /// A redactor with only the built-in patterns (compiled lazily, once per
    /// process, on first use).
    #[must_use]
    pub fn new() -> Self {
        Self { values: Vec::new() }
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
        scan::mask(&out, REDACTED)
    }

    /// Whether `text` contains anything that would be masked.
    #[must_use]
    pub fn contains_secret(&self, text: &str) -> bool {
        self.values.iter().any(|v| text.contains(v.as_str())) || !scan::scan_text(text).is_empty()
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
    fn masks_every_percent_encoding_of_a_short_or_dotted_secret() {
        let mut r = Redactor::new();
        for v in secret_variants("pass/wd")
            .into_iter()
            .chain(secret_variants("my.pass/word~1 x"))
        {
            r.add_secret(&v);
        }
        for text in [
            "pass%2Fwd",
            "pass%2fwd",
            "my.pass%2Fword~1%20x",
            "my.pass%2fword~1%20x",
            "my%2Epass%2Fword%7E1%20x",
            "my%2epass%2fword%7e1%20x",
            "my.pass%2fword~1+x",
        ] {
            let out = r.redact(&format!("at https://h/{text}?q end"));
            assert_eq!(out, format!("at https://h/{REDACTED}?q end"), "{text}");
        }
    }

    #[test]
    fn plain_secrets_add_no_extra_variants() {
        // Only base64 alignments and the value itself for an alphanumeric
        // secret: the extra encodings collapse into the original.
        let v = secret_variants("abcdef123456");
        assert_eq!(v.iter().filter(|x| x.contains('%')).count(), 0);
        assert!(v.len() <= 7, "{v:?}");
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

    #[test]
    fn percent_encode_respects_strict_form_and_hex_case_flags() {
        let cases = [
            ("", false, false, false, ""),
            ("", true, true, true, ""),
            ("a.b~c-d_e", false, false, false, "a.b~c-d_e"),
            ("a.b~c-d_e", true, false, false, "a%2Eb%7Ec-d_e"),
            ("a.b~c-d_e", true, false, true, "a%2eb%7ec-d_e"),
            ("a b/c", false, false, false, "a%20b%2Fc"),
            ("a b/c", false, true, false, "a+b%2Fc"),
            ("a b/c", false, true, true, "a+b%2fc"),
        ];
        for (input, strict, form, lower, expected) in cases {
            assert_eq!(
                percent_encode(input, strict, form, lower),
                expected,
                "input={input:?} strict={strict} form={form} lower={lower}"
            );
        }
    }

    #[test]
    fn percent_encode_handles_unicode_and_long_inputs() {
        assert_eq!(
            percent_encode("ü🔑", false, false, false),
            "%C3%BC%F0%9F%94%91"
        );
        assert_eq!(
            percent_encode("ü🔑", false, false, true),
            "%c3%bc%f0%9f%94%91"
        );
        let long_raw = "a/b ".repeat(150);
        let encoded = percent_encode(&long_raw, false, false, false);
        assert_eq!(encoded, "a%2Fb%20".repeat(150));
    }
}

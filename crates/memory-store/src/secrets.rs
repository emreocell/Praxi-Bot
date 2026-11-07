//! Secret detection / redaction (memories must never contain credentials).

use std::sync::OnceLock;

use regex::Regex;

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // Provider-style API keys and tokens.
            r"\bsk-[A-Za-z0-9_\-]{16,}",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}",
            r"\bgithub_pat_[A-Za-z0-9_]{20,}",
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\bxox[abprs]-[A-Za-z0-9\-]{10,}",
            r"\bAIza[0-9A-Za-z_\-]{30,}",
            // JWT.
            r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
            // PEM private keys.
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            // Authorization headers.
            r"(?i)\bbearer\s+[A-Za-z0-9._\-]{16,}",
            // "password: hunter2", "api_key = abc", "secret=..."
            r"(?i)\b(pass(word|wd)?|passcode|pwd|secret|api[_\- ]?key|token|pin)\b\s*(is|=|:)\s*\S{3,}",
            // Card-like digit groups.
            r"\b(?:\d[ \-]?){13,19}\b",
            // Long opaque tokens (hex / base64-ish, 32+).
            r"\b[A-Fa-f0-9]{32,}\b",
            r"\b[A-Za-z0-9+/_\-]{40,}={0,2}",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("static regex"))
        .collect()
    })
}

/// True if `text` looks like it contains a credential.
#[must_use]
pub fn contains_secret(text: &str) -> bool {
    patterns().iter().any(|re| re.is_match(text))
}

/// Replace every secret-looking span with `[REDACTED]`.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for re in patterns() {
        out = re.replace_all(&out, "[REDACTED]").into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_secrets() {
        for s in [
            "my key is sk-abcdefghijklmnop1234",
            "password: hunter2",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "AKIAABCDEFGHIJKLMNOP",
            "card 4111 1111 1111 1111",
            "-----BEGIN RSA PRIVATE KEY-----",
        ] {
            assert!(contains_secret(s), "{s}");
        }
    }

    #[test]
    fn ignores_plain_facts() {
        for s in [
            "The user lives in Berlin",
            "Prefers dark mode and short answers",
            "Birthday is on 12 May",
        ] {
            assert!(!contains_secret(s), "{s}");
        }
    }

    #[test]
    fn redacts() {
        let r = redact("use sk-abcdefghijklmnop1234 please");
        assert!(r.contains("[REDACTED]") && !r.contains("sk-abc"));
    }
}

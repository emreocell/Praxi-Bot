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
            r"\bglpat-[A-Za-z0-9_\-]{20,}",
            r"\bnpm_[A-Za-z0-9]{36}\b",
            r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}",
            r"\bya29\.[A-Za-z0-9_\-]{20,}",
            // AWS: long-term and temporary access key ids, and the key/value
            // lines of `~/.aws/credentials` (`aws_secret_access_key = ...`, whose
            // name has no word boundary around "secret").
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\bASIA[0-9A-Z]{16}\b",
            r"(?i)\baws_(?:secret_access_key|session_token)\s*[=:]\s*\S{16,}",
            // Azure storage and service-bus connection strings.
            r"(?i)\b(?:account|shared ?access)key\s*=\s*[A-Za-z0-9+/]{30,}={0,2}",
            r"\bxox[abprs]-[A-Za-z0-9\-]{10,}",
            r"\bAIza[0-9A-Za-z_\-]{30,}",
            // A user name and password inside a URL (`postgres://user:pw@host/db`).
            r"\b[A-Za-z][A-Za-z0-9+.\-]*://[^\s:/@]+:[^\s/@]{3,}@",
            // age secret keys.
            r"\bAGE-SECRET-KEY-1[0-9A-Z]{50,}",
            // JWT.
            r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
            // PEM private keys (PGP ones end in "KEY BLOCK-----").
            r"-----BEGIN [A-Z ]*PRIVATE KEY(?: BLOCK)?-----",
            // Authorization headers.
            r"(?i)\bbearer\s+[A-Za-z0-9._\-]{16,}",
            // "password: hunter2", "api_key = abc", "secret=..."
            r"(?i)\b(pass(word|wd)?|passcode|pwd|secret|api[_\- ]?key|token|pin)\b\s*(is|=|:)\s*\S{3,}",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("static regex"))
        .collect()
    })
}

/// Card-like digit groups. Weak on its own (order numbers, timestamps and
/// barcodes look the same), so [`credential_hits`] does not count them.
fn card_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(?:\d[ \-]?){13,19}\b").expect("static regex"))
}

/// Long opaque tokens (hex / base64-ish, 32+).  Unlike the patterns above, a
/// bare hex run of a hash length is let through when the words before it say
/// it is an identifier (see [`is_identifier_hash`]).
fn opaque_patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [r"\b[A-Fa-f0-9]{32,}\b", r"\b[A-Za-z0-9+/_\-]{40,}={0,2}"]
            .iter()
            .map(|p| Regex::new(p).expect("static regex"))
            .collect()
    })
}

/// Words that name a hash/commit/checksum right before a run of hex digits.
fn identifier_context() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"(?i)(\bcommit|\bsha-?(1|256|512)?|\bmd5|\bchecksum|\bhash|\bdigest|\brevision|\brev|\boid|\bcherry-picked|\bblob|\bgit\s+(checkout|show|revert|cherry-pick|reset|merge|rebase|diff|log))[\s:=#'"`(\[\-]*$"#,
        )
        .expect("static regex")
    })
}

/// Words that make a nearby hex run a credential after all.
fn credential_context() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)secret|password|passwd|passcode|token|api[_\- ]?key|private|credential|bearer",
        )
        .expect("static regex")
    })
}

/// Last (up to) `n` characters of `text`.
fn tail(text: &str, n: usize) -> &str {
    let start = text
        .char_indices()
        .rev()
        .nth(n.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    &text[start..]
}

/// A bare hex run of exactly a hash length (MD5 32, SHA-1 40, SHA-256 64) that
/// follows a word such as "commit", "sha256" or "checksum" is an identifier,
/// not a credential.  Anything else (other lengths, base64-looking text, no
/// naming word, or a credential word nearby) stays a secret.
fn is_identifier_hash(text: &str, start: usize, end: usize) -> bool {
    let run = &text[start..end];
    if !matches!(run.len(), 32 | 40 | 64) || !run.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let before = tail(&text[..start], 48);
    let after: String = text[end..].chars().take(48).collect();
    identifier_context().is_match(before)
        && !credential_context().is_match(before)
        && !credential_context().is_match(&after)
}

/// Spans of `text` matched by an opaque-token pattern and not exempt.
fn opaque_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = opaque_patterns()
        .iter()
        .flat_map(|re| re.find_iter(text).map(|m| (m.start(), m.end())))
        .filter(|&(s, e)| !is_identifier_hash(text, s, e))
        .collect();
    out.sort_unstable();
    out
}

/// True if `text` looks like it contains a credential.
#[must_use]
pub fn contains_secret(text: &str) -> bool {
    patterns().iter().any(|re| re.is_match(text))
        || card_pattern().is_match(text)
        || !opaque_spans(text).is_empty()
}

/// How many credential-shaped spans `text` holds, counting only the strong
/// patterns (provider keys, tokens, `password: ...` lines, URL credentials,
/// private-key headers). Card-like digit groups and long opaque runs are left
/// out: they are common in ordinary spreadsheets and URLs. A document with many
/// hits is a credentials dump, not prose that mentions a password.
#[must_use]
pub fn credential_hits(text: &str) -> usize {
    patterns().iter().map(|re| re.find_iter(text).count()).sum()
}

/// Replace every secret-looking span with `[REDACTED]`.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for re in patterns() {
        out = re.replace_all(&out, "[REDACTED]").into_owned();
    }
    out = card_pattern().replace_all(&out, "[REDACTED]").into_owned();
    // Opaque tokens last, on the text the other patterns already cleaned;
    // spans are replaced back to front so earlier offsets stay valid.
    let mut last_start = usize::MAX;
    for (s, e) in opaque_spans(&out).into_iter().rev() {
        if e > last_start {
            continue; // overlaps a span already replaced
        }
        out.replace_range(s..e, "[REDACTED]");
        last_start = s;
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
    fn detects_more_cloud_and_connection_credentials() {
        for s in [
            "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "AWS_SESSION_TOKEN: IQoJb3JpZ2luX2VjEXAMPLEEXAMPLEEXAMPLE",
            "temporary id ASIAIOSFODNN7EXAMPLE",
            "DefaultEndpointsProtocol=https;AccountName=x;AccountKey=Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A=",
            "DATABASE_URL=postgres://app:hunter22@db.example.com/prod",
            "glpat-abcdefghijklmnopqrstu",
            "sk_live_abcdefghijklmnop1234",
            "npm_abcdefghijklmnopqrstuvwxyz0123456789",
            "-----BEGIN PGP PRIVATE KEY BLOCK-----",
            "AGE-SECRET-KEY-1QQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQ",
        ] {
            assert!(contains_secret(s), "{s}");
            assert!(redact(s).contains("[REDACTED]"), "{s}");
        }
        for s in [
            "see https://example.com/docs/page",
            "mail me at a@example.com",
        ] {
            assert!(!contains_secret(s), "{s}");
        }
    }

    #[test]
    fn credential_hits_ignore_card_like_digits_and_long_slugs() {
        let csv = "1700000000000,a\n1700000000001,b\n".repeat(30);
        assert_eq!(credential_hits(&csv), 0);
        assert!(contains_secret(&csv), "still redacted line by line");
        let dump = "password: hunter22\nAKIAABCDEFGHIJKLMNOP\nsk-abcdefghijklmnop1234\n";
        assert_eq!(credential_hits(dump), 3);
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
    fn hashes_named_as_identifiers_are_not_secrets() {
        for s in [
            "The fix landed in commit 8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b.",
            "git checkout 8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b",
            "sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "md5 checksum d41d8cd98f00b204e9800998ecf8427e",
            "revision=8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b",
        ] {
            assert!(!contains_secret(s), "{s}");
            assert_eq!(redact(s), s, "{s}");
        }
    }

    #[test]
    fn bare_or_disguised_opaque_runs_stay_secrets() {
        for s in [
            // No naming word: could be anything, including an API key.
            "8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b",
            "use d41d8cd98f00b204e9800998ecf8427e for login",
            // Naming word, but not a hash length.
            "commit 8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b11",
            // A credential word nearby wins over the naming word.
            "secret hash 8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b",
            "sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 api key",
            "api key sha256 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            // Base64-looking 40+ run, even after a naming word.
            "hash Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A=",
            // Real key patterns are never exempt.
            "commit sk-abcdefghijklmnop1234",
            "checksum ghp_abcdefghijklmnopqrstuvwxyz0123",
        ] {
            assert!(contains_secret(s), "{s}");
        }
        assert!(
            redact("commit 8e51eaf1b2c3d4e5f60718293a4b5c6d7e8f9a0b11 and sk-abcdefghijklmnop1234")
                .matches("[REDACTED]")
                .count()
                >= 2
        );
    }

    #[test]
    fn redacts() {
        let r = redact("use sk-abcdefghijklmnop1234 please");
        assert!(r.contains("[REDACTED]") && !r.contains("sk-abc"));
    }
}

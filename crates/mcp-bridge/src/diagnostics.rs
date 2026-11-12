//! Bounded, secret-scrubbed diagnostics for untrusted MCP transports.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::McpError;

pub(crate) const STDERR_LINE_BYTES: usize = 8 * 1024;
const STDERR_TAIL_LINES: usize = 200;
pub(crate) const STDERR_RETAINED_BYTES: usize = 128 * 1024;
pub(crate) const STDERR_SNAPSHOT_BYTES: usize = 64 * 1024;
const TRUNCATED: &str = " [truncated]";
const REDACTED: &str = "[REDACTED]";

/// A live, shared secret masker supplied by the host (the daemon passes its
/// `SharedRedactor::redact`): every secret in the store, the built-in key
/// shapes and every encoded variant, refreshed while the daemon runs.
pub type ExternalRedactor = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Secrets of one connection, kept after rotation, masked through the
/// project's `secret_store::Redactor` (with `secret_variants`: JSON-escaped,
/// URL-encoded, base64) and then through the host's live redactor.
/// Deliberately has no `Debug` implementation exposing the patterns.
#[derive(Clone, Default)]
pub(crate) struct Redactor {
    local: Arc<Local>,
    external: Option<ExternalRedactor>,
}

#[derive(Default)]
struct Local {
    inner: secret_store::Redactor,
    /// Every registered rendering, to cut a retained prefix that ends in the
    /// middle of a secret.
    renderings: Vec<String>,
}

/// Shortest cut-off secret prefix that is masked at a truncated line end.
const MIN_PREFIX_BYTES: usize = 4;

/// The value, each of its lines, and the credential part of a header value
/// (`Bearer <token>` / `Basic <base64>`, including the decoded user and
/// password), so a server that logs only the token is still masked.
fn secret_parts(value: &str) -> Vec<String> {
    use base64::Engine as _;
    let mut parts = vec![value.to_owned()];
    parts.extend(value.lines().map(str::to_owned));
    if let Some((scheme, credential)) = value.split_once(' ') {
        let credential = credential.trim();
        parts.push(credential.to_owned());
        if scheme.eq_ignore_ascii_case("basic")
            && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(credential)
            && let Ok(decoded) = String::from_utf8(decoded)
        {
            parts.extend(decoded.split(':').map(str::to_owned));
            parts.push(decoded);
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

impl Redactor {
    pub(crate) fn new<'a>(
        values: impl IntoIterator<Item = &'a str>,
        external: Option<ExternalRedactor>,
    ) -> Self {
        let mut local = Local::default();
        for value in values {
            for part in secret_parts(value) {
                // `Debug` output of dependency errors escapes differently from JSON.
                let debug = format!("{part:?}");
                let debug = debug[1..debug.len() - 1].to_owned();
                for variant in secret_store::secret_variants(&part)
                    .into_iter()
                    .chain(std::iter::once(debug))
                {
                    local.inner.add_secret(&variant);
                    local.renderings.push(variant);
                }
            }
        }
        local.renderings.retain(|r| r.len() >= MIN_PREFIX_BYTES);
        local.renderings.sort_unstable();
        local.renderings.dedup();
        Self {
            local: Arc::new(local),
            external,
        }
    }

    fn redact(&self, text: &str) -> String {
        let masked = self.local.inner.redact(text);
        match &self.external {
            Some(external) => external(&masked),
            None => masked,
        }
    }

    pub(crate) fn scrub(&self, text: &str) -> String {
        self.redact(text)
    }

    /// Mask a line that is at most [`STDERR_LINE_BYTES`] of raw input. When
    /// `truncated`, a secret cut by the line limit is masked from its start.
    fn scrub_line(&self, text: &str, mut truncated: bool) -> String {
        let (head, partial) = if truncated {
            self.split_cut_secret(text)
        } else {
            (text, false)
        };
        let mut out = self.redact(head);
        if partial {
            out.push_str(REDACTED);
        }
        if out.len() > STDERR_LINE_BYTES - TRUNCATED.len() {
            let mut end = STDERR_LINE_BYTES - TRUNCATED.len();
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            truncated = true;
        }
        if truncated {
            out.push_str(TRUNCATED);
        }
        out
    }

    /// Split off a trailing fragment that is a proper prefix of a secret.
    fn split_cut_secret<'t>(&self, text: &'t str) -> (&'t str, bool) {
        let longest = self.local.renderings.iter().map(String::len).max();
        let Some(longest) = longest else {
            return (text, false);
        };
        let from = text.len().saturating_sub(longest);
        for (offset, _) in text[from..].char_indices() {
            let rest = &text[from + offset..];
            if rest.len() >= MIN_PREFIX_BYTES
                && self.local.renderings.iter().any(|r| r.starts_with(rest))
            {
                return (&text[..from + offset], true);
            }
        }
        (text, false)
    }

    pub(crate) fn error(&self, error: McpError) -> McpError {
        match error {
            McpError::Config(message) => McpError::Config(self.scrub(&message)),
            McpError::Store(message) => McpError::Store(self.scrub(&message)),
            McpError::Spawn { server, message } => McpError::Spawn {
                server,
                message: self.scrub(&message),
            },
            McpError::Transport { server, message } => McpError::Transport {
                server,
                message: self.scrub(&message),
            },
            other => other,
        }
    }
}

#[derive(Debug, Default)]
struct LogBuffer {
    lines: VecDeque<String>,
    bytes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct StderrLog(Mutex<LogBuffer>);

impl StderrLog {
    fn push(&self, line: String) {
        // The only producer is the bounded redactor below.
        debug_assert!(line.len() <= STDERR_LINE_BYTES);
        let mut log = self.0.lock();
        while log.lines.len() >= STDERR_TAIL_LINES || log.bytes + line.len() > STDERR_RETAINED_BYTES
        {
            if let Some(old) = log.lines.pop_front() {
                log.bytes -= old.len();
            } else {
                return;
            }
        }
        log.bytes += line.len();
        log.lines.push_back(line);
    }

    pub(crate) fn snapshot(&self) -> Vec<String> {
        let log = self.0.lock();
        let mut bytes = 0;
        let mut lines: Vec<_> = log
            .lines
            .iter()
            .rev()
            .take_while(|line| {
                bytes += line.len();
                bytes <= STDERR_SNAPSHOT_BYTES
            })
            .cloned()
            .collect();
        lines.reverse();
        lines
    }
}

/// Read fixed chunks, retaining one bounded prefix and discarding the rest of
/// an oversized line. A newline-free stream never grows an unbounded buffer.
pub(crate) async fn capture_stderr(
    server: &str,
    mut reader: impl AsyncRead + Unpin,
    log: &StderrLog,
    redactor: &Redactor,
) {
    let mut chunk = [0u8; 4096];
    let mut line = Vec::with_capacity(STDERR_LINE_BYTES);
    let mut discarding = false;
    let mut read_failed = false;
    let emit = |line: &[u8], truncated| {
        // Truncating a UTF-8 codepoint must not insert a replacement character
        // into a secret prefix before matching. Malformed bytes discard the
        // rest of the line, preserving only its valid, scrubbed prefix.
        let (text, truncated) = match std::str::from_utf8(line) {
            Ok(text) => (text, truncated),
            Err(error) => (
                std::str::from_utf8(&line[..error.valid_up_to()]).expect("valid UTF-8 prefix"),
                true,
            ),
        };
        let line = redactor.scrub_line(text, truncated);
        tracing::warn!(target: "mcp_bridge::stderr", server, "{line}");
        log.push(line);
    };
    loop {
        let Ok(count) = reader.read(&mut chunk).await else {
            read_failed = true;
            break;
        };
        if count == 0 {
            break;
        }
        for part in chunk[..count].split_inclusive(|byte| *byte == b'\n') {
            let newline = part.last() == Some(&b'\n');
            let content = if newline {
                &part[..part.len() - 1]
            } else {
                part
            };
            if !discarding {
                let take = content.len().min(STDERR_LINE_BYTES - line.len());
                line.extend_from_slice(&content[..take]);
                if take < content.len() {
                    emit(&line, true);
                    line.clear();
                    discarding = true;
                } else if newline {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    emit(&line, false);
                    line.clear();
                }
            }
            if newline {
                discarding = false;
            }
        }
    }
    if !line.is_empty() {
        emit(&line, read_failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain<'a>(values: impl IntoIterator<Item = &'a str>) -> Redactor {
        Redactor::new(values, None)
    }

    #[test]
    fn redacts_overlapping_multiline_and_cutoff_secrets_without_unbounded_expansion() {
        let redactor = plain([
            "token-aa",
            "token-aa-long",
            "two-aaaa\nlines-bb",
            "秘密秘密",
            "",
        ]);
        assert_eq!(
            redactor.scrub("token-aa-long token-aa two-aaaa lines-bb 秘密秘密"),
            "[REDACTED] [REDACTED] [REDACTED] [REDACTED] [REDACTED]"
        );
        assert_eq!(
            redactor.scrub_line("prefix token-", true),
            "prefix [REDACTED] [truncated]"
        );
        let redacted = plain(["xxxxxx"]).scrub_line(&"x".repeat(STDERR_LINE_BYTES), false);
        assert!(redacted.len() <= STDERR_LINE_BYTES);
        assert!(redacted.ends_with(TRUNCATED));
        assert!(!redacted.contains('x'));
    }

    #[test]
    fn redacts_secrets_escaped_by_json_error_data_and_debug() {
        let secret = "quoted\"credential\\with\ncontrol\u{0000}\t秘密";
        let redactor = plain([secret]);
        let error = rmcp::ErrorData::internal_error(
            "remote failure",
            Some(serde_json::json!({"credential": secret})),
        );
        let sanitized = redactor
            .error(McpError::Transport {
                server: "errors".into(),
                message: error.to_string(),
            })
            .to_string();
        assert!(sanitized.contains("remote failure"));
        assert!(sanitized.contains(REDACTED));
        assert!(!sanitized.contains("quoted"));
        assert_eq!(redactor.scrub(&format!("{secret:?}")), "\"[REDACTED]\"");
    }

    #[test]
    fn encoded_forms_and_header_credential_parts_are_masked() {
        use base64::Engine as _;
        use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
        let token = "tok/en+value?with=sym&bols-1234";
        let basic = format!("Basic {}", STANDARD.encode("alice:pa55-w0rd-secret"));
        let bearer = format!("Bearer {token}");
        let redactor = plain([bearer.as_str(), basic.as_str()]);
        let forms = [
            token.to_owned(),
            "tok%2Fen%2Bvalue%3Fwith%3Dsym%26bols-1234".to_owned(),
            STANDARD.encode(token),
            URL_SAFE_NO_PAD.encode(token),
            serde_json::to_string(token).unwrap(),
            bearer.clone(),
            basic.clone(),
            STANDARD.encode("alice:pa55-w0rd-secret"),
            "pa55-w0rd-secret".to_owned(),
        ];
        for form in forms {
            let out = redactor.scrub(&format!("log: {form} end"));
            assert!(out.contains(REDACTED), "not masked: {form}");
            assert!(out.ends_with(" end"));
            assert!(
                !out.contains("w0rd") && !out.contains("sym") && !out.contains("bols"),
                "leaked: {out}"
            );
        }
        // Not a parallel mechanism: the live host redactor runs on top.
        let external: ExternalRedactor = Arc::new(|text| text.replace("host-secret-9", "<H>"));
        let layered = Redactor::new([token], Some(external));
        assert_eq!(
            layered.scrub(&format!("{token} host-secret-9")),
            "[REDACTED] <H>"
        );
    }

    #[test]
    fn ring_and_snapshot_have_independent_byte_caps_and_preserve_order() {
        let log = StderrLog::default();
        for i in 0..210 {
            log.push(format!("l{i}"));
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 200);
        assert_eq!(snapshot[0], "l10");
        for i in 0..200 {
            log.push(format!("{i:03}{}", "a".repeat(STDERR_LINE_BYTES - 3)));
        }
        let retained = log.0.lock();
        assert!(retained.bytes <= STDERR_RETAINED_BYTES);
        assert_eq!(
            retained.bytes,
            retained.lines.iter().map(String::len).sum::<usize>()
        );
        drop(retained);
        let snapshot = log.snapshot();
        assert!(snapshot.iter().map(String::len).sum::<usize>() <= STDERR_SNAPSHOT_BYTES);
        assert!(snapshot.last().unwrap().starts_with("199"));
    }

    #[tokio::test]
    async fn newline_free_ten_megabyte_stream_is_bounded_and_recovers_at_newline() {
        let log = StderrLog::default();
        // AsyncRead::take synthesizes the flood, without allocating it in the test.
        let reader = tokio::io::repeat(b'x').take(10 * 1024 * 1024);
        capture_stderr("flood", reader, &log, &Redactor::default()).await;
        let lines = log.snapshot();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].len() <= STDERR_LINE_BYTES);
        assert!(lines[0].ends_with(TRUNCATED));
        let mut input = vec![b'x'; STDERR_LINE_BYTES + 20];
        input.extend_from_slice("\n正常\n".as_bytes());
        capture_stderr("flood", input.as_slice(), &log, &Redactor::default()).await;
        assert_eq!(log.snapshot().last().unwrap(), "正常");
    }

    #[tokio::test]
    async fn unicode_secret_cut_mid_codepoint_and_read_failure_do_not_leak_prefixes() {
        struct ReadError;
        impl AsyncRead for ReadError {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("read interrupted")))
            }
        }
        let log = StderrLog::default();
        let secret = "long-sensitive-prefix-clé秘密-sentinel";
        let cut = "long-sensitive-prefix-clé".len() + 1;
        let mut input = vec![b'.'; STDERR_LINE_BYTES - cut];
        input.extend_from_slice(secret.as_bytes());
        input.push(b'\n');
        capture_stderr("unicode", input.as_slice(), &log, &plain([secret])).await;
        let line = log.snapshot().pop().unwrap();
        assert!(!line.contains("clé"));
        assert!(!line.contains('\u{fffd}'));
        assert!(line.contains(REDACTED));
        assert!(line.ends_with(TRUNCATED));

        let input = "message: long-sensitive-prefix-clé秘"
            .as_bytes()
            .chain(ReadError);
        capture_stderr("read-error", input, &log, &plain([secret])).await;
        assert_eq!(
            log.snapshot().last().unwrap(),
            "message: [REDACTED] [truncated]"
        );
    }

    #[tokio::test]
    async fn secret_split_across_chunks_and_retained_prefix_is_scrubbed() {
        let log = StderrLog::default();
        let secret = "sentinel-secret-value-tail-123456789";
        for boundary in [4096, STDERR_LINE_BYTES] {
            let mut input = vec![b'.'; boundary - 26];
            input.extend_from_slice(secret.as_bytes());
            input.push(b'\n');
            capture_stderr("secret", input.as_slice(), &log, &plain([secret])).await;
        }
        for line in log.snapshot() {
            assert!(!line.contains("senti"));
            assert!(line.contains(REDACTED));
        }
    }
}

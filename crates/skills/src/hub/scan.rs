//! Static safety scan of a skill tree before installation.
//!
//! Heuristic and conservative: it never executes anything, it only reads the
//! files and reports what looks risky. The result is a [`RiskReport`] the
//! user must accept (see [`RiskReport::requires_acceptance`]) before the hub
//! installs the skill. A clean scan is not a guarantee of safety.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::HubError;
use super::util::{self, TreeFile, io_err};

/// Largest single file that is not flagged as oversized.
pub const MAX_SCAN_FILE_BYTES: u64 = 256 * 1024;

/// How worrying a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing (for example a URL in the text).
    Info,
    /// Needs the user's attention.
    Warn,
    /// Strong sign of malicious or dangerous behaviour.
    High,
}

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Shell/script usage or script and executable files.
    Script,
    /// Network access.
    Network,
    /// Credential patterns or credential-store access.
    Credential,
    /// Encoded or hidden content.
    Obfuscation,
    /// Text that tries to steer the agent against the user.
    Injection,
    /// Files that are unusually large.
    Oversized,
}

/// Overall risk of a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Nothing noteworthy.
    Low,
    /// Warnings only.
    Medium,
    /// At least one high-severity finding.
    High,
}

/// One thing the scan found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Category.
    pub kind: FindingKind,
    /// Severity.
    pub severity: Severity,
    /// File, relative to the skill root.
    pub file: String,
    /// 1-based line, when the finding is tied to a line.
    pub line: Option<usize>,
    /// Human-readable explanation (never contains the matched secret).
    pub message: String,
}

/// Result of scanning a skill tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskReport {
    /// Findings, ordered by file then line.
    pub findings: Vec<Finding>,
    /// Number of files scanned.
    pub files: usize,
    /// Total size in bytes.
    pub total_bytes: u64,
    /// SHA-256 over all file paths and contents. Acceptance is bound to this.
    pub fingerprint: String,
    /// True when the tree contains a script, native binary, or other active
    /// file type. Those files are not safe merely because no warning or
    /// high-severity rule matched their text.
    #[serde(default)]
    pub active: bool,
}

impl RiskReport {
    /// Highest severity found, mapped to a level.
    #[must_use]
    pub fn risk(&self) -> RiskLevel {
        match self.findings.iter().map(|f| f.severity).max() {
            Some(Severity::High) => RiskLevel::High,
            Some(Severity::Warn) => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    /// True when the user has to accept the report (by passing
    /// [`RiskReport::fingerprint`]) to install.
    ///
    /// Acceptance is required when any finding is a warning or worse, and also
    /// when the tree contains an active file type. A script or similar file is
    /// not treated as safe just because its text produced no warning.
    #[must_use]
    pub fn requires_acceptance(&self) -> bool {
        self.active || self.risk() != RiskLevel::Low
    }

    /// Findings of at least `min` severity.
    pub fn at_least(&self, min: Severity) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.severity >= min)
    }

    /// Plain-text rendering for a CLI or an approval dialog.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!(
            "risk: {:?} ({} files, {} bytes, fingerprint {})\n",
            self.risk(),
            self.files,
            self.total_bytes,
            self.fingerprint
        );
        for f in &self.findings {
            let loc = f.line.map_or(String::new(), |l| format!(":{l}"));
            let _ = writeln!(
                s,
                "  [{:?}/{:?}] {}{loc}: {}",
                f.severity, f.kind, f.file, f.message
            );
        }
        s
    }
}

const SCRIPT_EXT: &[&str] = &[
    "sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd", "py", "js", "mjs", "cjs", "ts", "rb",
    "pl", "php", "lua", "vbs", "jar", "wasm", "hta", "wsf", "jse", "vbe", "url", "scf", "lnk",
];
const NATIVE_EXT: &[&str] = &[
    "exe", "dll", "so", "dylib", "msi", "scr", "com", "bin", "node",
];
const INERT_BINARY_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "ico"];

/// Minimum length of an encoded run before it is decoded and scanned.
const MIN_ENCODED_RUN: usize = 16;
/// Largest decoded payload retained from one run.
const MAX_DECODED_BYTES: usize = 4096;
/// Most decoded payloads scanned in one file.
const MAX_DECODED_BLOBS: usize = 32;

use FindingKind::{Credential, Injection, Network, Obfuscation, Script};
use Severity::{High, Info, Warn};

type Rule = (&'static str, FindingKind, Severity, &'static str);

const PIPE_SHELL: &str = "pipes data into a shell";
const HIDE: &str = "asks the agent to hide actions from the user";
const IGNORE: &str = "tells the agent to ignore its instructions";

/// Substring rules matched against the lowercased line.
const RULES: &[Rule] = &[
    // Pipe to shell: download-and-run.
    ("| sh", Script, High, PIPE_SHELL),
    ("|sh", Script, High, PIPE_SHELL),
    ("| bash", Script, High, PIPE_SHELL),
    ("|bash", Script, High, PIPE_SHELL),
    ("| iex", Script, High, "pipes data into Invoke-Expression"),
    ("|iex", Script, High, "pipes data into Invoke-Expression"),
    ("invoke-expression", Script, High, "uses Invoke-Expression"),
    // Shell / process usage.
    ("sudo ", Script, Warn, "uses sudo"),
    ("rm -rf", Script, Warn, "recursive forced delete"),
    ("del /f", Script, Warn, "forced delete"),
    ("chmod +x", Script, Warn, "makes files executable"),
    ("bash -c", Script, Warn, "runs a shell command string"),
    ("sh -c", Script, Warn, "runs a shell command string"),
    ("cmd /c", Script, Warn, "runs a cmd command string"),
    ("powershell -", Script, Warn, "invokes PowerShell"),
    ("start-process", Script, Warn, "starts a process"),
    (
        "os.system(",
        Script,
        Warn,
        "runs shell commands from Python",
    ),
    ("subprocess.", Script, Warn, "spawns processes from Python"),
    ("child_process", Script, Warn, "spawns processes from Node"),
    ("eval(", Script, Warn, "evaluates dynamic code"),
    ("exec(", Script, Warn, "executes dynamic code"),
    ("schtasks", Script, Warn, "creates scheduled tasks"),
    ("crontab", Script, Warn, "edits crontab"),
    ("reg add", Script, Warn, "modifies the registry"),
    ("```bash", Script, Info, "contains a shell code block"),
    ("```sh", Script, Info, "contains a shell code block"),
    (
        "```powershell",
        Script,
        Info,
        "contains a PowerShell code block",
    ),
    ("```cmd", Script, Info, "contains a cmd code block"),
    // Network.
    ("curl ", Network, Warn, "uses curl"),
    ("wget ", Network, Warn, "uses wget"),
    ("invoke-webrequest", Network, Warn, "uses Invoke-WebRequest"),
    ("invoke-restmethod", Network, Warn, "uses Invoke-RestMethod"),
    ("netcat", Network, Warn, "uses netcat"),
    ("nc -e", Network, High, "netcat with command execution"),
    (
        "/dev/tcp/",
        Network,
        High,
        "opens a raw TCP socket from the shell",
    ),
    ("requests.post", Network, Warn, "sends HTTP requests"),
    ("requests.get", Network, Warn, "sends HTTP requests"),
    ("urllib", Network, Warn, "uses urllib"),
    ("xmlhttprequest", Network, Warn, "uses XMLHttpRequest"),
    ("fetch(", Network, Warn, "uses fetch()"),
    ("socket.", Network, Warn, "uses sockets"),
    ("http://", Network, Info, "references a URL"),
    ("https://", Network, Info, "references a URL"),
    // Obfuscation.
    ("base64 -d", Obfuscation, Warn, "decodes base64 in a shell"),
    (
        "base64 --decode",
        Obfuscation,
        Warn,
        "decodes base64 in a shell",
    ),
    (
        "frombase64string",
        Obfuscation,
        Warn,
        "decodes base64 in PowerShell",
    ),
    ("atob(", Obfuscation, Warn, "decodes base64 in JavaScript"),
    ("b64decode", Obfuscation, Warn, "decodes base64 in Python"),
    (
        "fromcharcode",
        Obfuscation,
        Warn,
        "builds text from char codes",
    ),
    // Credential stores.
    (".ssh/", Credential, Warn, "references SSH keys"),
    ("id_rsa", Credential, Warn, "references an SSH private key"),
    (
        ".aws/credentials",
        Credential,
        High,
        "references AWS credentials",
    ),
    (".netrc", Credential, Warn, "references .netrc"),
    (
        "login data",
        Credential,
        Warn,
        "references a browser password store",
    ),
    (
        "cookies.sqlite",
        Credential,
        Warn,
        "references browser cookies",
    ),
    ("wallet.dat", Credential, High, "references a crypto wallet"),
    // Prompt injection.
    ("ignore previous instructions", Injection, High, IGNORE),
    ("ignore all previous", Injection, High, IGNORE),
    ("ignore the above instructions", Injection, High, IGNORE),
    ("disregard previous", Injection, High, IGNORE),
    ("disregard your instructions", Injection, High, IGNORE),
    ("do not tell the user", Injection, High, HIDE),
    ("don't tell the user", Injection, High, HIDE),
    ("without telling the user", Injection, High, HIDE),
    ("do not mention this to the user", Injection, High, HIDE),
];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// True when `line` contains `prefix` followed by at least `min` token chars,
/// and `prefix` is not glued to a preceding identifier character.
fn has_token(line: &str, prefix: &str, min: usize) -> bool {
    let mut from = 0;
    while let Some(pos) = line[from..].find(prefix) {
        let at = from + pos;
        let before_ok = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let run = line[at + prefix.len()..]
            .chars()
            .take_while(|c| is_token_char(*c))
            .count();
        if before_ok && run >= min {
            return true;
        }
        from = at + prefix.len();
    }
    false
}

fn has_aws_key(line: &str) -> bool {
    line.as_bytes().windows(20).any(|w| {
        w.starts_with(b"AKIA")
            && w[4..]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    })
}

/// `password = "longvalue"`-style assignments (expects a lowercased line).
fn has_secret_assignment(lower: &str) -> bool {
    for key in ["api_key", "apikey", "secret", "password", "passwd", "token"] {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(key) {
            let after = &lower[from + pos + key.len()..];
            from += pos + key.len();
            let after = after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
            let after = after.trim_start();
            let Some(rest) = after.strip_prefix(['=', ':']) else {
                continue;
            };
            let rest = rest.trim_start().trim_start_matches(['"', '\'']);
            let value: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '+' | '_' | '-'))
                .collect();
            let placeholder = ["your", "example", "xxxx", "placeholder"]
                .iter()
                .any(|p| value.contains(p));
            if value.len() >= 20 && !placeholder {
                return true;
            }
        }
    }
    false
}

fn longest_base64_run(line: &str) -> usize {
    let mut best = 0;
    let mut cur = 0;
    for c in line.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=') {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

struct Collector<'a> {
    file: &'a str,
    seen: HashSet<(FindingKind, &'static str)>,
    out: Vec<Finding>,
}

impl Collector<'_> {
    fn add(
        &mut self,
        kind: FindingKind,
        severity: Severity,
        line: Option<usize>,
        message: &'static str,
    ) {
        // One finding per distinct message per file keeps reports readable.
        if self.seen.insert((kind, message)) {
            self.out.push(Finding {
                kind,
                severity,
                file: self.file.to_string(),
                line,
                message: message.to_string(),
            });
        }
    }
}

struct DecodeBudget {
    used: usize,
}

fn fold_confusable(c: char) -> char {
    match c {
        'а' | 'α' => 'a',
        'е' | 'ё' | 'ε' => 'e',
        'о' | 'ο' => 'o',
        'р' | 'ρ' => 'p',
        'с' | 'ϲ' => 'c',
        'у' => 'y',
        'х' | 'χ' => 'x',
        'і' | 'ї' | 'ι' => 'i',
        'ј' => 'j',
        'ѕ' => 's',
        'һ' | 'н' => 'h',
        'ԁ' => 'd',
        'ɡ' => 'g',
        'т' | 'τ' => 't',
        'к' | 'κ' => 'k',
        'м' => 'm',
        'в' | 'β' => 'b',
        'ν' => 'v',
        'υ' => 'u',
        '│' | '∣' | 'ǀ' => '|',
        _ => c,
    }
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_ws = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// NFKC, case-fold, confusable-fold, then collapse whitespace so rules see
/// one canonical line.
fn normalize_line(line: &str) -> String {
    let nfkc: String = line.nfkc().filter(|c| !is_invisible(*c)).collect();
    let folded: String = nfkc.to_lowercase().chars().map(fold_confusable).collect();
    collapse_whitespace(&folded)
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

fn take_hex(text: &str, digits: usize) -> Option<(u32, &str)> {
    let bytes = text.as_bytes();
    if bytes.len() < digits
        || !bytes[..digits]
            .iter()
            .copied()
            .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let mut value = 0u32;
    for byte in &bytes[..digits] {
        value = (value << 4) | u32::from(hex_val(*byte));
    }
    Some((value, &text[digits..]))
}

fn decoded_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_DECODED_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    text.chars()
        .any(|c| c.is_ascii_alphabetic())
        .then(|| text.to_string())
}

fn decode_base64(run: &str) -> Option<String> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(run) {
            return decoded_text(&bytes);
        }
    }
    None
}

fn decode_hex(run: &str) -> Option<String> {
    if run.len() < MIN_ENCODED_RUN || run.len() > MAX_DECODED_BYTES || !run.len().is_multiple_of(2)
    {
        return None;
    }
    let bytes = run.as_bytes();
    let mut out = Vec::with_capacity(run.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_hexdigit() || !bytes[i + 1].is_ascii_hexdigit() {
            return None;
        }
        out.push((hex_val(bytes[i]) << 4) | hex_val(bytes[i + 1]));
        i += 2;
    }
    decoded_text(&out)
}

fn for_runs(line: &str, mut is_token: impl FnMut(char) -> bool, mut on_run: impl FnMut(&str)) {
    let mut start = None;
    for (index, ch) in line.char_indices() {
        if is_token(ch) {
            if start.is_none() {
                start = Some(index);
            }
        } else if let Some(from) = start.take() {
            on_run(&line[from..index]);
        }
    }
    if let Some(from) = start {
        on_run(&line[from..]);
    }
}

fn consider_base64(out: &mut Vec<String>, line: &str, budget: &mut DecodeBudget) {
    for_runs(
        line,
        |c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'),
        |run| {
            if budget.used >= MAX_DECODED_BLOBS || run.len() < MIN_ENCODED_RUN {
                return;
            }
            if let Some(text) = decode_base64(run) {
                budget.used += 1;
                out.push(text);
            }
        },
    );
}

fn consider_hex(out: &mut Vec<String>, line: &str, budget: &mut DecodeBudget) {
    for_runs(
        line,
        |c| c.is_ascii_hexdigit(),
        |run| {
            if budget.used >= MAX_DECODED_BLOBS {
                return;
            }
            if let Some(text) = decode_hex(run) {
                budget.used += 1;
                out.push(text);
            }
        },
    );
}

fn percent_decode(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let has_escape = bytes
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit());
    if !has_escape {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out.push((hex_val(bytes[i + 1]) << 4) | hex_val(bytes[i + 2]));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    decoded_text(&out)
}

fn decode_escapes(line: &str) -> Option<String> {
    if !line.contains("\\x") && !line.contains("\\u") {
        return None;
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut changed = false;
    while let Some(ch) = rest.chars().next() {
        if let Some(hex) = rest.strip_prefix("\\x")
            && let Some((value, next)) = take_hex(hex, 2)
        {
            out.push(char::from(u8::try_from(value).unwrap_or(0)));
            rest = next;
            changed = true;
            continue;
        }
        if let Some(hex) = rest.strip_prefix("\\u")
            && let Some((value, next)) = take_hex(hex, 4)
            && let Some(decoded) = char::from_u32(value)
        {
            out.push(decoded);
            rest = next;
            changed = true;
            continue;
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    changed
        .then_some(out)
        .and_then(|text| decoded_text(text.as_bytes()))
}

fn push_decoded(out: &mut Vec<String>, budget: &mut DecodeBudget, text: Option<String>) {
    if budget.used >= MAX_DECODED_BLOBS {
        return;
    }
    if let Some(text) = text {
        budget.used += 1;
        out.push(text);
    }
}

fn decode_payloads(original: &str, normalized: &str, budget: &mut DecodeBudget) -> Vec<String> {
    let mut out = Vec::new();
    consider_base64(&mut out, original, budget);
    if normalized != original {
        consider_base64(&mut out, normalized, budget);
    }
    consider_hex(&mut out, original, budget);
    push_decoded(&mut out, budget, percent_decode(original));
    push_decoded(&mut out, budget, decode_escapes(original));
    out
}

fn note_secrets(c: &mut Collector<'_>, line: &str, normalized: &str, n: Option<usize>) {
    if (line.contains("-----BEGIN") && line.contains("PRIVATE KEY"))
        || (normalized.contains("-----begin") && normalized.contains("private key"))
    {
        c.add(Credential, High, n, "contains a private key");
    }
    if has_aws_key(line) {
        c.add(Credential, High, n, "contains an AWS access key id");
    }
    if has_token(line, "ghp_", 30)
        || has_token(line, "gho_", 30)
        || has_token(line, "github_pat_", 30)
        || has_token(line, "xoxb-", 20)
        || has_token(line, "xoxp-", 20)
        || has_token(line, "sk-", 20)
    {
        c.add(Credential, High, n, "contains an API token");
    }
    if has_secret_assignment(normalized) {
        c.add(
            Credential,
            Warn,
            n,
            "assigns a long literal to a secret-like name",
        );
    }
    if longest_base64_run(line) >= 200 {
        c.add(
            Obfuscation,
            Warn,
            n,
            "contains a very long base64-like blob",
        );
    }
    if normalized.matches("\\x").count() >= 8 || normalized.matches("\\u00").count() >= 8 {
        c.add(Obfuscation, Warn, n, "contains many escaped byte sequences");
    }
}

fn inspect_line(
    c: &mut Collector<'_>,
    line: &str,
    n: Option<usize>,
    decode: bool,
    include_info: bool,
    budget: &mut DecodeBudget,
) {
    if line.chars().any(is_invisible) {
        c.add(
            Obfuscation,
            High,
            n,
            "contains invisible or bidirectional control characters",
        );
    }
    let normalized = normalize_line(line);
    for (pat, kind, sev, msg) in RULES {
        if !include_info && *sev == Info {
            continue;
        }
        if normalized.contains(pat) {
            c.add(*kind, *sev, n, msg);
        }
    }
    note_secrets(c, line, &normalized, n);
    if decode {
        for decoded in decode_payloads(line, &normalized, budget) {
            for decoded_line in decoded.lines() {
                inspect_line(c, decoded_line, n, false, false, budget);
            }
        }
    }
}

fn scan_text(file: &str, text: &str, out: &mut Vec<Finding>) {
    let mut c = Collector {
        file,
        seen: HashSet::new(),
        out: Vec::new(),
    };
    let mut budget = DecodeBudget { used: 0 };
    for (i, line) in text.lines().enumerate() {
        inspect_line(&mut c, line, Some(i + 1), true, true, &mut budget);
    }
    out.extend(c.out);
}

fn extension(rel: &str) -> String {
    Path::new(rel)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn file_finding(f: &TreeFile, kind: FindingKind, severity: Severity, message: &str) -> Finding {
    Finding {
        kind,
        severity,
        file: f.rel.clone(),
        line: None,
        message: message.to_string(),
    }
}

fn is_active_ext(ext: &str) -> bool {
    SCRIPT_EXT.contains(&ext) || NATIVE_EXT.contains(&ext)
}

/// Scan one file. Returns whether it is an active file type.
fn scan_file(f: &TreeFile, bytes: &[u8], out: &mut Vec<Finding>) -> bool {
    let ext = extension(&f.rel);
    if f.size > MAX_SCAN_FILE_BYTES {
        out.push(file_finding(
            f,
            FindingKind::Oversized,
            Warn,
            &format!("file is {} bytes (limit {MAX_SCAN_FILE_BYTES})", f.size),
        ));
    }
    let native_magic = bytes.starts_with(b"MZ")
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE])
        || bytes.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]);
    let active = native_magic || is_active_ext(&ext);
    if NATIVE_EXT.contains(&ext.as_str()) || native_magic {
        out.push(file_finding(
            f,
            Script,
            High,
            "contains a native executable or library",
        ));
    } else if SCRIPT_EXT.contains(&ext.as_str()) {
        out.push(file_finding(f, Script, Warn, "contains a script file"));
    }
    let non_text = bytes.contains(&0) || std::str::from_utf8(bytes).is_err();
    if non_text && !INERT_BINARY_EXT.contains(&ext.as_str()) && !native_magic {
        out.push(file_finding(
            f,
            Obfuscation,
            Warn,
            "contains a non-text binary file",
        ));
    }
    // NUL and invalid UTF-8 are legal places to hide text the agent will read.
    let text = String::from_utf8_lossy(bytes);
    scan_text(&f.rel, &text, out);
    active
}

/// Scan the skill tree at `root` (see the module docs).
///
/// # Errors
/// Fails on I/O errors, symlinks/special files, or when the tree exceeds the
/// size limits ([`util::MAX_FILES`], [`util::MAX_TOTAL_BYTES`]).
pub fn scan_dir(root: &Path) -> Result<RiskReport, HubError> {
    let files = util::walk(root)?;
    let mut findings = Vec::new();
    let mut active = false;
    for f in &files {
        let bytes = fs::read(&f.path).map_err(|e| io_err("read", &f.path, &e))?;
        active |= scan_file(f, &bytes, &mut findings);
    }
    Ok(RiskReport {
        total_bytes: files.iter().map(|f| f.size).sum(),
        fingerprint: util::hash_files(&files)?,
        files: files.len(),
        active,
        findings,
    })
}

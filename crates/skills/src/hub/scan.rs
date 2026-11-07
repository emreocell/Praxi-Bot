//! Static safety scan of a skill tree before installation.
//!
//! Heuristic and conservative: it never executes anything, it only reads the
//! files and reports what looks risky. The result is a [`RiskReport`] the
//! user must accept (see [`RiskReport::requires_acceptance`]) before the hub
//! installs the skill. A clean scan is not a guarantee of safety.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

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

    /// True when any finding is a warning or worse, so the user has to accept
    /// the report (by passing [`RiskReport::fingerprint`]) to install.
    #[must_use]
    pub fn requires_acceptance(&self) -> bool {
        self.risk() != RiskLevel::Low
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
    "pl", "php", "lua", "vbs", "jar", "wasm",
];
const NATIVE_EXT: &[&str] = &[
    "exe", "dll", "so", "dylib", "msi", "scr", "com", "bin", "node",
];
const INERT_BINARY_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "ico", "pdf", "svg"];

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
    ("os.system(", Script, Warn, "runs shell commands from Python"),
    ("subprocess.", Script, Warn, "spawns processes from Python"),
    ("child_process", Script, Warn, "spawns processes from Node"),
    ("eval(", Script, Warn, "evaluates dynamic code"),
    ("exec(", Script, Warn, "executes dynamic code"),
    ("schtasks", Script, Warn, "creates scheduled tasks"),
    ("crontab", Script, Warn, "edits crontab"),
    ("reg add", Script, Warn, "modifies the registry"),
    ("```bash", Script, Info, "contains a shell code block"),
    ("```sh", Script, Info, "contains a shell code block"),
    ("```powershell", Script, Info, "contains a PowerShell code block"),
    ("```cmd", Script, Info, "contains a cmd code block"),
    // Network.
    ("curl ", Network, Warn, "uses curl"),
    ("wget ", Network, Warn, "uses wget"),
    ("invoke-webrequest", Network, Warn, "uses Invoke-WebRequest"),
    ("invoke-restmethod", Network, Warn, "uses Invoke-RestMethod"),
    ("netcat", Network, Warn, "uses netcat"),
    ("nc -e", Network, High, "netcat with command execution"),
    ("/dev/tcp/", Network, High, "opens a raw TCP socket from the shell"),
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
    ("base64 --decode", Obfuscation, Warn, "decodes base64 in a shell"),
    ("frombase64string", Obfuscation, Warn, "decodes base64 in PowerShell"),
    ("atob(", Obfuscation, Warn, "decodes base64 in JavaScript"),
    ("b64decode", Obfuscation, Warn, "decodes base64 in Python"),
    ("fromcharcode", Obfuscation, Warn, "builds text from char codes"),
    // Credential stores.
    (".ssh/", Credential, Warn, "references SSH keys"),
    ("id_rsa", Credential, Warn, "references an SSH private key"),
    (".aws/credentials", Credential, High, "references AWS credentials"),
    (".netrc", Credential, Warn, "references .netrc"),
    ("login data", Credential, Warn, "references a browser password store"),
    ("cookies.sqlite", Credential, Warn, "references browser cookies"),
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

fn scan_text(file: &str, text: &str, out: &mut Vec<Finding>) {
    let mut c = Collector {
        file,
        seen: HashSet::new(),
        out: Vec::new(),
    };
    for (i, line) in text.lines().enumerate() {
        let n = Some(i + 1);
        let lower = line.to_lowercase();
        for (pat, kind, sev, msg) in RULES {
            if lower.contains(pat) {
                c.add(*kind, *sev, n, msg);
            }
        }
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
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
        if has_secret_assignment(&lower) {
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
        if lower.matches("\\x").count() >= 8 || lower.matches("\\u00").count() >= 8 {
            c.add(
                Obfuscation,
                Warn,
                n,
                "contains many escaped byte sequences",
            );
        }
        if line.chars().any(is_invisible) {
            c.add(
                Obfuscation,
                High,
                n,
                "contains invisible or bidirectional control characters",
            );
        }
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

fn scan_file(f: &TreeFile, bytes: &[u8], out: &mut Vec<Finding>) {
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
    let text = if bytes.contains(&0) {
        None
    } else {
        std::str::from_utf8(bytes).ok()
    };
    match text {
        Some(text) => scan_text(&f.rel, text, out),
        None => {
            if !INERT_BINARY_EXT.contains(&ext.as_str()) && !native_magic {
                out.push(file_finding(
                    f,
                    Obfuscation,
                    Warn,
                    "contains a non-text binary file",
                ));
            }
        }
    }
}

/// Scan the skill tree at `root` (see the module docs).
///
/// # Errors
/// Fails on I/O errors, symlinks/special files, or when the tree exceeds the
/// size limits ([`util::MAX_FILES`], [`util::MAX_TOTAL_BYTES`]).
pub fn scan_dir(root: &Path) -> Result<RiskReport, HubError> {
    let files = util::walk(root)?;
    let mut findings = Vec::new();
    for f in &files {
        let bytes = fs::read(&f.path).map_err(|e| io_err("read", &f.path, &e))?;
        scan_file(f, &bytes, &mut findings);
    }
    Ok(RiskReport {
        total_bytes: files.iter().map(|f| f.size).sum(),
        fingerprint: util::hash_files(&files)?,
        files: files.len(),
        findings,
    })
}

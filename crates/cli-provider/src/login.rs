//! Interactive sign-in of a coding CLI on THIS machine (the always-on host), driven from the
//! owner's desktop.
//!
//! Only the vendor's own, unmodified login command runs, from a fixed registry entry (argv
//! arrays, no shell, nothing supplied by a client). Its output stays in bounded private memory;
//! only a verified sign-in page, a one-time device code and the fact that it wants a pasted code
//! leave this module. A pasted code goes to the process' stdin and nowhere else.
//!
//! What each vendor offers headlessly (checked with `--help` only, never by signing in):
//! * `claude auth login` prints a sign-in link and, after the person signed in in ANY browser,
//!   reads the code Anthropic's page shows from stdin (paste-code flow). `claude auth status
//!   --json` confirms.
//! * `codex login --device-auth` prints a page (`https://auth.openai.com/codex/device`) and a
//!   one-time code (device-code flow); the process exits 0 when the person approved.
//!   `codex login status` confirms.
//! * The engines below have no headless-friendly mode of their own, so they run on a pseudo
//!   terminal (`pty_session`): the CLI believes a person is typing, we read what it prints.
//!   `cursor-agent login` and `qodercli login` print a sign-in link and finish by themselves
//!   (qoder, read from npm package 1.1.99: "Login successful! Welcome, …"); `kimi login`
//!   prints a device link and a one-time code (read from npm package 2.8.1); `kiro-cli login
//!   --license free --use-device-flow` prints a device link and code (Kiro docs); `agy` signs in
//!   on its first run (vendor docs; no login subcommand in `agy --help`) and `opencode auth
//!   login opencode` asks "Select login method" (checked against opencode 2.3.0 on a PTY; the
//!   first entry, `OpenCode Console account`, is the browser sign-in). Each fails closed to
//!   "run it over SSH" when its prompt is not the one expected.
//!
//! Only the output rules in [`LoginSpec`] decide what leaves this module: a link on one of the
//! vendor's own hosts, a one-time code, and the account name the CLI prints once it is done.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::mpsc;

use crate::detect::{ChildTree, PathEnv, ShimError, apply_sanitized_env, isolate, resolve_spawn};
use pty_session::{PtySession, SpawnSpec};

/// Most output bytes kept per login (everything beyond is dropped, never forwarded).
const MAX_OUTPUT: usize = 32 * 1024;
/// A pasted code: one line, this long at most.
pub const MAX_CODE: usize = 1024;

/// How a vendor's login finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    /// The CLI prints a page and a one-time code; the person types the code on the page.
    DeviceCode,
    /// The CLI prints a sign-in link; the person signs in and pastes the page's code back.
    PasteCode,
    /// The CLI prints a sign-in link, the person signs in in any browser, and the CLI finishes
    /// by itself (no code to type or paste).
    Browser,
}

/// How the vendor's command is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Plain pipes (the CLI has a real non-interactive mode).
    Pipe,
    /// A pseudo terminal (the CLI only prints its link when it thinks a person is typing).
    Pty,
}

/// One scripted answer to a menu the CLI shows before it reaches the sign-in link.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    /// Wait until the screen text contains this (case-insensitive).
    pub wait_for: &'static str,
    /// Then type this (`\r` is Enter).
    pub send: &'static str,
}

/// Which host a sign-in page may be on. Never "this domain and everything under it": shared
/// hosting names (`amazonaws.com`, `google.com`, `awsapps.com`) hand out subdomains and open
/// redirects to anybody.
#[derive(Debug, Clone, Copy)]
pub enum Host {
    /// This host and no other.
    Exact(&'static str),
    /// `<labels>.<region>.amazonaws.com`, where the region is a real AWS region name
    /// (`device.sso.us-east-1.amazonaws.com`): `Aws("device.sso")`.
    Aws(&'static str),
}

/// One page a CLI may ask the person to open: an exact host and the start of its path.
#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub host: Host,
    /// The path is this, or lies under it (`/` allows any path).
    pub path: &'static str,
}

impl Page {
    const fn at(host: &'static str, path: &'static str) -> Self {
        Self {
            host: Host::Exact(host),
            path,
        }
    }
}

/// Where a CLI keeps a credential file, so it can be found in the service user's home.
#[derive(Debug, Clone, Copy)]
pub enum Root {
    /// The home folder.
    Home,
    /// `$XDG_DATA_HOME`, and `~/.local/share` when it is not set.
    DataHome,
    /// `%LOCALAPPDATA%`, and `~/AppData/Local` when it is not set.
    LocalAppData,
}

/// A credential FILE a sign-in writes (never a folder: a folder also fills with settings and
/// caches). Only its existence, size and modification time are looked at, never its content.
#[derive(Debug, Clone, Copy)]
pub struct CredentialFile {
    pub root: Root,
    pub path: &'static [&'static str],
}

/// One entry of the fixed registry. No field is ever derived from a request.
#[derive(Debug)]
pub struct LoginSpec {
    pub engine: &'static str,
    /// Argv after the executable.
    pub args: &'static [&'static str],
    pub kind: FlowKind,
    /// The only pages that may be shown (exact hosts, expected paths). The first one the CLI
    /// prints is the one shown.
    pub pages: &'static [Page],
    pub transport: Transport,
    /// A device flow with one fixed page: only this page is shown, without its query.
    pub device_page: Option<&'static str>,
    /// Extra environment (never a secret): keeps the CLI from opening a browser on the server.
    pub env: &'static [(&'static str, &'static str)],
    /// Menu answers typed before the link appears.
    pub script: &'static [Step],
    /// The credential files the sign-in writes. For a CLI that has no status command and does
    /// not exit when it is done: one of them appearing or changing after the page was shown is
    /// the evidence.
    pub credentials: &'static [CredentialFile],
    /// Text the CLI prints when it succeeded (case-insensitive). With no status command, an
    /// exit code 0 plus one of these is the evidence.
    pub success_markers: &'static [&'static str],
}

impl LoginSpec {
    const BASE: Self = Self {
        engine: "",
        args: &[],
        kind: FlowKind::Browser,
        pages: &[],
        transport: Transport::Pty,
        device_page: None,
        env: &[],
        script: &[],
        credentials: &[],
        success_markers: &[],
    };
}

pub const LOGIN_SPECS: &[LoginSpec] = &[
    LoginSpec {
        engine: "claude",
        args: &["auth", "login"],
        kind: FlowKind::PasteCode,
        pages: &[
            Page::at("claude.com", "/cai/oauth/"),
            Page::at("claude.com", "/oauth/"),
            Page::at("claude.ai", "/oauth/"),
            Page::at("claude.ai", "/cai/oauth/"),
            Page::at("platform.claude.com", "/oauth/"),
            Page::at("console.anthropic.com", "/oauth/"),
        ],
        transport: Transport::Pipe,
        ..LoginSpec::BASE
    },
    LoginSpec {
        engine: "codex",
        args: &["login", "--device-auth"],
        kind: FlowKind::DeviceCode,
        pages: &[Page::at("auth.openai.com", "/codex/device")],
        transport: Transport::Pipe,
        device_page: Some("https://auth.openai.com/codex/device"),
        ..LoginSpec::BASE
    },
    // `cursor-agent login` opens a browser to a `cursor.com/loginDeepControl` page and polls
    // until it is approved; `NO_OPEN_BROWSER` makes it print the link instead.
    LoginSpec {
        engine: "cursor",
        args: &["login"],
        kind: FlowKind::Browser,
        pages: &[Page::at("cursor.com", "/loginDeepControl")],
        env: &[("NO_OPEN_BROWSER", "1")],
        ..LoginSpec::BASE
    },
    // Antigravity has no login command: its first interactive run signs in (Google OAuth) and
    // asks for the code the page shows. It keeps running afterwards, so the sign-in is
    // confirmed by the token file it writes under `~/.gemini/antigravity-cli/`. Only Google's
    // OAuth page is shown: `google.com` as a whole has open redirects and user sites. The folder
    // also holds settings, caches and conversations, so only token files count; the names are
    // read from the CLI binary's strings, not from a sign-in (a wrong name fails closed:
    // "not confirmed").
    LoginSpec {
        engine: "agy",
        args: &[],
        kind: FlowKind::PasteCode,
        pages: &[Page::at("accounts.google.com", "/o/oauth2/")],
        credentials: &[
            CredentialFile {
                root: Root::Home,
                path: &[".gemini", "antigravity-cli", "authstore.enc"],
            },
            CredentialFile {
                root: Root::Home,
                path: &[".gemini", "antigravity-cli", "auth.json"],
            },
        ],
        ..LoginSpec::BASE
    },
    // `opencode auth login opencode` (the integration is named, so there is no provider menu)
    // asks "Select login method": "OpenCode Console account" (browser sign-in, the default) or
    // "API key (service account)". Enter takes the default. Other providers are chosen over
    // SSH, or by the bot with `shell_interactive`.
    LoginSpec {
        engine: "opencode",
        args: &["auth", "login", "opencode"],
        kind: FlowKind::Browser,
        pages: &[Page::at("opencode.ai", "/auth")],
        script: &[Step {
            wait_for: "select login method",
            send: "\r",
        }],
        // OpenCode keeps its credentials in `auth.json` under its data folder: `$XDG_DATA_HOME`
        // or `~/.local/share`, and `%LOCALAPPDATA%` on Windows.
        credentials: &[
            CredentialFile {
                root: Root::DataHome,
                path: &["opencode", "auth.json"],
            },
            CredentialFile {
                root: Root::LocalAppData,
                path: &["opencode", "auth.json"],
            },
        ],
        success_markers: &["login successful", "logged in", "credentials saved"],
        ..LoginSpec::BASE
    },
    // AWS Builder ID device flow (`--use-device-flow` prints a code and a link instead of
    // opening a browser; `--license free` skips the account-type menu).
    LoginSpec {
        engine: "kiro",
        args: &["login", "--license", "free", "--use-device-flow"],
        kind: FlowKind::DeviceCode,
        // The device page lives on `device.sso.<region>.amazonaws.com`; Builder ID's own start
        // page is `view.awsapps.com`. Every other `awsapps.com` name is somebody's own tenant.
        pages: &[
            Page {
                host: Host::Aws("device.sso"),
                path: "/",
            },
            Page::at("view.awsapps.com", "/start"),
        ],
        success_markers: &["logged in", "login successful", "device authorized"],
        ..LoginSpec::BASE
    },
    // "Starting browser login..." then the link on its own line, then
    // "Login successful! Welcome, <name>." or "Already logged in as <name>.".
    LoginSpec {
        engine: "qoder",
        args: &["login"],
        kind: FlowKind::Browser,
        pages: &[
            Page::at("qoder.com", "/device"),
            Page::at("qoder.cn", "/device"),
            Page::at("qoder.sh", "/device"),
            Page::at("qoder.ai", "/device"),
        ],
        success_markers: &["login successful", "already logged in"],
        ..LoginSpec::BASE
    },
    // "Opening browser for Kimi device login: <link>" / "...enter code: <code>" /
    // "Logged in to <provider>." (npm package 2.8.1, login-flow source).
    LoginSpec {
        engine: "kimi",
        args: &["login"],
        kind: FlowKind::DeviceCode,
        pages: &[
            Page::at("www.kimi.com", "/code/"),
            Page::at("kimi.com", "/code/"),
            Page::at("www.kimi.ai", "/code/"),
            Page::at("kimi.ai", "/code/"),
        ],
        success_markers: &["logged in to"],
        ..LoginSpec::BASE
    },
];

#[must_use]
pub fn login_spec(engine: &str) -> Option<&'static LoginSpec> {
    LOGIN_SPECS.iter().find(|s| s.engine == engine)
}

/// What the output asks of the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Open `url`; for a device flow type `code` there.
    Page { url: String, code: Option<String> },
}

fn strip_ansi(text: &str) -> String {
    static ANSI: OnceLock<regex::Regex> = OnceLock::new();
    let re = ANSI.get_or_init(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").expect("valid pattern")
    });
    re.replace_all(text, "").into_owned()
}

/// `us-east-1`, `eu-central-2`, `ap-southeast-3`, `us-gov-west-1`: lower-case words joined by
/// `-`, ending in a one or two digit number.
fn is_aws_region(name: &str) -> bool {
    let words: Vec<&str> = name.split('-').collect();
    let [first, middle @ .., last] = words.as_slice() else {
        return false;
    };
    let word = |w: &&str| (2..=10).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase());
    !middle.is_empty()
        && word(first)
        && middle.iter().all(word)
        && (1..=2).contains(&last.len())
        && last.bytes().all(|b| b.is_ascii_digit())
}

fn host_matches(rule: Host, host: &str) -> bool {
    match rule {
        Host::Exact(exact) => host == exact,
        Host::Aws(labels) => host
            .strip_suffix(".amazonaws.com")
            .and_then(|rest| rest.strip_prefix(labels))
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(is_aws_region),
    }
}

/// `path` is `prefix` or lies under it, on a `/` boundary.
fn path_matches(prefix: &str, path: &str) -> bool {
    prefix == "/"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| prefix.ends_with('/') || rest.starts_with('/'))
}

/// An https page the spec lists (exact host, expected path), with no login, port or fragment. A
/// device page with one fixed address is shown as that address without its query (the code is
/// shown separately); every other page keeps its query, which carries the sign-in state.
fn trusted_page(value: &str, spec: &LoginSpec) -> Option<String> {
    let value = value.trim_end_matches([')', ',', '.', ';', '"', '\'', '>']);
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let trusted = spec
        .pages
        .iter()
        .any(|p| host_matches(p.host, &host) && path_matches(p.path, url.path()));
    let plain = url.scheme() == "https"
        && trusted
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none();
    if !plain {
        return None;
    }
    match spec.device_page {
        Some(page) if spec.kind == FlowKind::DeviceCode => {
            let fixed = url::Url::parse(page).ok()?;
            (url.host_str() == fixed.host_str()
                && url.path().trim_end_matches('/') == fixed.path().trim_end_matches('/')
                && url.query().is_none())
            .then(|| page.to_owned())
        }
        _ => Some(url.to_string()),
    }
}

/// A one-time device code: `ABCD-1234` on a line of its own or anywhere in a line, or the word
/// after `code:` (`... and enter code: ABCD1234`).
fn device_code(line: &str) -> Option<String> {
    static DASHED: OnceLock<regex::Regex> = OnceLock::new();
    static LABELLED: OnceLock<regex::Regex> = OnceLock::new();
    let dashed = DASHED.get_or_init(|| {
        regex::Regex::new(r"^[A-Z0-9]{4,8}-[A-Z0-9]{4,8}$").expect("valid pattern")
    });
    let labelled = LABELLED.get_or_init(|| {
        regex::Regex::new(r"(?i)\bcode\b\s*[:=]?\s*([A-Z0-9]{4,8}(?:-[A-Z0-9]{4,8})?)\s*$")
            .expect("valid pattern")
    });
    let line = line.trim();
    if dashed.is_match(line) {
        return Some(line.to_owned());
    }
    // A word of the line that has the shape of a dashed code.
    if let Some(word) = line
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .find(|w| dashed.is_match(w))
    {
        return Some(word.to_owned());
    }
    // The label form is only trusted when the code has a digit or a dash: it keeps a plain
    // English word ("code: please") from being taken for a code.
    labelled
        .captures(line)
        .map(|c| c[1].to_owned())
        .filter(|c| {
            c.bytes().any(|b| b.is_ascii_digit() || b == b'-')
                || c.chars().all(|ch| ch.is_ascii_uppercase())
        })
}

/// Read what to show from the output so far. Only COMPLETE lines count (output arrives in
/// chunks that can stop inside a code), and only text matching the vendor's rules is returned.
#[must_use]
pub fn parse_prompt(spec: &LoginSpec, output: &str) -> Option<Prompt> {
    let clean = strip_ansi(output);
    let mut lines: Vec<&str> = clean.split('\n').collect();
    lines.pop(); // the last, unfinished line
    let mut url = None;
    let mut code = None;
    for line in lines {
        let line = line.trim_end_matches('\r').trim();
        // The FIRST page the CLI prints is the one shown: whatever it prints later (an error
        // message that quotes a link, a changelog) never replaces it.
        if url.is_none() {
            url = line
                .split_whitespace()
                .filter(|w| w.starts_with("https://"))
                .find_map(|w| trusted_page(w, spec));
        }
        if matches!(spec.kind, FlowKind::DeviceCode | FlowKind::Browser)
            && let Some(c) = device_code(line)
        {
            code = Some(c);
        }
    }
    let url = url?;
    match spec.kind {
        FlowKind::PasteCode => Some(Prompt::Page { url, code: None }),
        // A browser flow needs no code, but shows one when the CLI prints it.
        FlowKind::Browser => Some(Prompt::Page { url, code }),
        FlowKind::DeviceCode => code.map(|code| Prompt::Page {
            url,
            code: Some(code),
        }),
    }
}

fn account_pattern() -> &'static regex::Regex {
    static ACCOUNT: OnceLock<regex::Regex> = OnceLock::new();
    ACCOUNT.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:login successful! welcome,|(?:already )?logged in as|signed in as)\s+([^\s,;]+(?:@[^\s,;]+)?)",
        )
        .expect("valid pattern")
    })
}

/// The account the CLI says it signed in as, from its success line (`Welcome, …`,
/// `Already logged in as …`, `Logged in as …`). Bounded and printable only; `None`
/// when the CLI does not name one.
#[must_use]
pub fn parse_account(output: &str) -> Option<String> {
    let re = account_pattern();
    let clean = strip_ansi(output);
    let name = clean
        .lines()
        .find_map(|l| re.captures(l.trim()).map(|c| c[1].to_owned()))?;
    let name = name.trim_end_matches(['.', '!', ')']).trim();
    (!name.is_empty() && name.chars().count() <= 160 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// Is `word` one that turns the phrase after it into its opposite ("not logged in")?
fn is_negation(word: &str) -> bool {
    matches!(
        word,
        "not" | "no" | "never" | "cannot" | "unable" | "fail" | "failed" | "without"
    ) || word.ends_with("n't")
}

/// Does `marker` occur in `line` as a statement ("Logged in to X"), not as a denial ("You are
/// not logged in")? It must start a word, and none of the words before it in its sentence may
/// be a negation.
fn marker_in_line(line: &str, marker: &str) -> bool {
    line.match_indices(marker).any(|(at, _)| {
        let before = &line[..at];
        if before
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
        {
            return false;
        }
        let clause = before
            .rsplit(['.', '!', '?', ';'])
            .next()
            .unwrap_or_default();
        !clause
            .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
            .any(|w| is_negation(&w.replace('\u{2019}', "'")))
    })
}

/// Did the CLI print one of the spec's success lines? A negated phrase ("not logged in", "you
/// are no longer logged in") is not one.
#[must_use]
pub fn printed_success(spec: &LoginSpec, output: &str) -> bool {
    let lower = strip_ansi(output).to_ascii_lowercase();
    lower
        .lines()
        .any(|line| spec.success_markers.iter().any(|m| marker_in_line(line, m)))
}

/// What Anthropic's page shows after sign-in: a code, sometimes with `#state`.
#[must_use]
pub fn valid_code(value: &str) -> Option<&str> {
    let code = value.trim();
    (code.len() >= 8
        && code.len() <= MAX_CODE
        && code.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'#' | b'.' | b':' | b'/')
        }))
    .then_some(code)
}

/// A running login command. Dropping it kills the process tree.
pub struct LoginRun {
    inner: Inner,
    /// How many of the spec's menu steps were answered.
    script_done: usize,
    /// The spec's credential files as they were when the run started, and again when its page
    /// was shown ([`Self::watch_credentials`]): only a change after that counts as a sign-in.
    baseline: Stamps,
}

enum Inner {
    Pipe(PipeRun),
    Pty(PtySession),
}

struct PipeRun {
    child: ChildTree,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Event>,
    output: String,
    closed_streams: u8,
}

enum Event {
    Bytes(Vec<u8>),
    Closed,
}

/// Why a login could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// The CLI is not on the search path.
    NotInstalled,
    /// Refused by the shim rules or could not be spawned.
    SpawnFailed,
    /// The engine's files are not the ones that were pinned at install time.
    FilesChanged,
    /// Not pinned and not in a system location.
    UntrustedLocation,
}

/// One file of an engine's entry point (the program, and the script it runs when it is a
/// shim), pinned at install time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FilePin {
    /// Real path (symlinks resolved).
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Why a login executable is not trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustError {
    /// The engine is not on the search path.
    NotInstalled,
    /// The entry point is not the one that was pinned at install time, or its bytes changed.
    FilesChanged,
    /// No pin exists and the engine does not live in a system location.
    UntrustedLocation,
    /// The shim could not be resolved or hashed.
    Unreadable,
}

fn resolve(engine: &str, env: &PathEnv) -> Result<crate::detect::ResolvedCommand, StartError> {
    let spec = crate::engine_spec(engine).ok_or(StartError::SpawnFailed)?;
    let candidate = crate::search::find_engine(spec, env).ok_or(StartError::NotInstalled)?;
    let resolved =
        resolve_spawn(&candidate, env).map_err(|ShimError::Refused| StartError::SpawnFailed)?;
    if crate::detect::is_batch_file(&resolved.program) {
        return Err(StartError::SpawnFailed);
    }
    Ok(resolved)
}

fn sha256_file(path: &std::path::Path) -> std::io::Result<(u64, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let hex = digest.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    Ok((size, hex))
}

/// The files that make up the engine's entry point right now: the program and any argument that
/// is an existing file (an npm shim runs `node <script>`), each by real path with size and hash.
fn current_files(resolved: &crate::detect::ResolvedCommand) -> Result<Vec<FilePin>, TrustError> {
    let mut paths = vec![resolved.program.clone()];
    paths.extend(
        resolved
            .args
            .iter()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_file()),
    );
    paths
        .into_iter()
        .map(|p| {
            let real = std::fs::canonicalize(&p)
                .map(crate::search::plain_path)
                .map_err(|_| TrustError::Unreadable)?;
            let (size, sha256) = sha256_file(&real).map_err(|_| TrustError::Unreadable)?;
            Ok(FilePin {
                path: real.to_string_lossy().into_owned(),
                size,
                sha256,
            })
        })
        .collect()
}

/// Record the engine's entry point as it is NOW (called right after the install flow finished).
///
/// # Errors
/// The engine cannot be found or its files cannot be read.
pub fn pin_installed(engine: &str, env: &PathEnv) -> Result<Vec<FilePin>, TrustError> {
    let resolved = resolve(engine, env).map_err(|e| match e {
        StartError::NotInstalled => TrustError::NotInstalled,
        _ => TrustError::Unreadable,
    })?;
    current_files(&resolved)
}

/// System locations that a bot working as the service user cannot write to.
fn system_location(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    [
        "/usr/bin/",
        "/usr/local/bin/",
        "/usr/lib/",
        "/usr/local/lib/",
        "/bin/",
        "/opt/",
        "/snap/",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
        || lower.contains(":/program files")
        || lower.contains(":/windows/")
}

/// Is the executable the login would run the one that was installed (or a system install)?
/// With a pin, the search must still resolve to the same real files with the same bytes: a
/// file that shadows or replaces it is refused. Without one, only system locations pass.
///
/// # Errors
/// See [`TrustError`].
pub fn verify_engine(
    engine: &str,
    env: &PathEnv,
    pinned: Option<&[FilePin]>,
) -> Result<(), TrustError> {
    let resolved = resolve(engine, env).map_err(|e| match e {
        StartError::NotInstalled => TrustError::NotInstalled,
        _ => TrustError::Unreadable,
    })?;
    let now = current_files(&resolved)?;
    match pinned {
        Some(pins) if !pins.is_empty() => {
            if pins == now.as_slice() {
                Ok(())
            } else {
                Err(TrustError::FilesChanged)
            }
        }
        _ => {
            if now.iter().all(|f| system_location(&f.path)) {
                Ok(())
            } else {
                Err(TrustError::UntrustedLocation)
            }
        }
    }
}

/// Start the registry's login command. `env` decides where the CLI finds its home: the sanitized
/// environment keeps HOME / USERPROFILE of the service user, so the credential lands where the
/// engine's turns will read it. The executable is verified against `pinned` (see
/// [`verify_engine`]) immediately before it is spawned.
///
/// # Errors
/// See [`StartError`].
pub fn start(
    spec: &'static LoginSpec,
    env: &PathEnv,
    pinned: Option<&[FilePin]>,
) -> Result<LoginRun, StartError> {
    verify_engine(spec.engine, env, pinned).map_err(|e| match e {
        TrustError::NotInstalled => StartError::NotInstalled,
        TrustError::FilesChanged => StartError::FilesChanged,
        TrustError::UntrustedLocation => StartError::UntrustedLocation,
        TrustError::Unreadable => StartError::SpawnFailed,
    })?;
    let resolved = resolve(spec.engine, env)?;
    match spec.transport {
        Transport::Pipe => start_pipe(spec, env, &resolved),
        Transport::Pty => start_pty(spec, env, &resolved),
    }
}

fn start_pipe(
    spec: &'static LoginSpec,
    env: &PathEnv,
    resolved: &crate::detect::ResolvedCommand,
) -> Result<LoginRun, StartError> {
    // Before the CLI runs: a file it writes later is then a change against this.
    let baseline = credential_stamps(spec, env);
    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.args(&resolved.args)
        .args(spec.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    apply_sanitized_env(&mut cmd, env);
    cmd.env("NO_COLOR", "1").env("TERM", "dumb");
    for (key, value) in spec.env {
        cmd.env(key, value);
    }
    #[cfg(windows)]
    let wrap = isolate(cmd, crate::detect::SharedJob::new());
    #[cfg(not(windows))]
    let wrap = isolate(cmd);
    let mut wrap = wrap;
    let mut child = ChildTree::new(wrap.spawn().map_err(|_| StartError::SpawnFailed)?);
    let stdin = child.stdin().take();
    let stdout = child.stdout().take().ok_or(StartError::SpawnFailed)?;
    let stderr = child.stderr().take().ok_or(StartError::SpawnFailed)?;
    let (tx, rx) = mpsc::channel(64);
    spawn_reader(stdout, tx.clone());
    spawn_reader(stderr, tx);
    Ok(LoginRun {
        inner: Inner::Pipe(PipeRun {
            child,
            stdin,
            rx,
            output: String::new(),
            closed_streams: 0,
        }),
        script_done: spec.script.len(),
        baseline,
    })
}

/// The environment of a login on a pseudo terminal: the sanitized one (the service user's home,
/// the trusted `PATH`) plus what the spec adds. No `DISPLAY`, so nothing opens a browser on the
/// server's own screen; `BROWSER` points at a command that does nothing.
fn pty_env(spec: &LoginSpec, env: &PathEnv) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut probe = tokio::process::Command::new("unused");
    apply_sanitized_env(&mut probe, env);
    let mut out: Vec<(std::ffi::OsString, std::ffi::OsString)> = probe
        .as_std()
        .get_envs()
        .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
        .collect();
    let mut set = |key: &str, value: &str| {
        out.retain(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case(key));
        out.push((key.into(), value.into()));
    };
    set("NO_COLOR", "1");
    set("TERM", "xterm-256color");
    if cfg!(unix) {
        set("BROWSER", "false");
    }
    for (key, value) in spec.env {
        set(key, value);
    }
    out
}

/// The service user's home folder: the detector's, else the `HOME` / `USERPROFILE` it carries.
fn home_of(env: &PathEnv) -> Option<std::path::PathBuf> {
    env.home_dir.clone().or_else(|| {
        ["HOME", "USERPROFILE"]
            .iter()
            .find_map(|k| env.env_vars.get(*k))
            .map(std::path::PathBuf::from)
    })
}

fn start_pty(
    spec: &'static LoginSpec,
    env: &PathEnv,
    resolved: &crate::detect::ResolvedCommand,
) -> Result<LoginRun, StartError> {
    let baseline = credential_stamps(spec, env);
    let mut args: Vec<std::ffi::OsString> = resolved.args.iter().map(Into::into).collect();
    args.extend(spec.args.iter().map(Into::into));
    let session = PtySession::spawn(&SpawnSpec {
        program: resolved.program.clone().into_os_string(),
        args,
        cwd: home_of(env),
        env: pty_env(spec, env),
        // Wide, so a long sign-in link is not wrapped by the terminal.
        cols: pty_session::MAX_COLS,
        rows: 40,
    })
    .map_err(|_| StartError::SpawnFailed)?;
    Ok(LoginRun {
        inner: Inner::Pty(session),
        script_done: 0,
        baseline,
    })
}

fn spawn_reader(
    mut reader: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    tx: mpsc::Sender<Event>,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Event::Bytes(buf[..n].to_vec())).await.is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Event::Closed).await;
    });
}

/// Size and modification time of each credential file that exists.
type Stamps = HashMap<PathBuf, (SystemTime, u64)>;

/// Where the spec's credential files can be for the service user (every candidate: the CLI may
/// follow an XDG or `%LOCALAPPDATA%` setting this process does not see).
fn credential_paths(spec: &LoginSpec, env: &PathEnv) -> Vec<PathBuf> {
    let home = home_of(env);
    let under_home = |parts: &[&str]| {
        home.as_ref()
            .map(|h| parts.iter().fold(h.clone(), |p, part| p.join(part)))
    };
    let mut out = Vec::new();
    for file in spec.credentials {
        let roots: Vec<Option<PathBuf>> = match file.root {
            Root::Home => vec![home.clone()],
            Root::DataHome => vec![
                env.xdg_data_home.clone().filter(|p| p.is_absolute()),
                under_home(&[".local", "share"]),
            ],
            Root::LocalAppData => vec![
                env.local_app_data.clone().filter(|p| p.is_absolute()),
                under_home(&["AppData", "Local"]),
            ],
        };
        for root in roots.into_iter().flatten() {
            let path = file.path.iter().fold(root, |p, part| p.join(part));
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// A credential file's state, or `None` unless it is a regular, non-empty file. `symlink_metadata`
/// does not follow a link, so a link to some other, fresher file is not a credential.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    (meta.file_type().is_file() && meta.len() > 0).then_some((meta.modified().ok()?, meta.len()))
}

fn credential_stamps(spec: &LoginSpec, env: &PathEnv) -> Stamps {
    credential_paths(spec, env)
        .into_iter()
        .filter_map(|path| Some((stamp(&path)?, path)))
        .map(|(st, path)| (path, st))
        .collect()
}

fn credential_changed(spec: &LoginSpec, env: &PathEnv, baseline: &Stamps) -> bool {
    credential_stamps(spec, env)
        .iter()
        .any(|(path, now)| baseline.get(path) != Some(now))
}

impl PipeRun {
    fn push(&mut self, bytes: &[u8]) {
        if self.output.len() < MAX_OUTPUT {
            self.output.push_str(&String::from_utf8_lossy(bytes));
        }
    }
}

impl LoginRun {
    /// Wait up to `wait` for more output and return what to show, if it is there yet.
    /// `None` after the timeout means "nothing new"; the output is kept private.
    pub async fn next_prompt(&mut self, spec: &LoginSpec, wait: Duration) -> Option<Prompt> {
        let deadline = tokio::time::Instant::now() + wait;
        match &mut self.inner {
            Inner::Pipe(run) => {
                if let Some(p) = parse_prompt(spec, &run.output) {
                    return Some(p);
                }
                while run.closed_streams < 2 {
                    match tokio::time::timeout_at(deadline, run.rx.recv()).await {
                        Ok(Some(Event::Bytes(b))) => {
                            run.push(&b);
                            if let Some(p) = parse_prompt(spec, &run.output) {
                                return Some(p);
                            }
                        }
                        Ok(Some(Event::Closed)) => run.closed_streams += 1,
                        Ok(None) => run.closed_streams = 2,
                        Err(_) => return None,
                    }
                }
                None
            }
            Inner::Pty(session) => {
                loop {
                    // Menu answers first (they lead to the link), one per pass.
                    if let Some(step) = spec.script.get(self.script_done)
                        && session.text().to_ascii_lowercase().contains(step.wait_for)
                        && session.write(step.send.as_bytes()).is_ok()
                    {
                        self.script_done += 1;
                        // Let the CLI redraw before the next step.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                    if let Some(p) = parse_prompt(spec, &session.text()) {
                        return Some(p);
                    }
                    if session.exit().is_some() || tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                    let seen = session.received();
                    session
                        .changed(
                            seen,
                            Duration::from_millis(500).min(deadline - tokio::time::Instant::now()),
                        )
                        .await;
                }
            }
        }
    }

    /// The captured output (private: used for failure classification and for the account name,
    /// never forwarded as is).
    #[must_use]
    pub fn private_output(&self) -> String {
        match &self.inner {
            Inner::Pipe(run) => run.output.clone(),
            Inner::Pty(session) => session.text(),
        }
    }

    /// Hand one pasted line to the process. The code is not stored.
    ///
    /// # Errors
    /// The pipe or terminal is gone.
    pub async fn send_line(&mut self, code: &str) -> std::io::Result<()> {
        match &mut self.inner {
            Inner::Pipe(run) => {
                let stdin = run
                    .stdin
                    .as_mut()
                    .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
                stdin.write_all(code.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            Inner::Pty(session) => {
                // A terminal program reads a line when it gets Enter (CR), not LF.
                let mut line = Vec::with_capacity(code.len() + 1);
                line.extend_from_slice(code.as_bytes());
                line.push(b'\r');
                session.write(&line)
            }
        }
    }

    /// Drain output (bounded) until the process exits; its exit code.
    pub async fn wait_exit(&mut self, limit: Duration) -> Option<Option<i32>> {
        let deadline = tokio::time::Instant::now() + limit;
        match &mut self.inner {
            Inner::Pipe(run) => loop {
                tokio::select! {
                    status = run.child.wait() => return status.ok().map(|s| s.code()),
                    ev = run.rx.recv() => {
                        if let Some(Event::Bytes(b)) = ev {
                            run.push(&b);
                        }
                    }
                    () = tokio::time::sleep_until(deadline) => return None,
                }
            },
            Inner::Pty(session) => loop {
                if let Some(info) = session.exit() {
                    // The last lines can still be on their way.
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    return Some(info.code);
                }
                if tokio::time::Instant::now() >= deadline {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            },
        }
    }

    /// Take the credential files' state as the baseline: call it when the sign-in page is shown,
    /// so that whatever the CLI wrote while starting up (a refreshed token, a migration) is not
    /// taken for the sign-in.
    pub fn watch_credentials(&mut self, spec: &LoginSpec, env: &PathEnv) {
        self.baseline = credential_stamps(spec, env);
    }

    /// Has one of the spec's credential files appeared or changed since the baseline? Existence,
    /// size and modification time only; the content is never opened, and a symlink never counts.
    #[must_use]
    pub fn credential_written(&self, spec: &LoginSpec, env: &PathEnv) -> bool {
        credential_changed(spec, env, &self.baseline)
    }

    /// Kill the whole process tree and reap it.
    pub async fn terminate(&mut self) {
        match &mut self.inner {
            Inner::Pipe(run) => {
                run.stdin = None;
                run.child.terminate().await;
            }
            Inner::Pty(session) => session.kill(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_fixed_argv_with_no_shell_and_no_client_input() {
        // (see also the pin tests below)
        for spec in LOGIN_SPECS {
            assert!(crate::engine_spec(spec.engine).is_some(), "{}", spec.engine);
            // Antigravity has no login command: its first run signs in.
            assert!(
                !spec.args.is_empty() || spec.engine == "agy",
                "{}",
                spec.engine
            );
            for arg in spec.args {
                assert!(
                    arg.bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'_'),
                    "{arg}"
                );
            }
            assert_ne!(spec.pages.len(), 0);
        }
        // One entry per engine.
        let mut ids: Vec<_> = LOGIN_SPECS.iter().map(|s| s.engine).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), LOGIN_SPECS.len());
    }

    #[test]
    fn a_device_flow_shows_only_the_openai_page_and_a_well_formed_code() {
        let spec = login_spec("codex").unwrap();
        let out = "Welcome\n\x1b[1mhttps://auth.openai.com/codex/device\x1b[0m\nEnter this one-time code\n  AB12-CD34\n";
        assert_eq!(
            parse_prompt(spec, out),
            Some(Prompt::Page {
                url: "https://auth.openai.com/codex/device".into(),
                code: Some("AB12-CD34".into())
            })
        );
        // A half-printed code (no newline yet) is never read; other pages are never shown.
        assert_eq!(
            parse_prompt(spec, "https://auth.openai.com/codex/device\nAB12-CD"),
            None
        );
        assert_eq!(
            parse_prompt(spec, "https://evil.example/codex/device\nAB12-CD34\n"),
            None
        );
        assert_eq!(
            parse_prompt(
                spec,
                "https://auth.openai.com/oauth?token=secret\nAB12-CD34\n"
            ),
            None
        );
    }

    #[test]
    fn a_paste_flow_shows_only_a_vendor_link_without_userinfo() {
        let spec = login_spec("claude").unwrap();
        let ok = "Opening browser\nIf the browser did not open, visit: https://claude.com/cai/oauth/authorize?code=true&state=abc\nPaste code here > ";
        match parse_prompt(spec, ok) {
            Some(Prompt::Page { url, code: None }) => {
                assert!(url.starts_with("https://claude.com/cai/oauth/authorize"));
            }
            other => panic!("{other:?}"),
        }
        for bad in [
            "visit: https://evil.example/oauth/authorize?code=true\n",
            "visit: https://user:pw@claude.com/x\n",
            "visit: http://claude.com/x\n",
            "visit: https://claude.com.evil.example/x\n",
        ] {
            assert_eq!(parse_prompt(spec, bad), None, "{bad}");
        }
    }

    #[test]
    fn a_pasted_code_is_one_bounded_line_of_plain_characters() {
        assert_eq!(valid_code("  GOODCODE#abc  "), Some("GOODCODE#abc"));
        for bad in [
            "short",
            "has space inside",
            "line\nbreak",
            "x;rm -rf",
            &"a".repeat(MAX_CODE + 1),
        ] {
            assert_eq!(valid_code(bad), None, "{bad}");
        }
    }
}

/// One parser test per CLI, fed with the output of that CLI as a terminal shows it (colours,
/// carriage returns and all). The samples for kimi and qoder follow the strings of those CLIs'
/// own source (npm 2.8.1 and 1.1.99); the others follow the vendor docs.
#[cfg(test)]
mod per_cli_tests {
    use super::*;

    fn page(engine: &str, output: &str) -> Option<(String, Option<String>)> {
        parse_prompt(login_spec(engine).expect(engine), output)
            .map(|Prompt::Page { url, code }| (url, code))
    }

    #[test]
    fn every_engine_of_the_brief_has_a_flow() {
        for engine in [
            "claude", "codex", "cursor", "agy", "opencode", "kiro", "qoder", "kimi",
        ] {
            assert!(login_spec(engine).is_some(), "{engine}");
        }
        // The terminal ones really run on a PTY; the two with a real non-interactive mode do not.
        assert_eq!(login_spec("claude").unwrap().transport, Transport::Pipe);
        assert_eq!(login_spec("codex").unwrap().transport, Transport::Pipe);
        for engine in ["cursor", "agy", "opencode", "kiro", "qoder", "kimi"] {
            assert_eq!(login_spec(engine).unwrap().transport, Transport::Pty);
        }
    }

    #[test]
    fn cursor_shows_its_login_link_and_needs_no_code() {
        let out = "\x1b[2mOpening browser to log in...\x1b[0m\nIf it does not open, visit:\n  https://cursor.com/loginDeepControl?challenge=abc123&uuid=u-1&mode=login&redirectTarget=cli\nWaiting for login...\n";
        let (url, code) = page("cursor", out).expect("link");
        assert!(
            url.starts_with("https://cursor.com/loginDeepControl?challenge=abc123"),
            "{url}"
        );
        assert!(
            url.contains("uuid=u-1"),
            "the query carries the sign-in state: {url}"
        );
        assert_eq!(code, None);
        // Not before the line is complete, and never a foreign host.
        assert_eq!(page("cursor", "visit https://cursor.com/login"), None);
        assert_eq!(
            page("cursor", "visit https://cursor.com.evil.example/login\n"),
            None
        );
        assert_eq!(
            page("cursor", "visit https://evil.example/cursor.com\n"),
            None
        );
    }

    #[test]
    fn antigravity_shows_a_google_link_and_waits_for_a_pasted_code() {
        let out = "Welcome to Antigravity\r\nSign in to continue:\r\nhttps://accounts.google.com/o/oauth2/v2/auth?client_id=x&response_type=code&scope=openid&state=s1\r\nEnter the authorization code: ";
        let (url, code) = page("agy", out).expect("link");
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{url}"
        );
        assert_eq!(code, None);
        assert_eq!(login_spec("agy").unwrap().kind, FlowKind::PasteCode);
        assert_eq!(
            page("agy", "https://accounts.evil.example/o/oauth2\n"),
            None
        );
        // The token file the sign-in writes is what confirms it (never the folder around it).
        assert!(!login_spec("agy").unwrap().credentials.is_empty());
    }

    #[test]
    fn opencode_answers_its_login_method_menu_and_shows_the_console_link() {
        // The menu as opencode 2.3.0 draws it (read on a PTY): nothing to show yet.
        let menu = "T  Connect an integration\n|\n*  Select login method\n|  > OpenCode Console account\n|    API key (service account)\n";
        assert_eq!(page("opencode", menu), None);
        let spec = login_spec("opencode").unwrap();
        assert_eq!(spec.args, ["auth", "login", "opencode"]);
        assert_eq!(spec.script.len(), 1);
        assert!(menu.to_ascii_lowercase().contains(spec.script[0].wait_for));
        assert_eq!(spec.script[0].send, "\r");
        // After Enter it prints the console link (and a code, when it has one).
        let out = format!(
            "{menu}\nOpen https://opencode.ai/auth/device in your browser\nEnter code: ABCD-5678\n"
        );
        let (url, code) = page("opencode", &out).expect("link");
        assert_eq!(url, "https://opencode.ai/auth/device");
        assert_eq!(code.as_deref(), Some("ABCD-5678"));
        let (_, none) = page("opencode", "Open https://opencode.ai/auth\n").expect("link");
        assert_eq!(none, None);
        assert_eq!(page("opencode", "see https://example.com/auth\n"), None);
    }

    #[test]
    fn kiro_shows_the_device_page_and_the_code() {
        let out = "\x1b[1mConfirm the following code in the browser\x1b[0m\r\nCode: ABCD-1234\r\n\r\nOpen this URL: https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-1234\r\n";
        let (url, code) = page("kiro", out).expect("link");
        assert_eq!(
            url,
            "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-1234"
        );
        assert_eq!(code.as_deref(), Some("ABCD-1234"));
        // A link without a code is not shown for a device flow (the person could not finish).
        assert_eq!(
            page("kiro", "Open this URL: https://view.awsapps.com/start\n"),
            None
        );
        assert_eq!(
            page("kiro", "Code: ABCD-1234\nhttps://aws.evil.example/device\n"),
            None
        );
        // A relative of the host name is not the host.
        assert_eq!(
            page("kiro", "Code: ABCD-1234\nhttps://notamazonaws.com/x\n"),
            None
        );
    }

    #[test]
    fn qoder_shows_its_link_and_then_names_the_account() {
        let out = "Starting browser login...\n  https://qoder.com/device/selectAccounts?nonce=n1&challenge=c1\n\nWaiting for login in browser...\n";
        let (url, code) = page("qoder", out).expect("link");
        assert!(
            url.starts_with("https://qoder.com/device/selectAccounts?"),
            "{url}"
        );
        assert_eq!(code, None);
        let done = format!("{out}\nLogin successful! Welcome, ada@example.com.\n");
        assert_eq!(parse_account(&done).as_deref(), Some("ada@example.com"));
        assert!(printed_success(login_spec("qoder").unwrap(), &done));
        let again = "Already logged in as ada.\n";
        assert_eq!(parse_account(again).as_deref(), Some("ada"));
        assert!(printed_success(login_spec("qoder").unwrap(), again));
        assert!(!printed_success(login_spec("qoder").unwrap(), out));
        assert_eq!(page("qoder", "https://qoder.com.evil.example/x\n"), None);
    }

    #[test]
    fn kimi_shows_the_device_link_and_code_and_confirms_with_its_own_line() {
        let out = "\nOpening browser for Kimi device login: https://www.kimi.com/code/authorize_device?user_code=WXYZ-5678\nIf the browser did not open, paste the URL above and enter code: WXYZ-5678\nCode expires in 600s.\nWaiting for authorization to complete…\n";
        let (url, code) = page("kimi", out).expect("link");
        assert_eq!(
            url,
            "https://www.kimi.com/code/authorize_device?user_code=WXYZ-5678"
        );
        assert_eq!(code.as_deref(), Some("WXYZ-5678"));
        // A code without a dash, after the label, is read too.
        let plain = "Opening browser for Kimi device login: https://www.kimi.ai/code/authorize_device\nIf the browser did not open, paste the URL above and enter code: ABCD2345\n";
        assert_eq!(page("kimi", plain).unwrap().1.as_deref(), Some("ABCD2345"));
        assert!(printed_success(
            login_spec("kimi").unwrap(),
            "Logged in to Kimi Code.\n"
        ));
        assert!(!printed_success(login_spec("kimi").unwrap(), out));
        assert_eq!(parse_account("Logged in to Kimi Code.\n"), None);
        assert_eq!(
            page(
                "kimi",
                "enter code: ABCD2345\nhttps://evil.example/kimi.com\n"
            ),
            None
        );
    }

    #[test]
    fn codex_keeps_its_strict_rules() {
        // The fixed OpenAI page still wins over anything else a CLI prints.
        let out = "https://auth.openai.com/codex/device?x=1\nAB12-CD34\n";
        assert_eq!(page("codex", out), None);
        let ok = "https://auth.openai.com/codex/device\nAB12-CD34\n";
        assert_eq!(page("codex", ok).unwrap().1.as_deref(), Some("AB12-CD34"));
    }

    #[test]
    fn a_label_alone_is_not_a_code() {
        assert_eq!(device_code("Enter the code: please"), None);
        assert_eq!(device_code("no code here"), None);
        assert_eq!(
            device_code("enter code: ABCD2345").as_deref(),
            Some("ABCD2345")
        );
        assert_eq!(device_code("  WXYZ-5678  ").as_deref(), Some("WXYZ-5678"));
    }

    #[test]
    fn the_account_is_bounded_plain_text() {
        assert_eq!(parse_account("nothing to see\n"), None);
        let long = format!("Logged in as {}\n", "a".repeat(400));
        assert_eq!(parse_account(&long), None);
        assert_eq!(
            parse_account("\x1b[32mLogged in as owner@example.test\x1b[0m\n").as_deref(),
            Some("owner@example.test")
        );
    }

    #[test]
    fn a_pasted_code_may_carry_a_slash_like_googles() {
        assert_eq!(valid_code("4/0AbCdEf_gh-123"), Some("4/0AbCdEf_gh-123"));
        assert_eq!(valid_code("4/0AbC dEf_gh-123"), None);
    }
}

/// The sign-in page and the success line are read from a program's output: only what the vendor's
/// own pages look like may pass, whoever wrote the text.
#[cfg(test)]
mod hardening_tests {
    use super::*;

    fn shown(engine: &str, output: &str) -> Option<String> {
        parse_prompt(login_spec(engine).expect(engine), output).map(|Prompt::Page { url, .. }| url)
    }

    #[test]
    fn a_shared_hosting_name_is_not_a_sign_in_host() {
        let code = "Code: ABCD-1234\n";
        // Kiro: any bucket, API, tenant or look-alike under the AWS names is somebody's page.
        for bad in [
            "https://evil.s3.amazonaws.com/device",
            "https://attacker.execute-api.us-east-1.amazonaws.com/device",
            "https://ec2-1-2-3-4.compute-1.amazonaws.com/",
            "https://amazonaws.com/",
            "https://device.sso.evil.amazonaws.com/",
            "https://x.device.sso.us-east-1.amazonaws.com/",
            "https://device.sso.us-east-1.amazonaws.com.evil.example/",
            "https://device.sso.us-east-1.amazonaws.com:8443/",
            "https://evil.awsapps.com/start",
            "https://view.awsapps.com/other",
            "https://www.amazon.com/",
            "http://device.sso.us-east-1.amazonaws.com/",
        ] {
            assert_eq!(shown("kiro", &format!("{code}{bad}\n")), None, "{bad}");
        }
        for good in [
            "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-1234",
            "https://device.sso.eu-central-1.amazonaws.com/",
            "https://view.awsapps.com/start",
        ] {
            assert_eq!(
                shown("kiro", &format!("{code}{good}\n")).as_deref(),
                Some(good),
                "{good}"
            );
        }
        // Antigravity: Google's OAuth page only, not `google.com` (user sites, open redirects).
        for bad in [
            "https://sites.google.com/view/phish",
            "https://www.google.com/url?q=https://evil.example",
            "https://google.com/",
            "https://accounts.google.com/ServiceLogin?continue=https://evil.example",
            "https://evil.accounts.google.com/o/oauth2/v2/auth",
            "https://accounts.google.com.evil.example/o/oauth2/v2/auth",
            "https://accounts.google.com/o/oauth2evil",
            "https://antigravity.google/docs",
        ] {
            assert_eq!(shown("agy", &format!("{bad}\n")), None, "{bad}");
        }
        assert!(
            shown(
                "agy",
                "https://accounts.google.com/o/oauth2/v2/auth?client_id=x&state=s\n"
            )
            .is_some()
        );
    }

    #[test]
    fn every_engine_checks_the_exact_host_and_the_path() {
        for (engine, bad) in [
            ("claude", "https://evil.claude.com/oauth/authorize"),
            ("claude", "https://claude.com/other/page"),
            ("claude", "https://claude.com/oauth-evil"),
            ("claude", "https://claude.com.evil.example/oauth/authorize"),
            ("cursor", "https://evil.cursor.com/loginDeepControl"),
            ("cursor", "https://cursor.com/pricing"),
            ("cursor", "https://cursor.com/loginDeepControlX"),
            ("cursor", "https://cursor.sh/loginDeepControl"),
            ("opencode", "https://evil.opencode.ai/auth"),
            ("opencode", "https://opencode.ai/docs"),
            ("opencode", "https://opencode.ai/authx"),
            ("qoder", "https://evil.qoder.com/device/x"),
            ("qoder", "https://qoder.com/blog"),
            ("kimi", "https://evil.kimi.com/code/x"),
            ("kimi", "https://www.kimi.com/pricing"),
        ] {
            assert_eq!(
                shown(engine, &format!("see {bad}\n")),
                None,
                "{engine} {bad}"
            );
        }
        for (engine, good) in [
            ("claude", "https://claude.com/cai/oauth/authorize?code=true"),
            ("claude", "https://platform.claude.com/oauth/authorize?x=1"),
            ("cursor", "https://cursor.com/loginDeepControl?challenge=c"),
            ("opencode", "https://opencode.ai/auth/device"),
            ("qoder", "https://qoder.com/device/selectAccounts?n=1"),
        ] {
            assert_eq!(
                shown(engine, &format!("see {good}\n")).as_deref(),
                Some(good),
                "{engine}"
            );
        }
    }

    #[test]
    fn the_first_valid_page_wins() {
        let spec = login_spec("claude").unwrap();
        let first = "https://claude.com/cai/oauth/authorize?state=one";
        let later = "https://claude.ai/oauth/authorize?state=two";
        let out = format!("visit {first}\nerror, try again: {later}\n");
        let Some(Prompt::Page { url, .. }) = parse_prompt(spec, &out) else {
            panic!("no page")
        };
        assert_eq!(url, first);
        // A page that is not valid never counts, in either position.
        let out =
            format!("see https://evil.example/x\nvisit {first}\nsee https://evil.example/y\n");
        let Some(Prompt::Page { url, .. }) = parse_prompt(spec, &out) else {
            panic!("no page")
        };
        assert_eq!(url, first);
    }

    #[test]
    fn aws_regions_are_region_names() {
        for ok in [
            "us-east-1",
            "eu-central-2",
            "ap-southeast-3",
            "us-gov-west-1",
            "il-central-1",
        ] {
            assert!(is_aws_region(ok), "{ok}");
        }
        for bad in [
            "",
            "evil",
            "us-east",
            "us-east-x",
            "US-EAST-1",
            "us--1",
            "-east-1",
            "a-b-1",
            "us-east-123",
            "us.east.1",
            "us-east-1.evil",
        ] {
            assert!(!is_aws_region(bad), "{bad}");
        }
    }

    #[test]
    fn a_denial_is_not_a_success_line() {
        for (engine, text) in [
            ("kimi", "Logged in to Kimi Code.\n"),
            ("kiro", "Logged in\n"),
            ("qoder", "Login successful! Welcome, ada.\n"),
            ("qoder", "Already logged in as ada.\n"),
            ("opencode", "Login successful\n"),
            ("opencode", "Hello, no problem. Logged in\n"),
            ("kiro", "\x1b[32mLogged in\x1b[0m\n"),
        ] {
            assert!(
                printed_success(login_spec(engine).unwrap(), text),
                "{engine} {text:?}"
            );
        }
        for (engine, text) in [
            ("kiro", "You are not logged in\n"),
            ("kiro", "Not logged in. Run `kiro-cli login`.\n"),
            ("kiro", "Error: you aren't logged in\n"),
            ("kiro", "You are no longer logged in\n"),
            ("kiro", "You are never logged in\n"),
            ("kiro", "Could not tell: user isn\u{2019}t logged in\n"),
            ("kimi", "Error: not logged in to Kimi\n"),
            ("kimi", "NOT LOGGED IN TO KIMI\n"),
            ("opencode", "Status: not logged in\n"),
            ("opencode", "Failed: logged in state could not be read\n"),
            ("qoder", "You are not already logged in\n"),
            ("kiro", "unlogged in\n"),
        ] {
            assert!(
                !printed_success(login_spec(engine).unwrap(), text),
                "{engine} {text:?}"
            );
        }
    }

    fn env_in(home: &Path) -> PathEnv {
        let mut env = PathEnv::from_ambient();
        env.home_dir = Some(home.to_path_buf());
        env.env_vars.clear();
        env.xdg_data_home = None;
        env.local_app_data = None;
        env
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn only_a_credential_file_that_changed_after_the_baseline_counts() {
        let home = tempfile::tempdir().unwrap();
        let env = env_in(home.path());
        let spec = login_spec("opencode").unwrap();
        let auth = home.path().join(".local/share/opencode/auth.json");
        // An old file that was there before the run is not a sign-in...
        write(&auth, "{\"old\":1}");
        let baseline = credential_stamps(spec, &env);
        assert!(!credential_changed(spec, &env, &baseline), "at t=0");
        // ...an empty one is not either...
        write(&auth, "");
        assert!(!credential_changed(spec, &env, &baseline), "empty");
        // ...a rewritten one is.
        write(&auth, "{\"opencode\":{\"type\":\"oauth\"}}");
        assert!(credential_changed(spec, &env, &baseline), "rewritten");
        // A new baseline (the page was shown) starts again.
        let baseline = credential_stamps(spec, &env);
        assert!(!credential_changed(spec, &env, &baseline));
        // A file that did not exist before appears.
        let fresh = tempfile::tempdir().unwrap();
        let env2 = env_in(fresh.path());
        let baseline = credential_stamps(spec, &env2);
        assert!(!credential_changed(spec, &env2, &baseline));
        write(&fresh.path().join(".local/share/opencode/auth.json"), "x");
        assert!(credential_changed(spec, &env2, &baseline));
    }

    #[test]
    fn the_xdg_and_app_data_folders_are_looked_in_too() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let spec = login_spec("opencode").unwrap();
        let mut env = env_in(home.path());
        env.xdg_data_home = Some(other.path().join("xdg"));
        env.local_app_data = Some(other.path().join("local"));
        let baseline = credential_stamps(spec, &env);
        write(&other.path().join("xdg/opencode/auth.json"), "x");
        assert!(credential_changed(spec, &env, &baseline), "XDG_DATA_HOME");
        let baseline = credential_stamps(spec, &env);
        write(&other.path().join("local/opencode/auth.json"), "x");
        assert!(credential_changed(spec, &env, &baseline), "LOCALAPPDATA");
        // A relative setting is not trusted.
        let mut env = env_in(home.path());
        env.xdg_data_home = Some(PathBuf::from("relative"));
        assert!(credential_paths(spec, &env).iter().all(|p| p.is_absolute()));
    }

    #[test]
    fn a_folder_full_of_settings_is_not_a_credential() {
        let home = tempfile::tempdir().unwrap();
        let env = env_in(home.path());
        let spec = login_spec("agy").unwrap();
        let baseline = credential_stamps(spec, &env);
        let dir = home.path().join(".gemini/antigravity-cli");
        for name in [
            "settings.json",
            "cache/projects.json",
            "needs-auth-cache.json",
            "conversations/c1",
        ] {
            write(&dir.join(name), "{}");
        }
        assert!(!credential_changed(spec, &env, &baseline));
        write(&dir.join("authstore.enc"), "token");
        assert!(credential_changed(spec, &env, &baseline));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_credential_file_never_counts() {
        let home = tempfile::tempdir().unwrap();
        let env = env_in(home.path());
        let spec = login_spec("opencode").unwrap();
        let baseline = credential_stamps(spec, &env);
        // A link to a fresh, non-empty file elsewhere.
        let target = home.path().join("elsewhere.json");
        write(&target, "fresh");
        let auth = home.path().join(".local/share/opencode/auth.json");
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &auth).unwrap();
        assert!(!credential_changed(spec, &env, &baseline), "link to a file");
        std::fs::remove_file(&auth).unwrap();
        std::os::unix::fs::symlink(home.path(), &auth).unwrap();
        assert!(
            !credential_changed(spec, &env, &baseline),
            "link to a folder"
        );
        std::fs::remove_file(&auth).unwrap();
        std::os::unix::fs::symlink(home.path().join("missing"), &auth).unwrap();
        assert!(!credential_changed(spec, &env, &baseline), "dangling link");
        // A folder where the file belongs is nothing either.
        std::fs::remove_file(&auth).unwrap();
        std::fs::create_dir_all(&auth).unwrap();
        assert!(!credential_changed(spec, &env, &baseline), "folder");
    }
}

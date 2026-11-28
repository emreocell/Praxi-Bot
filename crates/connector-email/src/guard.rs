//! Outgoing-mail guardrails (ADR-0035, item ML-1). All of it lives in code, outside the model.
//!
//! * a **send log** (who, when, which bot, message id) the owner can open and export;
//! * an **opt-out list** (addresses and whole domains) for people who asked not to be written to;
//! * an **opt-out line** appended in code to the body (the model never writes or removes it);
//! * a **daily count** per account and a **same-recipient wait**.
//!
//! The count, the wait and the opt-out line are *soft* guards on Ask-level bots: [`MailGuard::check`]
//! returns [`Warning`]s. Only the owner can send past one, by approving the card that shows it
//! ("Send anyway"). On Full-level bots only the count and wait warnings are logged without blocking;
//! all other warnings, including a dropped opt-out line, block. The tool freezes the card it showed, warnings or not
//! ([`MailGuard::freeze`]) and a later call goes through past a warning only when it matches a
//! frozen entry exactly ([`MailGuard::take_card`]). The model has no argument that can do this.
//!
//! State is a JSON settings file, a JSON opt-out file and an append-only JSONL send log under one
//! directory (or memory only, for tests).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Default: at most this many mails per account in a rolling 24 hours before a warning.
pub const DEFAULT_DAILY_LIMIT: u32 = 20;
/// Default: warn when the same recipient got a mail from the account in the last N days.
pub const DEFAULT_SAME_RECIPIENT_DAYS: u32 = 0;
/// How long a frozen warning set stays valid after the card was shown.
const FREEZE_TTL: Duration = Duration::from_secs(30 * 60);
/// Longest subject kept in the log.
const SUBJECT_KEEP: usize = 120;
/// Longest footer text accepted from settings.
pub const MAX_FOOTER_CHARS: usize = 1000;
/// Most opt-out entries the list may hold.
pub const MAX_OPT_OUTS: usize = 20_000;
/// Most send-log entries kept in memory (and scanned by every check). Older ones stay on disk.
pub const DEFAULT_LOG_MEMORY: usize = 20_000;
/// The active log file is moved aside (`send-log.<unix ts>.<n>.jsonl`) once it is this big.
pub const DEFAULT_LOG_ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// The sentence shown once in a warning that is about opt-out rights.
pub const LEGAL_NOTICE: &str = "Ignoring someone's opt-out on commercial e-mail can be unlawful \
(in Turkey: Law No. 6563 on electronic commerce, ETK) and can lead to fines. \
This is not legal advice.";

/// Default opt-out line (Turkish first, English second). `{sender}` is the sending address.
/// Replies are not read by the program: the owner removes the person by hand (Settings, opt-out
/// list), so the text says so and does not promise an automatic stop.
pub const DEFAULT_FOOTER: &str = "Bu iletiyi almak istemiyorsanız, bu e-postayı \"DURDUR\" yazarak yanıtlayın; kaydınızı {sender} sahibi elle listeden çıkarır (otomatik değildir, birkaç gün sürebilir).
If you do not want to receive e-mail from {sender}, reply with the word \"STOP\"; the owner removes you from the list by hand (it is not automatic and can take a few days).";

/// Owner-changeable values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardSettings {
    /// Warn above this many sends per account in 24 hours. `0` turns the warning off.
    pub daily_limit: u32,
    /// Warn when a recipient got mail from the account within this many days. `0` = off.
    pub same_recipient_days: u32,
    /// Whether the opt-out line is added by default.
    pub opt_out_line: bool,
    /// Text of the opt-out line; `{sender}` is replaced by the sending address. Empty = default.
    pub footer: String,
    /// Extra domains a verification mail may link to (besides the site the sign-up
    /// started on), for sites whose mails go through another domain. A host on one
    /// of them (or below it) counts as followable in `email_wait_for`. Normalised
    /// domain names, owner-set only; empty by default.
    pub verification_link_domains: Vec<String>,
}

/// Most owner-listed verification link domains.
pub const MAX_VERIFICATION_DOMAINS: usize = 50;

impl Default for GuardSettings {
    fn default() -> Self {
        Self {
            daily_limit: DEFAULT_DAILY_LIMIT,
            same_recipient_days: DEFAULT_SAME_RECIPIENT_DAYS,
            opt_out_line: true,
            footer: String::new(),
            verification_link_domains: Vec::new(),
        }
    }
}

impl GuardSettings {
    /// Reject values that cannot work.
    ///
    /// # Errors
    /// A short plain sentence.
    pub fn validate(&self) -> Result<(), String> {
        if self.footer.chars().count() > MAX_FOOTER_CHARS {
            return Err(format!(
                "the opt-out text is too long (at most {MAX_FOOTER_CHARS} characters)"
            ));
        }
        if self.footer.contains('\0') {
            return Err("the opt-out text contains an invalid character".into());
        }
        if self.daily_limit > 100_000 || self.same_recipient_days > 3650 {
            return Err("that limit is unreasonably large".into());
        }
        if self.verification_link_domains.len() > MAX_VERIFICATION_DOMAINS {
            return Err(format!(
                "at most {MAX_VERIFICATION_DOMAINS} verification link domains"
            ));
        }
        for d in &self.verification_link_domains {
            crate::verify::normalize_allowed_domain(d)?;
        }
        Ok(())
    }

    /// The verification link allow-list in its normalised form (entries that do
    /// not normalise are dropped, so a hand-edited file cannot widen anything).
    #[must_use]
    pub fn verification_domains(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .verification_link_domains
            .iter()
            .filter_map(|d| crate::verify::normalize_allowed_domain(d).ok())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn footer_for(&self, sender: &str) -> String {
        let t = if self.footer.trim().is_empty() {
            DEFAULT_FOOTER
        } else {
            self.footer.trim()
        };
        t.replace("{sender}", sender)
    }
}

/// One address or domain the owner will not write to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptOut {
    /// `ann@example.com` or `example.com` (a whole domain), lowercase.
    pub value: String,
    /// `address` or `domain`.
    pub kind: String,
    /// Unix seconds when it was added.
    pub added_at: u64,
    /// Free note from the owner (why / where it came from).
    #[serde(default)]
    pub note: String,
}

/// One line of the send log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendLogEntry {
    /// Unix seconds.
    pub at: u64,
    /// Same moment as UTC text, for people.
    pub at_utc: String,
    /// Bot that sent (`None`: not a bot).
    pub bot: Option<String>,
    /// Run that sent.
    pub run: String,
    /// Account name.
    pub account: String,
    /// Sending address.
    pub from: String,
    /// Every recipient (to, cc and bcc), lowercase.
    pub recipients: Vec<String>,
    /// First characters of the subject.
    pub subject: String,
    /// Provider message id or the server's reply line.
    pub message_id: String,
    /// The opt-out line was in the mail.
    pub opt_out_line: bool,
    /// The owner approved past these warnings ("Send anyway"); empty = none were shown.
    pub overridden: Vec<String>,
}

/// One reason to stop and ask the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    /// `opt_out_listed`, `daily_limit`, `same_recipient` or `no_opt_out_line`.
    pub code: String,
    /// Plain English sentence (the UI shows its own translation by `code`).
    pub text: String,
    /// Addresses it is about (opt-out hit or repeat recipient).
    pub addresses: Vec<String>,
    /// Numbers for the sentence: sends so far / limit / days.
    pub count: u32,
    /// The limit or the number of days.
    pub limit: u32,
}

/// What [`MailGuard::check`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GuardReport {
    /// Warnings, possibly none.
    pub warnings: Vec<Warning>,
    /// Set when a warning is about opt-out rights: the one-time legal sentence.
    pub legal_notice: Option<String>,
    /// The opt-out line that will be appended (`None` when it is off for this mail).
    pub footer: Option<String>,
}

impl GuardReport {
    /// Sorted warning codes.
    #[must_use]
    pub fn codes(&self) -> Vec<String> {
        let mut c: Vec<String> = self.warnings.iter().map(|w| w.code.clone()).collect();
        c.sort();
        c
    }
}

/// A mail as the guard sees it.
#[derive(Debug, Clone)]
pub struct MailFacts<'a> {
    /// Bot that sends.
    pub bot: Option<&'a str>,
    /// Account name.
    pub account: &'a str,
    /// Sending address.
    pub from: &'a str,
    /// Recipients exactly as the SMTP envelope / provider gets them (see [`canonical_address`]).
    pub recipients: &'a [String],
    /// Whether the opt-out line stays on (`false` only when the model asked to drop it).
    pub opt_out_line: bool,
}

/// What [`MailGuard::take_card`] found of the card the owner was shown for a mail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CardCheck {
    /// A card for exactly this mail was shown, and recently enough.
    pub shown: bool,
    /// That card showed warnings and they include every warning the mail has now: the
    /// owner chose to send past them.
    pub overrides: bool,
}

struct Frozen {
    codes: Vec<String>,
    token: String,
    at: Instant,
}

#[derive(Default)]
struct State {
    settings: GuardSettings,
    opt_outs: Vec<OptOut>,
    /// Lookup built from `opt_outs`: the full and the `+tag`-free key of every address entry.
    opt_addrs: HashSet<String>,
    /// Domain entries (each also covers its subdomains).
    opt_domains: Vec<String>,
    log: Vec<SendLogEntry>,
    frozen: HashMap<String, Frozen>,
    /// Saved files that could not be read at start (set aside as `<name>.bad-<ts>`).
    load_errors: Vec<&'static str>,
    /// `opt-out.json` was unreadable AND could not be set aside: saving would destroy it.
    opt_out_locked: bool,
}

impl State {
    fn set_opt_outs(&mut self, list: Vec<OptOut>) {
        self.opt_addrs.clear();
        self.opt_domains.clear();
        for o in &list {
            if o.kind == "domain" {
                self.opt_domains.push(o.value.clone());
            } else {
                // Someone who opted out as `a+t@x.com` is the person behind `a@x.com` too.
                let k = key_of(&o.value);
                self.opt_addrs.insert(k.full);
                self.opt_addrs.insert(k.base);
            }
        }
        self.opt_outs = list;
    }
}

/// The guard. Cheap to share behind an `Arc`.
pub struct MailGuard {
    pub(crate) send_lock: tokio::sync::Mutex<()>,
    dir: Option<PathBuf>,
    state: Mutex<State>,
    log_memory: usize,
    log_rotate_bytes: u64,
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `2026-11-24T10:00:00Z` for `secs`.
#[must_use]
pub fn utc_text(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The keys one mailbox is matched under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressKeys {
    /// `local@domain`: local part lowercase, domain IDNA-ASCII (punycode) lowercase, no trailing dot.
    pub full: String,
    /// `full` with a `+tag` removed from the local part (the same mailbox at most providers).
    pub base: String,
    /// The domain part of `full`.
    pub domain: String,
}

/// Lowercase IDNA-ASCII form of a domain without a trailing dot; `None` when it is not a domain.
#[must_use]
pub fn canonical_domain(d: &str) -> Option<String> {
    let d = d.trim().trim_end_matches('.');
    if d.is_empty() || d.contains(['@', '/', '\\']) || crate::address::has_hidden_char(d) {
        return None;
    }
    idna::domain_to_ascii(d).ok().filter(|x| !x.is_empty())
}

/// The keys of `a@b`, `Name <a@b>` or `"Name" <a@b>`; `None` when there is no usable address.
/// Callers that deliver mail pass the address the SMTP envelope uses (`smtp::envelope_address`),
/// so the guard and the delivery read the same recipient.
#[must_use]
pub fn canonical_address(s: &str) -> Option<AddressKeys> {
    let t = s.trim();
    let inner = match (t.rfind('<'), t.rfind('>')) {
        (Some(a), Some(b)) if a < b => &t[a + 1..b],
        _ => t,
    };
    let inner = inner.trim();
    if crate::address::has_hidden_char(inner) {
        return None;
    }
    let (local, domain) = inner.rsplit_once('@')?;
    let local = local.to_lowercase();
    if local.is_empty() {
        return None;
    }
    let domain = canonical_domain(domain)?;
    let base_local = match local.split_once('+') {
        Some((head, _)) if !head.is_empty() => head,
        _ => local.as_str(),
    };
    Some(AddressKeys {
        base: format!("{base_local}@{domain}"),
        full: format!("{local}@{domain}"),
        domain,
    })
}

/// Whether a domain entry `entry` covers `domain` (itself or any subdomain).
fn domain_covers(entry: &str, domain: &str) -> bool {
    domain == entry
        || domain
            .strip_suffix(entry)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// Normalize an owner-typed opt-out entry (`a@b.c`, `@b.c` or `b.c`) the way recipients are
/// normalized: IDNA domain, lowercase, no trailing dot.
///
/// # Errors
/// A short plain sentence.
pub fn normalize_opt_out(raw: &str) -> Result<(String, &'static str), String> {
    let t = raw.trim();
    if t.is_empty() || t.len() > 254 || t.contains(char::is_whitespace) || t.contains(['<', '>']) {
        return Err(
            "enter one e-mail address (ann@example.com) or one domain (example.com)".into(),
        );
    }
    if let Some(domain) = t.strip_prefix('@') {
        return domain_entry(domain);
    }
    if t.contains('@') {
        let keys = canonical_address(t)
            .filter(|k| k.domain.contains('.'))
            .ok_or("that does not look like an e-mail address")?;
        return Ok((keys.full, "address"));
    }
    domain_entry(t)
}

fn domain_entry(d: &str) -> Result<(String, &'static str), String> {
    let bad = || "that does not look like a domain (example.com)".to_owned();
    let d = d.trim_end_matches('.');
    if d.contains('@')
        || !d.contains('.')
        || d.starts_with('.')
        || !d
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
    {
        return Err(bad());
    }
    let ascii = canonical_domain(d).ok_or_else(bad)?;
    if !ascii.contains('.') || ascii.contains("..") {
        return Err(bad());
    }
    Ok((ascii, "domain"))
}

/// Keys of a stored or planned recipient; a string that is not an address keeps its lowercase self.
fn key_of(r: &str) -> AddressKeys {
    canonical_address(r).unwrap_or_else(|| {
        let l = r.trim().to_lowercase();
        AddressKeys {
            domain: l
                .rsplit_once('@')
                .map_or_else(String::new, |(_, d)| d.to_owned()),
            base: l.clone(),
            full: l,
        }
    })
}

/// The warning shown while a saved guard file could not be read.
fn unreadable_warning(load_errors: &[&'static str]) -> Option<Warning> {
    if load_errors.is_empty() {
        return None;
    }
    Some(Warning {
        code: "guard_unreadable".into(),
        text: format!(
            "The mail guard could not read its saved {} (the damaged file was set aside,              not deleted). Opt-outs may be missing, so nobody is checked against them.",
            load_errors.join(" and ")
        ),
        addresses: load_errors.iter().map(|s| (*s).to_owned()).collect(),
        count: 0,
        limit: 0,
    })
}

const OPT_OUT_LOCKED: &str = "the saved opt-out list could not be read and could not be set aside; \
fix or move opt-out.json in the e-mail guard folder first";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What reading one saved JSON file gave.
enum Loaded<T> {
    Missing,
    Ok(T),
    /// Unreadable; `set_aside` is `false` when the bad file could not be moved away.
    Bad {
        set_aside: bool,
    },
}

/// Read `dir/name`. A file that cannot be read or parsed is renamed to `<name>.bad-<unix ts>`
/// BEFORE anything else happens, so a later save can never overwrite it.
fn load_json<T: serde::de::DeserializeOwned>(
    dir: &std::path::Path,
    name: &str,
    accept: impl Fn(&T) -> bool,
) -> Loaded<T> {
    let path = dir.join(name);
    let parsed = match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str::<T>(&t).ok().filter(|v| accept(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(_) => None,
    };
    if let Some(v) = parsed {
        return Loaded::Ok(v);
    }
    let bad = dir.join(format!("{name}.bad-{}", now_secs()));
    let set_aside = std::fs::rename(&path, &bad).is_ok();
    log::error!(
        "email: {name} could not be read; {}",
        if set_aside {
            "it was set aside as a .bad file"
        } else {
            "it could NOT be set aside, saving is blocked"
        }
    );
    Loaded::Bad { set_aside }
}

/// Parse the send-log lines (a damaged line is skipped) and keep the last `keep` entries.
fn parse_log(text: &str, keep: usize) -> Vec<SendLogEntry> {
    let mut v: Vec<SendLogEntry> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<SendLogEntry>(l).ok())
        .collect();
    if v.len() > keep {
        v.drain(..v.len() - keep);
    }
    v
}

/// The send-log files under `dir`, oldest first: `send-log.<ts>.<n>.jsonl` archives, then the
/// active `send-log.jsonl`.
fn log_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|d| d.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("send-log.")
                    && std::path::Path::new(n)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
            })
        })
        .collect();
    files.sort_by_key(|p| {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        (n == "send-log.jsonl", n.to_owned())
    });
    files
}

fn csv_row(e: &SendLogEntry) -> String {
    fn cell(s: &str) -> String {
        let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            format!("'{s}")
        } else {
            s.to_owned()
        };
        format!("\"{}\"", s.replace('"', "\"\""))
    }
    let row = [
        cell(&e.at_utc),
        cell(e.bot.as_deref().unwrap_or("")),
        cell(&e.run),
        cell(&e.account),
        cell(&e.from),
        cell(&e.recipients.join("; ")),
        cell(&e.subject),
        cell(&e.message_id),
        cell(if e.opt_out_line { "yes" } else { "no" }),
        cell(&e.overridden.join("; ")),
    ];
    format!("{}\r\n", row.join(","))
}

impl MailGuard {
    /// Memory only (tests, and the default until the host gives a directory).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            send_lock: tokio::sync::Mutex::new(()),
            dir: None,
            state: Mutex::new(State::default()),
            log_memory: DEFAULT_LOG_MEMORY,
            log_rotate_bytes: DEFAULT_LOG_ROTATE_BYTES,
        }
    }

    /// Load (or start) the files under `dir` with the default log limits.
    #[must_use]
    pub fn open(dir: PathBuf) -> Self {
        Self::open_with_limits(dir, DEFAULT_LOG_MEMORY, DEFAULT_LOG_ROTATE_BYTES)
    }

    /// Load (or start) the files under `dir`. At most `log_memory` send-log entries are kept in
    /// memory (and scanned by checks); the log file moves aside once it passes `rotate_bytes`.
    ///
    /// A settings or opt-out file that cannot be read is renamed to `<name>.bad-<unix ts>` before
    /// anything else, the guard remembers it (every mail card then carries a warning) and the
    /// damaged content is never overwritten. A damaged send-log line is skipped.
    #[must_use]
    pub fn open_with_limits(dir: PathBuf, log_memory: usize, rotate_bytes: u64) -> Self {
        let mut st = State::default();
        match load_json::<GuardSettings>(&dir, "guard-settings.json", |s| s.validate().is_ok()) {
            Loaded::Ok(s) => st.settings = s,
            Loaded::Missing => {}
            Loaded::Bad { .. } => st.load_errors.push("settings"),
        }
        match load_json::<Vec<OptOut>>(&dir, "opt-out.json", |_| true) {
            Loaded::Ok(v) => {
                let list = v
                    .into_iter()
                    .map(|mut o| {
                        if let Ok((value, kind)) = normalize_opt_out(&o.value) {
                            o.value = value;
                            o.kind = kind.into();
                        }
                        o
                    })
                    .collect();
                st.set_opt_outs(list);
            }
            Loaded::Missing => {}
            Loaded::Bad { set_aside } => {
                st.load_errors.push("opt-out list");
                st.opt_out_locked = !set_aside;
            }
        }
        // The newest `log_memory` entries, reaching back into moved-aside files when the active
        // one is short (right after a rotation).
        let keep = log_memory.max(1);
        for f in log_files(&dir).iter().rev() {
            if st.log.len() >= keep {
                break;
            }
            if let Ok(t) = std::fs::read_to_string(f) {
                let mut older = parse_log(&t, keep - st.log.len());
                older.append(&mut st.log);
                st.log = older;
            }
        }
        Self {
            send_lock: tokio::sync::Mutex::new(()),
            dir: Some(dir),
            state: Mutex::new(st),
            log_memory: log_memory.max(1),
            log_rotate_bytes: rotate_bytes,
        }
    }

    /// Names of saved files that could not be read at start (empty when all is well).
    #[must_use]
    pub fn load_errors(&self) -> Vec<&'static str> {
        lock(&self.state).load_errors.clone()
    }

    /// Atomic save: temp file in the same folder, flushed to disk, then renamed over the old one,
    /// so a crash leaves either the old or the new file, never a truncated one.
    fn write_json(&self, name: &str, text: &str) -> Result<(), String> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let Some(dir) = &self.dir else { return Ok(()) };
        let err = |e: std::io::Error| format!("cannot save: {e}");
        std::fs::create_dir_all(dir).map_err(err)?;
        let tmp = dir.join(format!(
            "{name}.{}.tmp",
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let res = (|| {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, dir.join(name))
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res.map_err(err)
    }

    /// Current settings.
    #[must_use]
    pub fn settings(&self) -> GuardSettings {
        lock(&self.state).settings.clone()
    }

    /// Replace the settings (owner only; the daemon route classifies it).
    ///
    /// # Errors
    /// Invalid values or a failed save.
    pub fn set_settings(&self, mut s: GuardSettings) -> Result<(), String> {
        s.validate()?;
        s.verification_link_domains = s.verification_domains();
        let text = serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?;
        self.write_json("guard-settings.json", &text)?;
        lock(&self.state).settings = s;
        Ok(())
    }

    /// The opt-out list, newest first.
    #[must_use]
    pub fn opt_outs(&self) -> Vec<OptOut> {
        let mut v = lock(&self.state).opt_outs.clone();
        v.reverse();
        v
    }

    /// Add an address or domain (idempotent).
    ///
    /// # Errors
    /// Invalid entry, list full, failed save.
    pub fn add_opt_out(&self, raw: &str, note: &str) -> Result<OptOut, String> {
        let (value, kind) = normalize_opt_out(raw)?;
        let mut st = lock(&self.state);
        if st.opt_out_locked {
            return Err(OPT_OUT_LOCKED.into());
        }
        if let Some(e) = st.opt_outs.iter().find(|o| o.value == value) {
            return Ok(e.clone());
        }
        if st.opt_outs.len() >= MAX_OPT_OUTS {
            return Err("the opt-out list is full".into());
        }
        let entry = OptOut {
            value,
            kind: kind.into(),
            added_at: now_secs(),
            note: note.chars().take(200).collect(),
        };
        let mut next = st.opt_outs.clone();
        next.push(entry.clone());
        let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        self.write_json("opt-out.json", &text)?;
        st.set_opt_outs(next);
        Ok(entry)
    }

    /// Remove an entry; `false` when it was not there.
    ///
    /// # Errors
    /// A failed save.
    pub fn remove_opt_out(&self, raw: &str) -> Result<bool, String> {
        let want = normalize_opt_out(raw).map_or_else(
            |_| raw.trim().trim_start_matches('@').to_lowercase(),
            |(v, _)| v,
        );
        let mut st = lock(&self.state);
        if st.opt_out_locked {
            return Err(OPT_OUT_LOCKED.into());
        }
        let next: Vec<OptOut> = st
            .opt_outs
            .iter()
            .filter(|o| o.value != want)
            .cloned()
            .collect();
        if next.len() == st.opt_outs.len() {
            return Ok(false);
        }
        let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        self.write_json("opt-out.json", &text)?;
        st.set_opt_outs(next);
        Ok(true)
    }

    /// The send log, newest first (at most `limit`).
    #[must_use]
    pub fn log(&self, limit: usize) -> Vec<SendLogEntry> {
        lock(&self.state)
            .log
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// The whole log as CSV (oldest first), for the owner's export: moved-aside log files first,
    /// then the active one (memory only when there is no folder). Cells that start with a formula
    /// character are prefixed so a spreadsheet never runs them.
    #[must_use]
    pub fn export_csv(&self) -> String {
        let mut out = String::from(
            "time_utc,bot,run,account,from,recipients,subject,message_id,opt_out_line,send_anyway\r\n",
        );
        let Some(dir) = &self.dir else {
            for e in &lock(&self.state).log {
                out.push_str(&csv_row(e));
            }
            return out;
        };
        let files = log_files(dir);
        for f in files {
            if let Ok(t) = std::fs::read_to_string(&f) {
                for e in t
                    .lines()
                    .filter_map(|l| serde_json::from_str::<SendLogEntry>(l).ok())
                {
                    out.push_str(&csv_row(&e));
                }
            }
        }
        out
    }

    /// Evaluate the soft guards for `facts`. Never refuses.
    #[must_use]
    pub fn check(&self, facts: &MailFacts<'_>) -> GuardReport {
        self.evaluate(facts, false)
    }

    /// The guards that apply to a calendar invitation, an update of one or a
    /// cancellation, sent to `recipients` from `account`: the opt-out list and the daily
    /// limit. Not the repeat-recipient rule (an update and a cancellation follow the
    /// invitation by design) and not the opt-out line (an invitation carries none).
    #[must_use]
    pub fn check_invite(&self, account: &str, recipients: &[String]) -> GuardReport {
        self.evaluate(
            &MailFacts {
                bot: None,
                account,
                from: "",
                recipients,
                opt_out_line: true,
            },
            true,
        )
    }

    fn evaluate(&self, facts: &MailFacts<'_>, invite: bool) -> GuardReport {
        let st = lock(&self.state);
        let now = now_secs();
        let mut warnings = Vec::new();

        let keyed: Vec<(&String, AddressKeys)> =
            facts.recipients.iter().map(|r| (r, key_of(r))).collect();
        let hits: Vec<String> = keyed
            .iter()
            .filter(|(_, k)| {
                st.opt_addrs.contains(&k.full)
                    || st.opt_addrs.contains(&k.base)
                    || st.opt_domains.iter().any(|d| domain_covers(d, &k.domain))
            })
            .map(|(r, _)| (*r).clone())
            .collect();
        if let Some(w) = unreadable_warning(&st.load_errors) {
            warnings.push(w);
        }
        let mut legal = false;
        if !hits.is_empty() {
            legal = true;
            warnings.push(Warning {
                code: "opt_out_listed".into(),
                text: format!(
                    "These recipients asked not to be written to (opt-out list): {}.",
                    hits.join(", ")
                ),
                addresses: hits,
                count: 0,
                limit: 0,
            });
        }

        let day_ago = now.saturating_sub(86_400);
        let sent_today: usize = st
            .log
            .iter()
            .filter(|e| e.at >= day_ago && e.account == facts.account)
            .map(|e| e.recipients.len().max(1))
            .sum();
        let limit = st.settings.daily_limit;
        let planned = facts.recipients.len();
        if limit > 0 && sent_today + planned > limit as usize {
            warnings.push(Warning {
                code: "daily_limit".into(),
                text: format!(
                    "This would make {} e-mails from this account in 24 hours; your limit is {limit}.",
                    sent_today + planned
                ),
                addresses: Vec::new(),
                count: u32::try_from(sent_today + planned).unwrap_or(u32::MAX),
                limit,
            });
        }

        let days = if invite {
            0
        } else {
            st.settings.same_recipient_days
        };
        if days > 0 {
            let since = now.saturating_sub(u64::from(days) * 86_400);
            let seen: HashSet<String> = st
                .log
                .iter()
                .filter(|e| e.at >= since)
                .flat_map(|e| e.recipients.iter().map(|x| key_of(x).base))
                .collect();
            let repeat: Vec<String> = keyed
                .iter()
                .filter(|(_, k)| seen.contains(&k.base))
                .map(|(r, _)| (*r).clone())
                .collect();
            if !repeat.is_empty() {
                warnings.push(Warning {
                    code: "same_recipient".into(),
                    text: format!(
                        "You already wrote to these recipients in the last {days} days: {}.",
                        repeat.join(", ")
                    ),
                    addresses: repeat,
                    count: 0,
                    limit: days,
                });
            }
        }

        if !facts.opt_out_line && !invite {
            legal = true;
            warnings.push(Warning {
                code: "no_opt_out_line".into(),
                text: "This mail would go out without the opt-out line.".into(),
                addresses: Vec::new(),
                count: 0,
                limit: 0,
            });
        }

        GuardReport {
            warnings,
            legal_notice: legal.then(|| LEGAL_NOTICE.to_owned()),
            footer: (facts.opt_out_line && !invite).then(|| st.settings.footer_for(facts.from)),
        }
    }

    /// Whether the opt-out line is on by default (owner setting).
    #[must_use]
    pub fn opt_out_line_default(&self) -> bool {
        lock(&self.state).settings.opt_out_line
    }

    /// Remember that the owner is being shown the mail with `key`, with exactly these
    /// warnings (none is a card too: a send can then tell "the owner saw this very mail"
    /// from "no card was shown for it").
    pub fn freeze(&self, key: &str, report: &GuardReport, token: &str) {
        let mut st = lock(&self.state);
        st.frozen.retain(|_, f| f.at.elapsed() < FREEZE_TTL);
        st.frozen.insert(
            key.to_owned(),
            Frozen {
                codes: report.codes(),
                token: token.to_owned(),
                at: Instant::now(),
            },
        );
    }

    /// Drop what [`Self::freeze`] kept for `token` (the approval ended without a send).
    pub fn discard(&self, token: &str) {
        lock(&self.state).frozen.retain(|_, f| f.token != token);
    }

    /// One-shot: what the owner was shown for this very mail (`key`). The entry is consumed
    /// whether or not it is still fresh.
    pub fn take_card(&self, key: &str, report: &GuardReport) -> CardCheck {
        let mut st = lock(&self.state);
        let Some(f) = st.frozen.remove(key) else {
            return CardCheck::default();
        };
        let shown = f.at.elapsed() < FREEZE_TTL;
        let covers = report.codes().iter().all(|c| f.codes.contains(c));
        CardCheck {
            shown,
            overrides: shown && covers && !f.codes.is_empty(),
        }
    }

    /// One-shot: `true` when the owner was shown every warning in `report` for this very mail
    /// (and there was a warning to send past).
    pub fn take_override(&self, key: &str, report: &GuardReport) -> bool {
        self.take_card(key, report).overrides
    }

    /// Log a mail the program sent on its own for the owner (not a bot's `email_send`), e.g. the
    /// "send a test message" button: `run` says what it was.
    pub fn record_system_mail(
        &self,
        run: &str,
        account: &str,
        from: &str,
        to: &str,
        subject: &str,
        message_id: &str,
    ) {
        self.record_program_mail(
            None,
            run,
            account,
            from,
            &[to.to_owned()],
            subject,
            message_id,
        );
    }

    /// Log a mail a connector sent for a bot (a calendar invitation, or one the calendar
    /// server delivered itself): like [`Self::record_system_mail`] but with the bot that
    /// acted and one entry for all `to`, so the send log and the daily limit show it.
    #[allow(clippy::too_many_arguments)] // the fields of one log line
    pub fn record_program_mail(
        &self,
        bot: Option<&str>,
        run: &str,
        account: &str,
        from: &str,
        to: &[String],
        subject: &str,
        message_id: &str,
    ) {
        self.record(SendLogEntry {
            at: now_secs(),
            at_utc: String::new(),
            bot: bot.map(str::to_owned),
            run: run.to_owned(),
            account: account.to_owned(),
            from: from.to_owned(),
            recipients: to.iter().map(|r| key_of(r).full).collect(),
            subject: subject.to_owned(),
            message_id: message_id.to_owned(),
            opt_out_line: false,
            overridden: Vec::new(),
        });
    }

    /// Append a sent mail to the log (memory first, then the file). Memory keeps only the last
    /// `log_memory` entries; the file moves aside when it grows past the rotation size.
    pub fn record(&self, mut entry: SendLogEntry) {
        entry.subject = entry.subject.chars().take(SUBJECT_KEEP).collect();
        entry.at_utc = utc_text(entry.at);
        let line = serde_json::to_string(&entry).unwrap_or_default();
        {
            let mut st = lock(&self.state);
            st.log.push(entry);
            if st.log.len() > self.log_memory {
                let extra = st.log.len() - self.log_memory;
                st.log.drain(..extra);
            }
        }
        if let Some(dir) = &self.dir {
            let active = dir.join("send-log.jsonl");
            let res = std::fs::create_dir_all(dir).and_then(|()| {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&active)?;
                writeln!(f, "{line}")?;
                f.sync_data()
            });
            if let Err(e) = res {
                log::warn!("email: could not write the send log: {e}");
            }
            if std::fs::metadata(&active).is_ok_and(|m| m.len() > self.log_rotate_bytes) {
                // Never overwrite an older archive made in the same second.
                let ts = now_secs();
                let mut n = 0;
                let mut old = dir.join(format!("send-log.{ts}.{n:04}.jsonl"));
                while old.exists() {
                    n += 1;
                    old = dir.join(format!("send-log.{ts}.{n:04}.jsonl"));
                }
                if let Err(e) = std::fs::rename(&active, old) {
                    log::warn!("email: could not rotate the send log: {e}");
                }
            }
        }
    }
}

/// Append the opt-out line to `body` (a separator the line cannot be confused with the text of).
#[must_use]
pub fn with_footer(body: &str, footer: &str) -> String {
    format!("{}\n\n-- \n{footer}\n", body.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(rcpt: &[String], line: bool) -> MailFacts<'_> {
        MailFacts {
            bot: Some("b1"),
            account: "work",
            from: "me@example.org",
            recipients: rcpt,
            opt_out_line: line,
        }
    }

    fn entry(rcpt: &[&str], at: u64) -> SendLogEntry {
        SendLogEntry {
            at,
            at_utc: String::new(),
            bot: None,
            run: "r".into(),
            account: "work".into(),
            from: "me@example.org".into(),
            recipients: rcpt.iter().map(|s| (*s).to_owned()).collect(),
            subject: "=cmd".into(),
            message_id: "m1".into(),
            opt_out_line: true,
            overridden: vec![],
        }
    }

    #[test]
    fn utc_text_is_right() {
        assert_eq!(utc_text(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_text(1_795_000_000), "2026-11-18T11:06:40Z");
    }

    #[test]
    fn opt_out_hits_address_and_domain_and_warns_with_legal_note() {
        let g = MailGuard::in_memory();
        g.add_opt_out("Ann@Example.com", "").unwrap();
        g.add_opt_out("@blocked.org", "").unwrap();
        let r = vec!["ann@example.com".to_owned(), "x@blocked.org".to_owned()];
        let rep = g.check(&facts(&r, true));
        assert_eq!(rep.codes(), vec!["opt_out_listed"]);
        assert_eq!(rep.warnings[0].addresses.len(), 2);
        assert!(rep.legal_notice.is_some());
        let ok = vec!["bob@example.com".to_owned()];
        assert_eq!(g.check(&facts(&ok, true)).codes(), Vec::<String>::new());
    }

    #[test]
    fn new_default_disables_repeat_wait_and_saved_setting_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            MailGuard::open(dir.path().to_path_buf())
                .settings()
                .same_recipient_days,
            0
        );
        std::fs::write(
            dir.path().join("guard-settings.json"),
            r#"{"same_recipient_days":14}"#,
        )
        .unwrap();
        assert_eq!(
            MailGuard::open(dir.path().to_path_buf())
                .settings()
                .same_recipient_days,
            14
        );
    }

    #[test]
    fn daily_limit_and_same_recipient_are_warnings_not_blocks() {
        let g = MailGuard::in_memory();
        let s = GuardSettings {
            daily_limit: 2,
            same_recipient_days: 14,
            ..GuardSettings::default()
        };
        g.set_settings(s).unwrap();
        g.record(entry(&["a@x.io", "b@x.io"], now_secs()));
        let r = vec!["a@x.io".to_owned()];
        let rep = g.check(&facts(&r, true));
        assert_eq!(rep.codes(), vec!["daily_limit", "same_recipient"]);
        assert!(rep.legal_notice.is_none());
    }

    #[test]
    fn override_needs_a_matching_frozen_card_and_is_one_shot() {
        let g = MailGuard::in_memory();
        g.add_opt_out("a@x.io", "").unwrap();
        let r = vec!["a@x.io".to_owned()];
        let rep = g.check(&facts(&r, true));
        assert!(
            !g.take_override("k", &rep),
            "nothing was shown to the owner"
        );
        g.freeze("k", &rep, "t1");
        assert!(g.take_override("k", &rep));
        assert!(!g.take_override("k", &rep), "consumed");
        g.freeze("k", &rep, "t2");
        g.discard("t2");
        assert!(!g.take_override("k", &rep), "approval ended");
        // A new warning that the owner was not shown does not ride on an old card.
        let none = g.check(&facts(&["z@x.io".to_owned()], true));
        g.freeze("k", &none, "t3");
        assert!(!g.take_override("k", &rep));
    }

    #[test]
    fn a_card_without_warnings_is_remembered_but_overrides_nothing() {
        let g = MailGuard::in_memory();
        let clean = g.check(&facts(&["z@x.io".to_owned()], true));
        assert_eq!(clean.codes(), Vec::<String>::new());
        assert_eq!(
            g.take_card("k", &clean),
            CardCheck::default(),
            "no card was shown"
        );
        g.freeze("k", &clean, "t1");
        let seen = g.take_card("k", &clean);
        assert!(seen.shown && !seen.overrides, "{seen:?}");
        assert!(!g.take_card("k", &clean).shown, "one shot");
        g.freeze("k", &clean, "t2");
        g.discard("t2");
        assert!(!g.take_card("k", &clean).shown, "approval ended");
        // A warning the card did not show is no override, whatever else it showed.
        g.add_opt_out("a@x.io", "").unwrap();
        let warned = g.check(&facts(&["a@x.io".to_owned()], true));
        g.freeze("k", &clean, "t3");
        let c = g.take_card("k", &warned);
        assert!(c.shown && !c.overrides, "{c:?}");
    }

    #[test]
    fn export_neutralises_formulas_and_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let g = MailGuard::open(dir.path().to_path_buf());
        g.record(entry(&["a@x.io"], 100));
        g.add_opt_out("b@x.io", "asked").unwrap();
        assert!(g.export_csv().contains("\"'=cmd\""));
        let again = MailGuard::open(dir.path().to_path_buf());
        assert_eq!(again.log(10).len(), 1);
        assert_eq!(again.opt_outs().len(), 1);
        assert!(again.remove_opt_out("b@x.io").unwrap());
    }

    #[test]
    fn address_keys_cover_case_tag_idn_display_name_and_trailing_dot() {
        let k = canonical_address("\"Ann\" <A+Promo@X.com.>").unwrap();
        assert_eq!(
            (k.full.as_str(), k.base.as_str()),
            ("a+promo@x.com", "a@x.com")
        );
        let u = canonical_address("ann@MÜNCHEN.de").unwrap();
        assert_eq!(u.full, "ann@xn--mnchen-3ya.de");
        assert_eq!(canonical_address("ann@xn--mnchen-3ya.de").unwrap(), u);
        assert_eq!(canonical_address("+x@a.com").unwrap().base, "+x@a.com");
        assert!(canonical_address("nobody").is_none());
        assert_eq!(
            normalize_opt_out("Ann@Münchén.DE.").unwrap(),
            ("ann@xn--mnchn-esa3k.de".to_owned(), "address")
        );
        assert!(domain_covers("x.com", "shop.x.com"));
        assert!(!domain_covers("x.com", "notx.com"));
    }

    #[test]
    fn opt_out_matches_both_keys_and_subdomains_and_the_other_checks_use_them() {
        let g = MailGuard::in_memory();
        g.add_opt_out("a@x.com", "").unwrap();
        g.add_opt_out("@blocked.org", "").unwrap();
        for r in ["A@X.com", "a+t@x.com", "a@x.com.", "u@sub.blocked.org"] {
            let rep = g.check(&facts(&[r.to_owned()], true));
            assert_eq!(rep.codes(), vec!["opt_out_listed"], "{r}");
        }
        assert_eq!(
            g.check(&facts(&["b@x.com".into()], true)).codes(),
            Vec::<String>::new()
        );
        assert_eq!(
            g.check(&facts(&["u@notblocked.org".into()], true)).codes(),
            Vec::<String>::new()
        );
        g.set_settings(GuardSettings {
            same_recipient_days: 14,
            ..GuardSettings::default()
        })
        .unwrap();
        // same-recipient wait: a +tag variant of a logged mailbox is the same recipient
        g.record(entry(&["c@y.io"], now_secs()));
        let rep = g.check(&facts(&["C+news@y.io".into()], true));
        assert_eq!(rep.codes(), vec!["same_recipient"]);
    }

    #[test]
    fn a_corrupt_opt_out_file_is_kept_warned_about_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let junk = "[{\"value\":\"ann@x.com\",\"kind\":\"add";
        std::fs::write(dir.path().join("opt-out.json"), junk).unwrap();
        std::fs::write(dir.path().join("guard-settings.json"), "{not json").unwrap();
        let g = MailGuard::open(dir.path().to_path_buf());
        assert_eq!(g.load_errors(), vec!["settings", "opt-out list"]);
        let rep = g.check(&facts(&["ann@x.com".into()], true));
        assert!(
            rep.codes().contains(&"guard_unreadable".to_owned()),
            "{rep:?}"
        );
        // the owner adds an entry: the damaged content must still be on disk
        g.add_opt_out("bob@x.com", "").unwrap();
        let bad: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("opt-out.json.bad-"))
            .collect();
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&bad[0])).unwrap(),
            junk
        );
        assert!(std::fs::read_dir(dir.path()).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("guard-settings.json.bad-")
        }));
        // saves leave no temp file behind and the new list is readable
        assert!(
            !std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        );
        assert_eq!(
            MailGuard::open(dir.path().to_path_buf()).opt_outs().len(),
            1
        );
    }

    #[test]
    fn the_log_in_memory_is_capped_and_the_file_rotates_but_the_export_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let g = MailGuard::open_with_limits(dir.path().to_path_buf(), 3, 400);
        for i in 0..12 {
            g.record(entry(&[&format!("u{i}@x.io")], 1_000 + i));
        }
        assert_eq!(g.log(100).len(), 3, "memory is capped");
        assert_eq!(g.log(1)[0].recipients, ["u11@x.io"]);
        let csv = g.export_csv();
        for i in 0..12 {
            assert!(
                csv.contains(&format!("u{i}@x.io")),
                "u{i} missing from export"
            );
        }
        assert_eq!(
            MailGuard::open_with_limits(dir.path().to_path_buf(), 3, 400)
                .log(100)
                .len(),
            3
        );
    }

    #[test]
    fn an_invitation_is_held_to_the_opt_outs_and_the_daily_limit_only() {
        let g = MailGuard::in_memory();
        g.add_opt_out("ann@example.com", "").unwrap();
        let one = |a: &str| vec![a.to_owned()];
        // An address that asked not to be written to is refused ...
        let rep = g.check_invite("work", &one("ann@example.com"));
        assert_eq!(rep.codes(), vec!["opt_out_listed"]);
        assert!(rep.legal_notice.is_some());
        // ... and any other address is fine: an invitation carries no opt-out line, so that
        // is not a warning for it (and asks for no footer) ...
        let ok = g.check_invite("work", &one("bob@example.com"));
        assert_eq!(ok.warnings, vec![]);
        assert!(ok.footer.is_none() && ok.legal_notice.is_none());
        // ... while an ordinary mail without the line still warns, as it always did.
        assert_eq!(
            g.check(&facts(&one("bob@example.com"), false)).codes(),
            vec!["no_opt_out_line"]
        );
        // The daily limit counts the account's mail, invitations included.
        g.set_settings(GuardSettings {
            daily_limit: 2,
            ..GuardSettings::default()
        })
        .unwrap();
        g.record(entry(&["a@x.io", "b@x.io"], now_secs()));
        assert_eq!(
            g.check_invite("work", &one("c@x.io")).codes(),
            vec!["daily_limit"]
        );
        // Another account's count is its own.
        assert_eq!(g.check_invite("calendar", &one("c@x.io")).warnings, vec![]);
        // An update or a cancellation goes to people who were just invited: that is not a
        // repeat to hold back, while the plain check still flags it.
        assert_eq!(
            g.check_invite("work", &one("a@x.io")).codes(),
            vec!["daily_limit"]
        );
        g.set_settings(GuardSettings {
            same_recipient_days: 14,
            ..GuardSettings::default()
        })
        .unwrap();
        assert_eq!(g.check_invite("work", &one("a@x.io")).warnings, vec![]);
        assert_eq!(
            g.check(&facts(&one("a@x.io"), true)).codes(),
            vec!["same_recipient"]
        );
    }

    #[test]
    fn a_mail_a_program_sent_is_logged_with_the_bot_and_every_recipient() {
        let g = MailGuard::in_memory();
        let to = vec!["a@x.io".to_owned(), "B@x.io".to_owned()];
        g.record_program_mail(
            Some("bot-7"),
            "calendar-server-invite",
            "work",
            "me@example.org",
            &to,
            "Invitation: Sync",
            "m9",
        );
        let log = g.log(10);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].bot.as_deref(), Some("bot-7"));
        assert_eq!(log[0].run, "calendar-server-invite");
        assert_eq!(log[0].recipients, ["a@x.io", "b@x.io"]);
        assert_eq!(log[0].account, "work");
        // It counts against the daily limit like any other send.
        g.set_settings(GuardSettings {
            daily_limit: 3,
            ..GuardSettings::default()
        })
        .unwrap();
        assert_eq!(
            g.check(&facts(&["c@x.io".to_owned(), "d@x.io".to_owned()], true))
                .codes(),
            vec!["daily_limit"]
        );
        // The older single-recipient helper is the same entry with no bot.
        g.record_system_mail("r", "work", "me@example.org", "e@x.io", "s", "m10");
        let log = g.log(10);
        assert_eq!(log.len(), 2);
        assert!(
            log.iter()
                .any(|e| e.bot.is_none() && e.recipients == ["e@x.io"])
        );
    }
}

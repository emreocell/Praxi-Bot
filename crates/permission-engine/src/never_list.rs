//! The "never list" — commands that always require approval regardless of policy.
//!
//! These are catastrophic, irreversible operations.  Even a `full()` policy
//! cannot bypass them.
//!
//! ## Design
//!
//! The command is **parsed**, not glob-matched as a whole string.  It is split
//! into segments (`;`, `&&`, `||`, `|`, `&`, newline), leading wrappers such as
//! `sudo` are skipped, `sh -c` / `cmd /c` / `powershell -Command` payloads are
//! inspected recursively, and each segment becomes *program + arguments*.  Two
//! kinds of checks then apply:
//!
//! Beyond that: `( … )`, `{ …; }`, backticks and `$( … )` are command
//! boundaries whose contents are scanned recursively; `eval`, `env -S`,
//! `timeout`, `xargs`, `busybox`, `setsid`, `stdbuf`, `chroot`, … are wrappers
//! with per-wrapper option tables (case-sensitive: `sudo -S` is not `-s`);
//! globs in protected components (`/e*`, `C:\Win*`) and MSYS/WSL/Cygwin drive
//! paths (`/c/Windows`, `/mnt/c/Windows`) are recognised.
//!
//! Known limit: targets fed through stdin (`… | xargs rm -rf`) cannot be seen.
//!
//! 1. **Unconditional** — the program is catastrophic whatever its target:
//!    `format`, `diskpart` + `clean`, `mkfs*`, `dd of=/dev/*`, `bcdedit`,
//!    `Format-Volume`, `Clear-Disk`, `cipher /w`, and bootloader installers
//!    (`grub-install`, `bootsect`).
//! 2. **Target-aware** — a recursive delete (`rm -r`, `Remove-Item -Recurse`,
//!    `rd /s`, `rmdir /s`, `del /s`) matches only when one of its targets is a
//!    *protected* path (filesystem root, drive root, home root, system
//!    directories).  `rm -rf /tmp/x` therefore does **not** match.
//!
//! The protected path lists are plain data (the `const` tables below).

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::PolicyError;

// ── Protected-path data ───────────────────────────────────────────────────────

/// Unix/macOS system directories: the directory itself *and* anything below it.
const UNIX_SYSTEM_TREES: &[&str] = &["bin", "boot", "etc", "sbin", "usr", "system"];

/// Unix directories whose first path component may merely *start with* one of
/// these prefixes (`/lib`, `/lib32`, `/lib64`, `/libexec`, …); the directory
/// itself and anything below it is protected.
const UNIX_SYSTEM_TREE_PREFIXES: &[&str] = &["lib"];

/// Unix/macOS top-level directories protected only as the directory itself
/// (their sub-paths, e.g. `/var/tmp/x`, are ordinary).
const UNIX_PROTECTED_EXACT: &[&str] = &["var", "root"];

/// Parents of per-user home directories.  Both the parent (`/home`) and a
/// direct child (`/home/<name>`) are protected; deeper paths are not.
const UNIX_HOME_PARENTS: &[&str] = &["home", "users"];

/// Literal home-directory spellings as they appear before shell expansion.
const HOME_TOKENS: &[&str] = &[
    "~",
    "$home",
    "${home}",
    "$env:home",
    "$env:userprofile",
    "${env:userprofile}",
    "%userprofile%",
];

/// Windows system directories directly under a drive root: the directory
/// itself and anything below it.
const WIN_SYSTEM_TREES: &[&str] = &["windows", "programdata"];

/// Windows system directories identified by name prefix (`Program Files`,
/// `Program Files (x86)`).
const WIN_SYSTEM_TREE_PREFIXES: &[&str] = &["program files"];

/// Windows directory holding per-user home directories.  Both `C:\Users` and
/// `C:\Users\<name>` are protected; deeper paths are not.
const WIN_HOME_PARENT: &str = "users";

/// Device-node prefixes that `dd of=` must not write to.
const DEVICE_PREFIXES: &[&str] = &["/dev/", r"\\.\physicaldrive"];

/// Pseudo-devices that are harmless `dd` outputs.
const HARMLESS_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
    "/dev/random",
    "/dev/urandom",
];

// ── Command-parsing data ──────────────────────────────────────────────────────

/// How to skip over a wrapper's own options to reach the wrapped command.
struct WrapperSpec {
    /// Option tokens (exact, case-sensitive) that consume the next token.
    value_opts: &'static [&'static str],
    /// Positional arguments to skip before the command (`timeout 5 cmd`).
    positionals: usize,
}

/// Look up a wrapper by its lower-case program name.
fn wrapper_spec(name: &str) -> Option<WrapperSpec> {
    let (value_opts, positionals): (&'static [&'static str], usize) = match name {
        "sudo" => (
            &[
                "-u",
                "-g",
                "-h",
                "-p",
                "-C",
                "-D",
                "-R",
                "-T",
                "-U",
                "-r",
                "-t",
                "--user",
                "--group",
                "--host",
                "--prompt",
                "--close-from",
                "--chdir",
                "--chroot",
                "--other-user",
                "--role",
                "--type",
                "--command-timeout",
            ],
            0,
        ),
        "doas" => (&["-u", "-C"], 0),
        "env" => (&["-u", "-C", "--unset", "--chdir"], 0),
        "nohup" | "command" | "setsid" | "busybox" | "toybox" | "unbuffer" => (&[], 0),
        "exec" => (&["-a"], 0),
        "time" => (&["-f", "-o", "--format", "--output"], 0),
        "nice" => (&["-n", "--adjustment"], 0),
        "ionice" => (&["-c", "-n", "-p", "-P", "-u", "--class", "--classdata"], 0),
        "stdbuf" => (&["-i", "-o", "-e", "--input", "--output", "--error"], 0),
        "timeout" => (&["-s", "-k", "--signal", "--kill-after"], 1),
        "chroot" => (&["--userspec", "--groups"], 1),
        "taskset" => (&[], 1),
        "flock" => (&["-w", "-E", "--timeout", "--conflict-exit-code"], 1),
        "wsl" => (&["-d", "--distribution", "-u", "--user", "--cd"], 0),
        "xargs" => (
            &[
                "-a",
                "-d",
                "-E",
                "-I",
                "-L",
                "-n",
                "-P",
                "-s",
                "--arg-file",
                "--delimiter",
                "--eof",
                "--replace",
                "--max-lines",
                "--max-args",
                "--max-procs",
                "--max-chars",
            ],
            0,
        ),
        _ => return None,
    };
    Some(WrapperSpec {
        value_opts,
        positionals,
    })
}

/// Shell keywords that may precede a command inside a compound statement.
const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{", "}",
];

/// POSIX shells that accept `-c <command>`.
const POSIX_SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// PowerShell hosts that accept `-Command <command>`.
const POWERSHELL_HOSTS: &[&str] = &["powershell", "pwsh"];

/// Maximum nesting of `sh -c "…"`-style payloads that is inspected.  Deeper
/// nesting is treated as suspicious and matches.
const MAX_PAYLOAD_DEPTH: u8 = 4;

// ── Target classification ─────────────────────────────────────────────────────

/// What a path is anchored to.
#[derive(Debug, PartialEq, Eq)]
enum Anchor {
    /// Absolute Unix path (`/…`).
    Unix,
    /// Windows drive (`C:\…`).
    Drive,
    /// Home directory (`~`, `$HOME`, `%USERPROFILE%`, …).
    Home,
    /// Anything else (relative path, bare word).
    Relative,
}

/// Lexically normalised target path: anchor plus components (no `.`, `..`,
/// empty components, or trailing glob `*`).
struct Target {
    anchor: Anchor,
    components: Vec<String>,
}

fn is_drive_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(c), Some(':'), None) if c.is_ascii_alphabetic()
    )
}

/// Windows system-root spellings that expand to `C:\Windows`.
const WIN_ROOT_TOKENS: &[&str] = &["%windir%", "%systemroot%", "$env:windir", "$env:systemroot"];

/// Concrete directory names that match the `lib*` prefix rule.
const UNIX_LIB_NAMES: &[&str] = &["lib", "lib32", "lib64", "libx32", "libexec"];
/// Concrete names matching the `program files*` prefix rule.
const WIN_PF_NAMES: &[&str] = &["program files", "program files (x86)"];

/// `true` if the component contains shell glob metacharacters.
fn has_glob(comp: &str) -> bool {
    comp.contains(['*', '?', '[', '{'])
}

/// Does a (possibly glob) path component denote one of `names`?  An invalid
/// glob fails closed (matches).
fn comp_matches_any(comp: &str, names: &[&str]) -> bool {
    match Glob::new(comp) {
        Ok(glob) => {
            let matcher = glob.compile_matcher();
            names.iter().any(|n| matcher.is_match(n))
        }
        Err(_) => true,
    }
}

fn is_single_letter(s: &str) -> bool {
    let mut chars = s.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphabetic())
}

impl Target {
    /// Parse a path argument (case-insensitive).
    fn parse(raw: &str) -> Self {
        let lowered = raw.to_lowercase();
        let mut unified = lowered.replace('\\', "/");
        // Win32 namespace prefixes: `\\?\C:\...`, `\\.\C:\...`.
        for prefix in ["//?/", "//./"] {
            if let Some(rest) = unified.strip_prefix(prefix) {
                unified = rest.to_owned();
                break;
            }
        }
        let trailing_sep = unified.ends_with('/');
        let mut parts = unified.split('/');

        let mut win_root = false;
        let (mut anchor, first_is_anchor) = if unified.starts_with('/') {
            (Anchor::Unix, false)
        } else {
            match parts.clone().next() {
                Some(first) if is_drive_prefix(first) => (Anchor::Drive, true),
                Some(first) if HOME_TOKENS.contains(&first) => (Anchor::Home, true),
                Some(first) if WIN_ROOT_TOKENS.contains(&first) => {
                    win_root = true;
                    (Anchor::Drive, true)
                }
                _ => (Anchor::Relative, false),
            }
        };
        if first_is_anchor {
            parts.next();
        }

        let mut components: Vec<String> = Vec::new();
        if win_root {
            components.push("windows".to_owned());
        }
        for part in parts {
            match part {
                "" | "." => {}
                ".." => {
                    if components.last().is_some_and(|c| c != "..") {
                        components.pop();
                    } else if anchor == Anchor::Relative {
                        components.push("..".to_owned());
                    }
                }
                other => components.push(other.to_owned()),
            }
        }

        // MSYS / Git Bash (`/c/Windows`), WSL (`/mnt/c/...`) and Cygwin
        // (`/cygdrive/c/...`) spell Windows drives as Unix paths.  A bare `/x`
        // is ambiguous (an ordinary Unix directory), so it only counts as a
        // drive root with a trailing separator or further components.
        if anchor == Anchor::Unix {
            let drive_at = match components.as_slice() {
                [m, l, ..] if (m == "mnt" || m == "cygdrive") && is_single_letter(l) => Some(2),
                [l, rest @ ..] if is_single_letter(l) && (!rest.is_empty() || trailing_sep) => {
                    Some(1)
                }
                _ => None,
            };
            if let Some(n) = drive_at {
                components.drain(..n);
                anchor = Anchor::Drive;
            }
        }

        // `/etc/*` deletes the *contents* of `/etc`: treat it as `/etc`.
        if components.last().is_some_and(|c| c == "*") {
            components.pop();
        }
        Self { anchor, components }
    }

    /// Return `true` if the target is a protected path.
    fn is_protected(&self) -> bool {
        let comps: Vec<&str> = self.components.iter().map(String::as_str).collect();
        match self.anchor {
            Anchor::Relative => false,
            Anchor::Home => comps.is_empty(),
            Anchor::Unix => comps.is_empty() || Self::unix_protected(&comps),
            Anchor::Drive => comps.is_empty() || Self::windows_protected(&comps),
        }
    }

    fn unix_protected(comps: &[&str]) -> bool {
        let first = comps[0];
        if has_glob(first) {
            // A glob may expand to any protected directory: be conservative.
            return comp_matches_any(first, UNIX_SYSTEM_TREES)
                || comp_matches_any(first, UNIX_LIB_NAMES)
                || (comps.len() == 1 && comp_matches_any(first, UNIX_PROTECTED_EXACT))
                || (comps.len() <= 2 && comp_matches_any(first, UNIX_HOME_PARENTS));
        }
        if UNIX_SYSTEM_TREES.contains(&first)
            || UNIX_SYSTEM_TREE_PREFIXES
                .iter()
                .any(|prefix| first.starts_with(prefix))
        {
            return true;
        }
        if comps.len() == 1 && UNIX_PROTECTED_EXACT.contains(&first) {
            return true;
        }
        comps.len() <= 2 && UNIX_HOME_PARENTS.contains(&first)
    }

    fn windows_protected(comps: &[&str]) -> bool {
        let first = comps[0];
        if has_glob(first) {
            return comp_matches_any(first, WIN_SYSTEM_TREES)
                || comp_matches_any(first, WIN_PF_NAMES)
                || (comps.len() <= 2 && comp_matches_any(first, &[WIN_HOME_PARENT]));
        }
        if WIN_SYSTEM_TREES.contains(&first)
            || WIN_SYSTEM_TREE_PREFIXES
                .iter()
                .any(|prefix| first.starts_with(prefix))
        {
            return true;
        }
        comps.len() <= 2 && first == WIN_HOME_PARENT
    }
}

// ── Tokenising ────────────────────────────────────────────────────────────────

/// Incremental splitter used by [`split_segments`].
#[derive(Default)]
struct Splitter {
    segments: Vec<Vec<String>>,
    current: Vec<String>,
    token: String,
    has_token: bool,
}

impl Splitter {
    fn end_token(&mut self) {
        if self.has_token {
            self.current.push(std::mem::take(&mut self.token));
            self.has_token = false;
        }
    }

    fn end_segment(&mut self) {
        self.end_token();
        if !self.current.is_empty() {
            self.segments.push(std::mem::take(&mut self.current));
        }
    }
}

/// Split a command line into segments of unquoted tokens.  Unquoted `(`, `)` and
/// backticks start a new segment (subshells, `$(...)`, backticks); a standalone
/// `{` / `}` is a shell keyword (see `SHELL_KEYWORDS`).
///
/// Quotes group words (`"C:\Program Files"`) and are removed.  Backslashes are
/// literal so Windows paths survive.  Separators inside quotes are literal.
fn split_segments(command: &str) -> Vec<Vec<String>> {
    let mut sp = Splitter::default();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                sp.token.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                sp.has_token = true;
            }
            ';' | '|' | '&' | '\n' | '\r' | '(' | ')' | '`' => sp.end_segment(),
            c if c.is_whitespace() => sp.end_token(),
            c => {
                sp.token.push(c);
                sp.has_token = true;
            }
        }
    }
    sp.end_segment();
    sp.segments
}

/// Reduce a program token to its bare lower-case name (`/bin/rm` -> `rm`,
/// `Rmdir.EXE` -> `rmdir`, `format.com` -> `format`).
fn program_name(token: &str) -> String {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_lowercase();
    for ext in [".exe", ".com", ".bat", ".cmd"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    base
}

/// Result of peeling wrappers off a segment.
enum Resolved<'a> {
    /// A program (lower-case name) and its arguments.
    Command(String, &'a [String]),
    /// A command string to be parsed again (`eval ...`, `env -S ...`).
    Payload(String),
}

/// Skip leading wrappers / shell keywords and return the wrapped command.
fn resolve_program(tokens: &[String]) -> Option<Resolved<'_>> {
    let mut i = 0;
    loop {
        let name = program_name(tokens.get(i)?);
        i += 1;
        if SHELL_KEYWORDS.contains(&name.as_str()) {
            continue;
        }
        if name == "eval" {
            return Some(Resolved::Payload(tokens.get(i..)?.join(" ")));
        }
        if let Some(spec) = wrapper_spec(&name) {
            while let Some(tok) = tokens.get(i) {
                if tok == "--" {
                    i += 1;
                    break;
                }
                if name == "env" {
                    let attached = tok
                        .strip_prefix("--split-string=")
                        .or_else(|| tok.strip_prefix("-S").filter(|r| !r.is_empty()));
                    let separate = (tok == "-S" || tok == "--split-string").then_some("");
                    if let Some(head) = attached.or(separate) {
                        let rest = tokens.get(i + 1..).unwrap_or_default().join(" ");
                        return Some(Resolved::Payload(format!("{head} {rest}")));
                    }
                    if !tok.starts_with('-') && tok.contains('=') {
                        i += 1;
                        continue;
                    }
                }
                if tok.starts_with('-') && tok.len() > 1 {
                    i += 1;
                    if spec.value_opts.contains(&tok.as_str()) {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            i += spec.positionals;
            continue;
        }
        return Some(Resolved::Command(name, tokens.get(i..).unwrap_or_default()));
    }
}

/// Collect the bodies of `$(...)` and backtick substitutions (single-quoted
/// text is literal and skipped; double quotes do *not* prevent substitution).
fn extract_substitutions(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let (mut in_single, mut in_double) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_single {
            in_single = c != '\'';
        } else {
            match c {
                '\'' if !in_double => in_single = true,
                '"' => in_double = !in_double,
                '`' => {
                    let mut j = i + 1;
                    while j < chars.len() && chars[j] != '`' {
                        j += if chars[j] == '\\' { 2 } else { 1 };
                    }
                    let end = j.min(chars.len());
                    out.push(chars[i + 1..end].iter().collect());
                    i = end;
                }
                '$' if chars.get(i + 1) == Some(&'(') => {
                    let (mut depth, mut j) = (1, i + 2);
                    while j < chars.len() && depth > 0 {
                        match chars[j] {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    let end = if depth == 0 { j - 1 } else { j };
                    out.push(chars[i + 2..end].iter().collect());
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

// ── Per-program checks ────────────────────────────────────────────────────────

/// Return the command payload of `sh -c …`, `cmd /c …`, `powershell -Command …`.
fn shell_payload(program: &str, args: &[String]) -> Option<String> {
    let runs_command = |arg: &str| -> bool {
        if POSIX_SHELLS.contains(&program) {
            arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c')
        } else if program == "cmd" {
            arg.eq_ignore_ascii_case("/c") || arg.eq_ignore_ascii_case("/k")
        } else if POWERSHELL_HOSTS.contains(&program) {
            arg.len() >= 2
                && arg.starts_with('-')
                && "command".starts_with(&arg[1..].to_ascii_lowercase())
        } else {
            false
        }
    };
    let idx = args.iter().position(|a| runs_command(a))?;
    Some(args[idx + 1..].join(" "))
}

/// A Windows-style switch: `/` + one or two alphanumerics, optionally `:value`
/// (`/s`, `/q`, `/a:h`).  Longer words such as `/etc` are paths, not switches.
fn is_windows_switch(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('/') else {
        return false;
    };
    let name = rest.split(':').next().unwrap_or_default();
    (1..=2).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Unix `rm`: recursive (`-r`, `-R`, `-rf`, `--recursive`, `--no-preserve-root`)
/// against a protected target.  A leading `/…` is always a target here.
fn rm_matches(args: &[String]) -> bool {
    let mut recursive = false;
    let mut targets: Vec<&str> = Vec::new();
    let mut options_ended = false;
    for arg in args {
        if options_ended {
            targets.push(arg);
        } else if arg == "--" {
            options_ended = true;
        } else if arg.starts_with("--") {
            recursive |= arg == "--recursive" || arg == "--no-preserve-root";
        } else if arg.starts_with('-') && arg.len() > 1 {
            recursive |= arg.contains('r');
        } else {
            targets.push(arg);
        }
    }
    recursive && targets.iter().any(|t| Target::parse(t).is_protected())
}

/// `Remove-Item` / `ri` / `rd` / `rmdir` / `del` / `erase`: recursive
/// (`-Recurse`, `/s`) against a protected target.  Windows `/x` switches are
/// recognised only here, never for Unix programs.
fn windows_delete_matches(args: &[String]) -> bool {
    let mut recursive = false;
    let mut targets: Vec<&str> = Vec::new();
    for arg in args {
        if let Some(flag) = arg.strip_prefix('-').filter(|f| !f.is_empty()) {
            let name = flag.split(':').next().unwrap_or_default();
            recursive |= !name.is_empty() && "recurse".starts_with(name);
        } else if is_windows_switch(arg) {
            recursive |= arg == "/s";
        } else {
            targets.push(arg);
        }
    }
    recursive && targets.iter().any(|t| Target::parse(t).is_protected())
}

/// `dd` writing to a block device (`of=/dev/sda`, `of=\\.\PhysicalDrive0`).
fn dd_matches(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg.strip_prefix("of=").is_some_and(|out| {
            DEVICE_PREFIXES.iter().any(|p| out.starts_with(p)) && !HARMLESS_DEVICES.contains(&out)
        })
    })
}

/// Decide whether one command segment is catastrophic.
fn segment_matches(tokens: &[String], mentions_clean: bool, depth: u8) -> bool {
    let (program, raw_args) = match resolve_program(tokens) {
        None => return false,
        Some(Resolved::Payload(payload)) => return structural_match(&payload, depth + 1),
        Some(Resolved::Command(program, args)) => (program, args),
    };
    let program = program.as_str();

    if let Some(payload) = shell_payload(program, raw_args) {
        return structural_match(&payload, depth + 1);
    }

    // Option / target comparisons are case-insensitive.
    let lowered: Vec<String> = raw_args.iter().map(|a| a.to_lowercase()).collect();
    let args = lowered.as_slice();

    if program.starts_with("mkfs") {
        return true;
    }

    match program {
        "format" | "format-volume" | "clear-disk" | "bcdedit" | "grub-install"
        | "grub2-install" | "bootsect" => true,
        "diskpart" => mentions_clean || args.iter().any(|a| a == "/s"),
        "cipher" => args.iter().any(|a| a.starts_with("/w")),
        "dd" => dd_matches(args),
        "rm" => rm_matches(args),
        "remove-item" | "ri" | "rd" | "rmdir" | "del" | "erase" => windows_delete_matches(args),
        _ => false,
    }
}

/// Run the structural checks over a command (substitutions, then segments).
fn structural_match(command: &str, depth: u8) -> bool {
    if depth > MAX_PAYLOAD_DEPTH {
        return true;
    }
    if extract_substitutions(command)
        .iter()
        .any(|inner| structural_match(inner, depth + 1))
    {
        return true;
    }
    let segments = split_segments(command);
    let mentions_clean = segments
        .iter()
        .flatten()
        .any(|t| t.eq_ignore_ascii_case("clean"));
    segments
        .iter()
        .any(|segment| segment_matches(segment, mentions_clean, depth))
}

// ── NeverList ─────────────────────────────────────────────────────────────────

/// A never-list that flags catastrophic commands by parsing program + target.
#[derive(Debug, Clone)]
pub struct NeverList {
    /// Extra user-defined glob patterns (applied after the structural check).
    extra_patterns: Vec<String>,
    /// Compiled form of `extra_patterns`.
    extra_set: GlobSet,
}

/// Names of the daemon's bearer token and its control files. A command that
/// mentions one always needs approval: damping, not a boundary (the name can
/// be assembled at run time).
const DAEMON_STATE_NAMES: &[&str] = &[
    "api-token",
    "owner-key",
    "control-latch",
    "view-privileges",
    "bot.db",
    "policy.json",
    "identity.key",
    "secrets.enc",
];

/// The CLI sign-in / config stores and the installed runtimes, as canonical locations. A command
/// that names one of them (to replace an engine's executable, read its token or change its hooks)
/// always needs approval, even at level Full. Only the REAL stores match: the service user's
/// home-level directories (`~/.claude`, `~/.codex`, the opencode / cursor config and data dirs
/// under home, XDG and `%APPDATA%` / `%LOCALAPPDATA%`) and `<data>/runtimes`. A project's own
/// `.claude/` or `.codex/` folder in any other directory is ordinary work and does not match.
/// Damping, not a boundary: a name can be assembled at run time, and a bot the owner gave an
/// unrestricted shell as the same OS user can still reach these files.
#[derive(Debug, Clone, Default)]
pub struct CliStores {
    /// Normalised absolute store roots (directories or single files).
    roots: Vec<String>,
    /// Normalised home directories (for `~` / `$HOME` spellings).
    homes: Vec<String>,
}

/// Home-level store locations, relative to a home directory (lower case, `/` separated).
const HOME_STORES: &[&str] = &[
    ".claude",
    ".claude.json",
    ".codex",
    ".cursor",
    ".cursor-agent",
    ".gemini",
    ".qwen",
    ".kiro",
    ".copilot",
    ".kimi-code",
    ".config/opencode",
    ".config/claude",
    ".config/cursor",
    ".config/cursor-agent",
    ".local/share/opencode",
    ".local/share/cursor-agent",
    "appdata/roaming/opencode",
    "appdata/roaming/cursor",
    "appdata/local/opencode",
    "appdata/local/cursor",
    "appdata/local/cursor-agent",
];

/// Spellings of the home directory before shell expansion (lower case).
const HOME_SPELLINGS: &[&str] = &[
    "~",
    "$home",
    "${home}",
    "$env:home",
    "$env:userprofile",
    "${env:userprofile}",
    "%userprofile%",
    "%home%",
];

static DATA_DIR: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Tell the never-list where the daemon's data directory is, so `<data>/runtimes` (the CLIs
/// Praxi installed itself) is protected by location.
pub fn set_data_dir(dir: &std::path::Path) {
    if let Ok(mut slot) = DATA_DIR.write() {
        *slot = Some(norm_path(&dir.to_string_lossy()));
    }
}

/// Lower-case, `/`-separated, `.` and `..` resolved lexically, no trailing slash.
fn norm_path(raw: &str) -> String {
    let unified = raw.replace('\\', "/").to_lowercase();
    let (prefix, rest) = match unified.find(':') {
        Some(1) => (&unified[..2], &unified[2..]),
        _ => ("", unified.as_str()),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("{prefix}/{}", parts.join("/"))
}

fn is_absolute(raw: &str) -> bool {
    let b = raw.as_bytes();
    raw.starts_with('/')
        || raw.starts_with('\\')
        || (b.len() >= 3 && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\'))
}

fn env_dir(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty() && is_absolute(v))
        .map(|v| norm_path(&v))
}

impl CliStores {
    /// The stores of the process' own user, from its environment.
    #[must_use]
    pub fn from_env() -> Self {
        let homes: Vec<String> = ["HOME", "USERPROFILE"]
            .iter()
            .filter_map(|k| env_dir(k))
            .collect();
        let mut roots = Vec::new();
        for home in &homes {
            roots.extend(HOME_STORES.iter().map(|s| format!("{home}/{s}")));
        }
        for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
            roots.extend(env_dir(key));
        }
        if let Some(dir) = env_dir("XDG_CONFIG_HOME") {
            roots.extend(
                ["opencode", "claude", "cursor", "cursor-agent"].map(|n| format!("{dir}/{n}")),
            );
        }
        if let Some(dir) = env_dir("XDG_DATA_HOME") {
            roots.extend(["opencode", "cursor-agent"].map(|n| format!("{dir}/{n}")));
        }
        for key in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(dir) = env_dir(key) {
                roots.extend(["opencode", "cursor", "cursor-agent"].map(|n| format!("{dir}/{n}")));
            }
        }
        if let Ok(slot) = DATA_DIR.read()
            && let Some(data) = slot.as_ref()
        {
            roots.push(format!("{data}/runtimes"));
        }
        Self { roots, homes }
    }

    /// Stores for explicit homes (tests, or a caller that knows the service user).
    #[must_use]
    pub fn for_homes(homes: &[&str], data_dir: Option<&str>) -> Self {
        let homes: Vec<String> = homes.iter().map(|h| norm_path(h)).collect();
        let mut roots: Vec<String> = homes
            .iter()
            .flat_map(|h| HOME_STORES.iter().map(move |s| format!("{h}/{s}")))
            .collect();
        if let Some(data) = data_dir {
            roots.push(format!("{}/runtimes", norm_path(data)));
        }
        Self { roots, homes }
    }

    fn contains(&self, path: &str) -> bool {
        self.roots.iter().any(|root| {
            path == root
                || path
                    .strip_prefix(root.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
    }

    /// Whether one token of a command names a store. `cwd` resolves a relative token; without
    /// one a relative token cannot be placed and does not match.
    fn token_names_store(&self, token: &str, cwd: Option<&str>) -> bool {
        let lower = token.to_lowercase();
        // The home spellings expand to every known home.
        for spelling in HOME_SPELLINGS {
            if let Some(rest) = lower.strip_prefix(spelling)
                && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
            {
                return self
                    .homes
                    .iter()
                    .any(|h| self.contains(&norm_path(&format!("{h}{rest}"))));
            }
        }
        if lower.contains('$') || lower.contains('%') || lower.contains('`') {
            // Any other variable cannot be resolved here.
            return false;
        }
        if is_absolute(token) {
            return self.contains(&norm_path(token));
        }
        cwd.is_some_and(|cwd| self.contains(&norm_path(&format!("{cwd}/{token}"))))
    }

    /// Whether `command` (run in `cwd`, when known) names a store.
    #[must_use]
    pub fn names_store(&self, command: &str, cwd: Option<&str>) -> bool {
        let cwd = cwd.filter(|c| is_absolute(c)).map(norm_path);
        let lower = command.to_lowercase();
        // The runtime directories Praxi installs have specific names that no project uses.
        if [
            "runtimes/node-v",
            "runtimes/npm-global",
            "runtimes\\node-v",
            "runtimes\\npm-global",
        ]
        .iter()
        .any(|n| lower.contains(n))
        {
            return true;
        }
        command
            .split(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        ';' | '|' | '&' | '<' | '>' | '(' | ')' | '"' | '\'' | '=' | ','
                    )
            })
            .filter(|t| !t.is_empty())
            .any(|t| self.token_names_store(t, cwd.as_deref()))
    }
}

/// Subcommands of the Praxi CLI (`bot`) that carry authority: pairing and invites, host
/// administration, keys, providers, approvals, tool servers, and starting a daemon.
const PRAXI_CLI_AUTHORITY: &[&str] = &["host", "secrets", "providers", "approve", "mcp", "serve"];

/// Whether `token` is the Praxi CLI binary: `bot` / `bot.exe` (any directory), or the absolute
/// path of the binary that is running this process, under any name.
fn is_praxi_binary(token: &str) -> bool {
    let norm = token.replace('\\', "/");
    let base = norm.rsplit('/').next().unwrap_or_default().to_lowercase();
    if base == "bot" || base == "bot.exe" {
        return true;
    }
    std::env::current_exe().is_ok_and(|exe| {
        let exe = exe.to_string_lossy().replace('\\', "/");
        norm.contains('/') && norm.eq_ignore_ascii_case(&exe)
    })
}

/// A command that runs the Praxi CLI with an authority-bearing subcommand or `--data-dir`
/// (which points the CLI at a data directory and the owner key stored in it). The CLI reads the
/// owner key file itself, so these never auto-run from a bot shell, at any level. Damping, not a
/// boundary: the binary can be reached under a name assembled at run time.
fn runs_praxi_cli(raw_command: &str) -> bool {
    raw_command
        .split([';', '|', '&', '\n', '\r'])
        .any(|segment| {
            let tokens: Vec<&str> = segment
                .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '(' | ')' | '`'))
                .filter(|t| !t.is_empty())
                .collect();
            tokens
                .iter()
                .position(|t| is_praxi_binary(t))
                .is_some_and(|i| {
                    tokens[i + 1..].iter().any(|t| {
                        let t = t.to_lowercase();
                        PRAXI_CLI_AUTHORITY.contains(&t.as_str())
                            || t == "--data-dir"
                            || t.starts_with("--data-dir=")
                    })
                })
        })
}

fn names_daemon_state(raw_command: &str) -> bool {
    let lower = raw_command.to_lowercase();
    DAEMON_STATE_NAMES.iter().any(|name| lower.contains(name))
}

impl NeverList {
    /// Build a [`NeverList`] from an optional list of additional glob patterns.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::InvalidPattern`] if any pattern is malformed.
    pub fn from_patterns(extra: &[&str]) -> Result<Self, PolicyError> {
        let mut builder = GlobSetBuilder::new();
        for &pat in extra {
            let glob = Glob::new(pat).map_err(|e| PolicyError::InvalidPattern(e.to_string()))?;
            builder.add(glob);
        }
        let extra_set = builder
            .build()
            .map_err(|e| PolicyError::InvalidPattern(e.to_string()))?;
        Ok(Self {
            extra_patterns: extra.iter().map(|s| (*s).to_owned()).collect(),
            extra_set,
        })
    }

    /// Return the built-in default never-list (no extra patterns).
    #[must_use]
    pub fn default_list() -> Self {
        Self {
            extra_patterns: Vec::new(),
            extra_set: GlobSet::empty(),
        }
    }

    /// Return `true` if the command requires approval regardless of policy.
    ///
    /// The command is normalised (lower-cased, trimmed), parsed into segments
    /// of program + arguments, and matches if:
    /// - a program is unconditionally destructive (e.g. `bcdedit`, `mkfs*`), OR
    /// - a recursive delete targets a protected path (e.g. `rm -rf /etc`), OR
    /// - any user-defined extra pattern matches the full normalised command.
    #[must_use]
    pub fn matches(&self, raw_command: &str) -> bool {
        self.matches_in(raw_command, None)
    }

    /// [`Self::matches`] for a command run in `cwd`: a relative path argument is placed there
    /// before it is compared with the CLI sign-in stores.
    #[must_use]
    pub fn matches_in(&self, raw_command: &str, cwd: Option<&str>) -> bool {
        // The structural check is case-preserving (wrapper options such as
        // `sudo -S` vs `-s` differ); extra glob patterns use the normalised form.
        structural_match(raw_command.trim(), 0)
            || names_daemon_state(raw_command)
            || runs_praxi_cli(raw_command)
            || CliStores::from_env().names_store(raw_command, cwd)
            || self.extra_set.is_match(Self::normalize(raw_command))
    }

    /// Normalise a raw command string before matching.
    ///
    /// Converts to lower case and strips leading/trailing whitespace.
    #[must_use]
    pub fn normalize(command: &str) -> String {
        command.trim().to_lowercase()
    }

    /// Return the list of user-defined extra patterns.
    #[must_use]
    pub fn extra_patterns(&self) -> &[String] {
        &self.extra_patterns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_praxi_cli_with_authority_needs_approval_from_a_bot_shell() {
        let list = NeverList::default_list();
        for cmd in [
            "/opt/praxi/bin/bot host invite --role desktop",
            "bot --data-dir /var/lib/praxi host invite --role desktop",
            "cd /tmp && bot host admin --all-desktops",
            "sudo bot secrets list",
            "bot.exe serve --host",
            r"C:\praxi\bin\bot.exe providers",
            "bot status --data-dir=/var/lib/praxi",
        ] {
            assert!(list.matches(cmd), "{cmd}");
        }
        for cmd in ["echo hello bot", "ls /opt/praxi/bin", "cargo test host"] {
            assert!(!list.matches(cmd), "{cmd}");
        }
    }

    fn check(cmd: &str) -> bool {
        NeverList::default_list().matches(cmd)
    }

    // ── Must match ────────────────────────────────────────────────────────────

    #[test]
    fn matches_commands_that_name_installed_clis_and_their_sign_in_stores() {
        // Home-level stores and the installed runtimes, by explicit location.
        let stores = CliStores::for_homes(&["/home/praxi", r"C:\Users\me"], Some("/var/lib/praxi"));
        for (command, cwd) in [
            ("cat /home/praxi/.claude/.credentials.json", None),
            ("cat ~/.claude/.credentials.json", None),
            ("cat $HOME/.codex/auth.json", None),
            ("cat ${HOME}/.codex/auth.json", None),
            (r"type %USERPROFILE%\.codex\auth.json", None),
            ("cat $env:USERPROFILE/.claude.json", None),
            (r"copy x C:\Users\me\.claude\settings.json", None),
            ("cat ~/.local/share/opencode/auth.json", None),
            ("cat ../.codex/auth.json", Some("/home/praxi/work")),
            // Qwen Code: OAuth token and settings (hooks / MCP servers) under ~/.qwen
            ("cat ~/.qwen/oauth_creds.json", None),
            ("cat ${HOME}/.qwen/settings.json", None),
            ("cat .qwen/oauth_creds.json", Some("/home/praxi")),
            // Kiro CLI: MCP servers, hooks and permission rules under ~/.kiro
            ("cat ~/.kiro/settings/mcp.json", None),
            ("rm ${HOME}/.kiro/settings/permissions.yaml", None),
            // Copilot CLI: config.json (accounts, plaintext token without a keychain) and
            // settings.json (hooks, MCP servers) under ~/.copilot
            ("cat ~/.copilot/config.json", None),
            (r"type %USERPROFILE%\.copilot\settings.json", None),
            ("cat .copilot/config.json", Some("/home/praxi")),
            // Kimi Code CLI: OAuth token and config.toml (providers, API keys, hooks, MCP)
            ("cat ~/.kimi-code/credentials/kimi-code.json", None),
            ("cat ${HOME}/.kimi-code/config.toml", None),
            ("cat .kimi-code/config.toml", Some("/home/praxi")),
            (
                "cp /tmp/evil /var/lib/praxi/runtimes/npm-global/bin/claude",
                None,
            ),
            (
                "echo hi >> /var/lib/praxi/runtimes/node-v22.1.0-linux-x64/bin/npm",
                None,
            ),
            // cwd = home: a relative store path is the real store
            ("cat .claude/.credentials.json", Some("/home/praxi")),
            ("cat .codex/auth.json", Some("/home/praxi")),
        ] {
            assert!(stores.names_store(command, cwd), "{command} in {cwd:?}");
        }
    }

    #[test]
    fn a_project_local_cli_folder_is_ordinary_work() {
        let stores = CliStores::for_homes(&["/home/praxi"], Some("/var/lib/praxi"));
        for (command, cwd) in [
            // inside a repository, absolute or relative
            ("cat /home/praxi/work/app/.claude/settings.json", None),
            ("cat .claude/settings.json", Some("/home/praxi/work/app")),
            ("ls .codex .cursor", Some("/srv/repo")),
            ("cat .claude/settings.json", None),
            (
                "git add .claude/commands/review.md",
                Some("/home/praxi/work/app"),
            ),
            // look-alike names that are not the stores
            ("cat /home/praxi/work/.claude/x", None),
            ("cat ../app/.claude/x", Some("/home/praxi/work/other")),
            ("cargo build", None),
            ("ls runtimes", None),
            ("cat notes/claude-code.md", None),
            // an unresolved variable that is not a home spelling is never guessed
            ("cat $PROJECT/.claude/x", None),
        ] {
            assert!(!stores.names_store(command, cwd), "{command} in {cwd:?}");
        }
    }

    #[test]
    fn matches_commands_that_name_daemon_credentials_or_control_files() {
        for command in [
            "cat ~/.praxi/api-token",
            r"type C:\Users\me\AppData\Roaming\praxi\API-TOKEN",
            "curl -H \"Authorization: Bearer $(cat $DATA/api-token)\" http://127.0.0.1:1/v1/approvals",
            "cp control-latch.json /tmp/x",
            "sqlite3 bot.db 'select * from approvals'",
            "tar czf x.tgz bot.db-wal",
            r"Get-Content $env:APPDATA\praxi\policy.json",
            "sh -c 'cat api-token'",
        ] {
            assert!(check(command), "{command}");
        }
        assert!(!check("ls /tmp/work"));
        assert!(!check("cat notes.txt"));
    }

    #[test]
    fn matches_rm_rf_root() {
        assert!(check("rm -rf /"));
    }

    #[test]
    fn matches_rm_rf_root_glob() {
        assert!(check("rm -rf /*"));
    }

    #[test]
    fn matches_rm_rf_home_tilde() {
        assert!(check("rm -rf ~"));
        assert!(check("rm -rf ~/"));
        assert!(check("rm -rf $HOME"));
    }

    #[test]
    fn matches_rm_no_preserve_root() {
        assert!(check("rm -rf --no-preserve-root /"));
        assert!(check("rm --no-preserve-root /bin"));
    }

    #[test]
    fn matches_sudo_rm_rf_etc() {
        assert!(check("sudo rm -rf /etc"));
        assert!(check("sudo -u root rm -rf /usr"));
    }

    #[test]
    fn matches_rm_other_system_dirs() {
        for dir in [
            "/bin", "/boot", "/usr", "/var", "/lib64", "/sbin", "/System",
        ] {
            assert!(check(&format!("rm -rf {dir}")), "{dir}");
        }
    }

    #[test]
    fn matches_rm_recursive_spellings() {
        assert!(check("rm -r /etc"));
        assert!(check("rm -fr /etc"));
        assert!(check("rm -R /etc"));
        assert!(check("rm --recursive --force /etc"));
        assert!(check("rm -rf -- /etc"));
        assert!(check("/bin/rm -rf /etc"));
    }

    #[test]
    fn matches_rm_dotdot_escape_to_system_dir() {
        assert!(check("rm -rf /tmp/../etc"));
    }

    #[test]
    fn matches_rm_home_directory_roots() {
        assert!(check("rm -rf /home/alice"));
        assert!(check("rm -rf /Users/alice"));
        assert!(check("rm -rf /root"));
    }

    #[test]
    fn matches_any_protected_target_among_several() {
        assert!(check("rm -rf ./build /etc"));
    }

    #[test]
    fn matches_powershell_remove_item_windows() {
        assert!(check("Remove-Item -Recurse -Force C:\\Windows"));
        assert!(check("Remove-Item -Force -Recurse C:\\Windows\\System32"));
        assert!(check("Remove-Item -Recurse -Force C:\\"));
        assert!(check("Remove-Item -Recurse -Force \"C:\\Program Files\""));
        assert!(check(
            "Remove-Item -Recurse -Force \"C:\\Program Files (x86)\""
        ));
        assert!(check("ri -r D:\\"));
    }

    #[test]
    fn matches_windows_home_roots() {
        assert!(check("Remove-Item -Recurse -Force C:\\Users\\alice"));
        assert!(check("Remove-Item -Recurse $env:USERPROFILE"));
        assert!(check("rd /s /q %USERPROFILE%"));
    }

    #[test]
    fn matches_cmd_recursive_deletes() {
        assert!(check("rd /s /q C:\\"));
        assert!(check("rmdir /s /q C:\\Windows"));
        assert!(check("del /s /q C:\\*"));
        assert!(check("rd /s /q D:\\"));
    }

    #[test]
    fn matches_nested_shell_payloads() {
        assert!(check("bash -c \"rm -rf /\""));
        assert!(check("cmd /c \"rd /s /q C:\\\""));
        assert!(check(
            "powershell -NoProfile -Command \"Remove-Item -Recurse C:\\Windows\""
        ));
    }

    #[test]
    fn matches_chained_commands() {
        assert!(check("cd /tmp && rm -rf /etc"));
        assert!(check("echo hi; rm -rf /"));
    }

    #[test]
    fn matches_unconditional_disk_tools() {
        assert!(check("Format-Volume -DriveLetter D"));
        assert!(check("Clear-Disk -Number 0 -RemoveData"));
        assert!(check("mkfs.ext4 /dev/sda1"));
        assert!(check("mkfs /dev/sdb1"));
        assert!(check("format d: /q"));
        assert!(check("bcdedit /deletevalue {current} nx"));
        assert!(check("grub-install /dev/sda"));
        assert!(check("cipher /w:c:\\"));
        assert!(check("dd if=/dev/zero of=/dev/sda"));
    }

    #[test]
    fn matches_diskpart_clean() {
        assert!(check("echo clean | diskpart"));
        assert!(check("diskpart /s wipe.txt"));
    }

    // ── Must NOT match ────────────────────────────────────────────────────────

    #[test]
    fn does_not_match_relative_rm() {
        assert!(!check("rm -rf ./build"));
        assert!(!check("rm -rf ./build/artifacts"));
        assert!(!check("rm -rf build"));
    }

    #[test]
    fn does_not_match_rm_tmp() {
        assert!(!check("rm -rf /tmp/x"));
        assert!(!check("rm -rf /tmp/my_project"));
        assert!(!check("rm -rf /tmp"));
    }

    #[test]
    fn does_not_match_unix_paths_that_look_like_windows_switches() {
        // `/s` and `/etc/..`-style tokens are targets for Unix programs.
        assert!(!check("rm -rf /s"));
        assert!(!check("rm -rf /tmp"));
    }

    #[test]
    fn does_not_match_home_subdirectories() {
        assert!(!check("rm -rf /home/user/project"));
        assert!(!check("rm -rf ~/project/target"));
        assert!(!check("rm -rf $HOME/project"));
    }

    #[test]
    fn does_not_match_non_recursive_rm_of_protected_target() {
        assert!(!check("rm /etc/hosts.bak"));
        assert!(!check("rm -f /var"));
    }

    #[test]
    fn does_not_match_windows_project_dirs() {
        assert!(!check("Remove-Item -Recurse C:\\Users\\me\\project\\dist"));
        assert!(!check("rd /s /q C:\\Users\\me\\project\\dist"));
        assert!(!check("del /s /q build\\*"));
        assert!(!check("Remove-Item -Recurse .\\build"));
    }

    #[test]
    fn does_not_match_safe_programs() {
        assert!(!check("ls -la /home/user"));
        assert!(!check("echo rm -rf /"));
        assert!(!check("git commit -m \"rm -rf /\""));
        assert!(!check("cat /etc/hosts"));
        assert!(!check("dd if=/dev/zero of=/dev/null bs=1M count=1"));
        assert!(!check("dd if=a.img of=b.img"));
        assert!(!check("cipher /e secret.txt"));
        assert!(!check("diskpart /?"));
        assert!(!check(""));
    }

    // ── Review 2026-11-07 regressions ─────────────────────────────────────────

    #[test]
    fn matches_sudo_env_flags_case_sensitive() {
        assert!(check("sudo -n rm -rf /"));
        assert!(check("sudo -S rm -rf /"));
        assert!(check("sudo -H rm -rf /etc"));
        assert!(check("sudo -E -n -u root rm -rf /usr"));
        assert!(check("sudo -u root -- rm -rf /"));
        assert!(check("doas -n rm -rf /"));
        assert!(check("env -i FOO=1 rm -rf /"));
        assert!(check("env -u HOME rm -rf /"));
        assert!(check("env -S 'rm -rf /'"));
        assert!(check("env -S\"rm -rf /etc\""));
        assert!(check("env --split-string=\"rm -rf /\""));
        assert!(check("nice -n 5 rm -rf /"));
    }

    #[test]
    fn matches_subshells_groups_and_substitution() {
        assert!(check("(rm -rf /etc)"));
        assert!(check("( cd /tmp && rm -rf / )"));
        assert!(check("{ rm -rf /; }"));
        assert!(check("echo $(rm -rf /)"));
        assert!(check("echo `rm -rf /`"));
        assert!(check("echo \"x $(rm -rf /etc) y\""));
        assert!(check("echo $(echo $(rm -rf /))"));
        assert!(check("diff <(rm -rf /) b"));
    }

    #[test]
    fn matches_eval_and_exec_style_wrappers() {
        assert!(check("eval rm -rf /"));
        assert!(check("eval \"rm -rf /etc\""));
        assert!(check("timeout 5 rm -rf /"));
        assert!(check("timeout -s KILL 5 rm -rf /"));
        assert!(check("timeout --signal=KILL 5s rm -rf /"));
        assert!(check("xargs rm -rf /"));
        assert!(check("xargs -n 1 rm -rf /etc"));
        assert!(check("busybox rm -rf /"));
        assert!(check("setsid rm -rf /"));
        assert!(check("stdbuf -oL rm -rf /"));
        assert!(check("chroot /mnt rm -rf /"));
        assert!(check("nohup rm -rf / &"));
        assert!(check("sh -c 'eval rm -rf /'"));
    }

    #[test]
    fn matches_globbed_protected_targets() {
        assert!(check("rm -rf /e*"));
        assert!(check("rm -rf /[e]tc"));
        assert!(check("rm -rf /et?"));
        assert!(check("rm -rf /{etc,usr}"));
        assert!(check("rm -rf /us*/lib"));
        assert!(check("rm -rf /ho*/alice"));
        assert!(check(r"Remove-Item -Recurse -Force C:\Win*"));
        assert!(check(r"rd /s /q C:\Prog*"));
        assert!(check(r"rd /s /q C:\Pro?ram?ata"));
    }

    #[test]
    fn matches_msys_wsl_and_win32_namespace_paths() {
        assert!(check("rm -rf /c/Windows"));
        assert!(check("rm -rf /c/"));
        assert!(check("rm -rf /c/*"));
        assert!(check("rm -rf /mnt/c/Windows"));
        assert!(check("rm -rf /mnt/c"));
        assert!(check("rm -rf /mnt/d/"));
        assert!(check("rm -rf /cygdrive/c/Windows/System32"));
        assert!(check("rm -rf /c/Users/alice"));
        assert!(check(r"Remove-Item -Recurse \\?\C:\Windows"));
        assert!(check(r"rd /s /q %SystemRoot%"));
        assert!(check("format.com c:"));
        assert!(check("format.exe d: /q"));
    }

    #[test]
    fn new_forms_do_not_overmatch() {
        assert!(!check("sudo -n ls /etc"));
        assert!(!check("sudo -n rm -rf ./build"));
        assert!(!check("env FOO=1 make clean"));
        assert!(!check("timeout 5 rm -rf /tmp/x"));
        assert!(!check("(cd build && make)"));
        assert!(!check("{ echo hi; }"));
        assert!(!check("echo $(date)"));
        assert!(!check("echo '$(rm -rf /)'"));
        assert!(!check("echo `date`"));
        assert!(!check("eval echo hi"));
        assert!(!check("xargs echo /etc"));
        assert!(!check("rm -rf /tmp/*"));
        assert!(!check("rm -rf /t*"));
        assert!(!check("rm -rf /c/Users/alice/project"));
        assert!(!check("rm -rf /mnt/data"));
        assert!(!check("rm -rf /mnt/c/Users/alice/project"));
        assert!(!check("rm -rf /s"));
        assert!(!check("rm -rf ${HOME}/project"));
        assert!(!check("git log --format={x} -n 3"));
    }

    // ── Custom extra patterns ─────────────────────────────────────────────────

    #[test]
    fn custom_pattern_matches() {
        let list = NeverList::from_patterns(&["my-dangerous-cmd*"]).unwrap();
        assert!(list.matches("my-dangerous-cmd --force"));
        assert!(!list.matches("safe-cmd"));
        assert_eq!(list.extra_patterns(), &["my-dangerous-cmd*"]);
    }

    #[test]
    fn invalid_custom_pattern_is_rejected() {
        assert!(NeverList::from_patterns(&["[unclosed"]).is_err());
    }

    #[test]
    fn norm_path_normalizes_separators_case_dots_and_unicode() {
        let long_tail = "segment/".repeat(40);
        let long_in = format!("C:\\Root\\{long_tail}File.TXT");
        let long_expected = format!("c:/root/{long_tail}file.txt");
        let cases = [
            ("", "/"),
            ("/", "/"),
            ("\\", "/"),
            ("C:\\", "c:/"),
            ("C:", "c:/"),
            (r"C:\Users\Alice\..\Bob\.\Notes", "c:/users/bob/notes"),
            ("/var/lib/../log/./praxi/", "/var/log/praxi"),
            ("/../../etc", "/etc"),
            (r"C:\..\..\Windows", "c:/windows"),
            (r"\\server\share\dir\", "/server/share/dir"),
            (r"C:\Kullanıcı\Ödev\", "c:/kullanıcı/ödev"),
            (long_in.as_str(), long_expected.as_str()),
        ];
        for (raw, expected) in cases {
            assert_eq!(norm_path(raw), expected, "raw={raw:?}");
        }
    }

    #[test]
    fn is_absolute_distinguishes_unix_windows_unc_and_relative_paths() {
        let cases = [
            ("", false),
            (".", false),
            ("./rel/path", false),
            ("../up", false),
            ("relative\\win", false),
            ("C:", false),
            ("c:relative", false),
            ("/", true),
            ("/etc/passwd", true),
            ("\\", true),
            (r"\\server\share", true),
            ("C:\\", true),
            ("C:/Users/me", true),
            (r"d:\data", true),
        ];
        for (raw, expected) in cases {
            assert_eq!(is_absolute(raw), expected, "raw={raw:?}");
        }
    }
}

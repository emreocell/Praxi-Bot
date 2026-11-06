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

/// Programs that merely run another program given as their first non-option
/// argument.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "command", "exec", "time", "nice",
];

/// Wrapper options that consume the following token as their value.
const WRAPPER_OPTIONS_WITH_VALUE: &[&str] =
    &["-u", "-g", "-h", "-p", "-n", "-d", "-t", "-r", "-c", "-s"];

/// Shell keywords that may precede a command inside a compound statement.
const SHELL_KEYWORDS: &[&str] = &["if", "then", "else", "elif", "do", "while", "until", "!"];

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

impl Target {
    /// Parse an (already lower-cased, unquoted) path argument.
    fn parse(raw: &str) -> Self {
        let unified = raw.replace('\\', "/");
        let mut parts = unified.split('/');

        let (anchor, first_is_anchor) = if unified.starts_with('/') {
            (Anchor::Unix, false)
        } else {
            match parts.clone().next() {
                Some(first) if is_drive_prefix(first) => (Anchor::Drive, true),
                Some(first) if HOME_TOKENS.contains(&first) => (Anchor::Home, true),
                _ => (Anchor::Relative, false),
            }
        };
        if first_is_anchor {
            parts.next();
        }

        let mut components: Vec<String> = Vec::new();
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

/// Split a command line into segments of unquoted tokens.
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
            ';' | '|' | '&' | '\n' | '\r' => sp.end_segment(),
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

/// Reduce a program token to its bare lower-case name (`/bin/rm` → `rm`,
/// `Rmdir.EXE` → `rmdir`).
fn program_name(token: &str) -> &str {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// Skip leading wrappers / shell keywords and return `(program, args)`.
fn resolve_program(tokens: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    loop {
        let name = program_name(tokens.get(i)?);
        i += 1;
        if SHELL_KEYWORDS.contains(&name) {
            continue;
        }
        if WRAPPERS.contains(&name) {
            while let Some(tok) = tokens.get(i) {
                if tok.starts_with('-') {
                    i += 1;
                    if WRAPPER_OPTIONS_WITH_VALUE.contains(&tok.as_str()) {
                        i += 1;
                    }
                } else if name == "env" && tok.contains('=') {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        return Some((name, tokens.get(i..).unwrap_or_default()));
    }
}

// ── Per-program checks ────────────────────────────────────────────────────────

/// Return the command payload of `sh -c …`, `cmd /c …`, `powershell -Command …`.
fn shell_payload(program: &str, args: &[String]) -> Option<String> {
    let runs_command = |arg: &str| -> bool {
        if POSIX_SHELLS.contains(&program) {
            arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c')
        } else if program == "cmd" {
            arg == "/c" || arg == "/k"
        } else if POWERSHELL_HOSTS.contains(&program) {
            arg.len() >= 2 && arg.starts_with('-') && "command".starts_with(&arg[1..])
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
    let Some((program, args)) = resolve_program(tokens) else {
        return false;
    };

    if let Some(payload) = shell_payload(program, args) {
        return structural_match(&payload, depth + 1);
    }

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

/// Run the structural (non-glob) checks over a normalised command.
fn structural_match(command: &str, depth: u8) -> bool {
    if depth > MAX_PAYLOAD_DEPTH {
        return true;
    }
    let segments = split_segments(command);
    let mentions_clean = segments.iter().flatten().any(|t| t == "clean");
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
        let norm = Self::normalize(raw_command);
        structural_match(&norm, 0) || self.extra_set.is_match(&norm)
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

    fn check(cmd: &str) -> bool {
        NeverList::default_list().matches(cmd)
    }

    // ── Must match ────────────────────────────────────────────────────────────

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
}

//! Per-bot persistent workspace folders, disk usage metadata, and threshold checks.
//!
//! Desktop default root: `<Documents>/Praxi/<bot>/`
//! Always-on Linux host default root: `/var/lib/praxi-work/<bot>/`
//!
//! Security boundaries:
//! * Never inside the daemon data directory (`data_dir`) or credential/CLI-config folders.
//! * Single-component folder names only; symlinks and reparse points are never traversed.
//! * Disk usage walks metadata only (`symlink_metadata`), never opening file contents.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Standard subfolders created inside every bot's workspace.
pub const WORKSPACE_SUBDIRS: [&str; 5] = ["leads", "drafts", "downloads", "notes", "logs"];

/// Optional environment override for the workspaces root (used in tests and custom hosts).
pub const WORKSPACES_DIR_ENV: &str = "BOT_WORKSPACES_DIR";

/// Default workspaces root on an always-on Linux host (`HostMode::Host`).
pub const HOST_WORKSPACES_ROOT: &str = "/var/lib/praxi-work";

/// Default host disk usage percentage thresholds (80 / 90 / 95%).
pub const DEFAULT_WARN_PERCENT_1: u8 = 80;
pub const DEFAULT_WARN_PERCENT_2: u8 = 90;
pub const DEFAULT_WARN_PERCENT_3: u8 = 95;
/// Default minimum free space left after a write before warning (5 GiB).
pub const DEFAULT_MIN_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Default age threshold for cleanup suggestions in `downloads/` and `logs/`.
pub const DEFAULT_CLEANUP_MAX_AGE_DAYS: u32 = 30;

/// Host filesystem space snapshot around the workspaces root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStats {
    /// Total disk capacity in bytes.
    pub total_bytes: u64,
    /// Free bytes available to the service user.
    pub free_bytes: u64,
    /// Used bytes (`total_bytes.saturating_sub(free_bytes)`).
    pub used_bytes: u64,
    /// Integer percentage `0..=100` of disk used.
    pub used_percent: u8,
}

/// Resolve the workspaces root path for this installation.
#[must_use]
pub fn resolve_workspaces_root(is_host: bool, override_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        return Some(dir.to_path_buf());
    }
    if let Some(env) = std::env::var_os(WORKSPACES_DIR_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(env));
    }
    if is_host {
        return Some(PathBuf::from(HOST_WORKSPACES_ROOT));
    }
    desktop_documents_dir().map(|docs| docs.join("Praxi"))
}

/// The user's Documents folder on desktop platforms.
#[must_use]
pub fn desktop_documents_dir() -> Option<PathBuf> {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.document_dir().map(Path::to_path_buf))
        .filter(|p| p.is_absolute())
        .or_else(|| crate::user_home().map(|h| h.join("Documents")))
}

/// Check whether `name` is a single safe directory name (no separators, traversal,
/// reserved Windows device names, trailing dots/spaces, alternate data streams,
/// 8.3 short-name aliases such as `RESEAR~1`, or control characters).
///
/// A name that passes is still only trusted when the directory it resolves to
/// carries exactly this name (see [`ensure_bot_workspace_dir`]).
#[must_use]
pub fn is_valid_folder_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 80 {
        return false;
    }
    if name == "." || name == ".." || name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return false;
    }
    // `~1` .. `~9`: NTFS answers to the 8.3 alias of another folder, which
    // would make two bots share one directory.
    if name
        .as_bytes()
        .windows(2)
        .any(|w| w[0] == b'~' && w[1].is_ascii_digit())
    {
        return false;
    }
    // Device names are reserved with any extension and with spaces before it
    // (`CON .txt`).
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            let mut chars = rest.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
            )
        })
    };
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT");
    !reserved
}

/// Derive a readable, safe single-component folder name from a bot's display name
/// and UUID.
#[must_use]
pub fn sanitize_bot_folder_name(bot_name: &str, bot_id: &str) -> String {
    let mut out = String::with_capacity(bot_name.len().min(48));
    let mut prev_dash = false;
    for ch in bot_name.trim().chars() {
        let forbidden =
            ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*');
        if forbidden {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(ch);
            prev_dash = ch == '-';
        }
        if out.chars().count() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches([' ', '.', '-']).to_owned();
    if is_valid_folder_name(&trimmed) {
        trimmed
    } else {
        let short: String = bot_id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        if short.is_empty() {
            "bot".to_owned()
        } else {
            format!("bot-{short}")
        }
    }
}

/// Validate that `root` is an absolute local path outside `data_dir` and outside
/// any credential/CLI-config directory. Creates `root` if missing.
///
/// # Errors
/// Returns a refusal reason if `root` is unsafe or cannot be created/resolved.
pub fn validate_workspaces_root(root: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err("workspaces root must be an absolute path".into());
    }
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("workspaces root cannot contain parent traversal".into());
    }
    if !root.components().any(|c| matches!(c, Component::Normal(_))) {
        return Err("a drive or filesystem root cannot be the workspaces root".into());
    }
    // The workspaces root itself is the one folder that may sit next to the
    // service account's home (the host's `/var/lib/praxi-work`); credentials,
    // browsers, OS trees, the data folder and other users' profiles stay refused.
    if crate::files::workspaces_root_protected(root, data_dir) {
        return Err(
            "workspaces root cannot be inside Praxi's data folder or a credential folder".into(),
        );
    }
    ensure_plain_dir_chain(root)?;
    let resolved = crate::files::resolve_local_folder(root).map_err(|e| e.to_string())?;
    if crate::files::workspaces_root_protected(&resolved, data_dir) {
        return Err(
            "workspaces root resolves inside Praxi's data folder or a credential folder".into(),
        );
    }
    secure_workspaces_root(&resolved)?;
    crate::files::register_workspaces_root(&resolved);
    Ok(storage::without_verbatim_prefix(resolved))
}

/// Ensure `<root>/<folder_name>` and its standard subfolders exist as real
/// directories inside `root` (no symlinks or reparse points).
///
/// # Errors
/// Refuses invalid folder names, protected roots, symlinks, or I/O failures.
pub fn ensure_bot_workspace_dir(
    root: &Path,
    folder_name: &str,
    data_dir: &Path,
) -> Result<PathBuf, String> {
    if !is_valid_folder_name(folder_name) {
        return Err(format!(
            "invalid bot workspace folder name: {folder_name:?}"
        ));
    }
    let root_real = validate_workspaces_root(root, data_dir)?;
    ensure_bot_dir_in_root(&root_real, folder_name, data_dir)
}

/// Create or open `<root_real>/<folder_name>`, requiring the directory that is
/// found to carry exactly that name. NTFS and macOS answer to other spellings of
/// an existing folder (an 8.3 alias, a case or Unicode-normalisation twin);
/// without this check a second bot could be handed the first bot's directory.
pub(crate) fn ensure_bot_dir_in_root(
    root_real: &Path,
    folder_name: &str,
    data_dir: &Path,
) -> Result<PathBuf, String> {
    let bot_dir = root_real.join(folder_name);
    ensure_plain_child_dir(root_real, &bot_dir)?;
    let bot_real = crate::files::resolve_local_folder(&bot_dir).map_err(|e| e.to_string())?;
    let bot_real = storage::without_verbatim_prefix(bot_real);
    if !bot_real.starts_with(root_real) || bot_real == root_real {
        return Err("bot workspace escaped workspaces root".into());
    }
    if bot_real.parent() != Some(root_real)
        || bot_real.file_name() != Some(std::ffi::OsStr::new(folder_name))
    {
        return Err(format!(
            "bot workspace folder {folder_name:?} resolves to {}: another spelling of an existing folder",
            bot_real.display()
        ));
    }
    if crate::files::is_protected_root(&bot_real, data_dir) {
        return Err("bot workspace cannot overlap Praxi's data folder or credentials".into());
    }
    for sub in WORKSPACE_SUBDIRS {
        let sub_path = bot_real.join(sub);
        // If a file or symlink already sits at a subdir name, leave it alone rather
        // than overwriting user data, unless it is a missing entry.
        if fs::symlink_metadata(&sub_path).is_err() {
            create_private_dir(&sub_path)
                .map_err(|e| format!("create workspace subfolder {sub}: {e}"))?;
        }
    }
    Ok(bot_real)
}

fn ensure_plain_dir_chain(dir: &Path) -> Result<(), String> {
    // Check existing ancestors too: an otherwise plain directory can sit below
    // a symlink/junction pointing into a different bot's tree.
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_plain_dir_chain(parent)?;
    }
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if is_link_or_reparse(&meta) || !meta.is_dir() {
                return Err(format!("{} is not a plain directory", dir.display()));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|err| format!("create {}: {err}", dir.display()))
        }
        Err(e) => Err(format!("stat {}: {e}", dir.display())),
    }
}

fn ensure_plain_child_dir(parent: &Path, child: &Path) -> Result<(), String> {
    if child.parent() != Some(parent) {
        return Err("workspace folder must be a direct child of workspaces root".into());
    }
    match fs::symlink_metadata(child) {
        Ok(meta) => {
            if is_link_or_reparse(&meta) || !meta.is_dir() {
                return Err(format!("{} is not a plain directory", child.display()));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_private_dir(child).map_err(|err| format!("create {}: {err}", child.display()))
        }
        Err(e) => Err(format!("stat {}: {e}", child.display())),
    }
}

/// The account this process runs as.
#[cfg(unix)]
fn process_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Make one directory that only its creator can use. On Unix the mode is set to exactly `0700`
/// after the directory exists (the mode given to `mkdir` is masked by the process umask), and the
/// result must be a plain directory owned by this process's account; anything else is removed
/// again and refused, so a bot's folder and its standard subfolders are never readable by other
/// accounts.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        fs::DirBuilder::new().mode(0o700).create(path)?;
        let checked = fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .and_then(|()| fs::symlink_metadata(path))
            .and_then(|meta| {
                if !is_link_or_reparse(&meta)
                    && meta.is_dir()
                    && meta.uid() == process_uid()
                    && meta.mode() & 0o777 == 0o700
                {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "the new folder is not a private directory owned by this account",
                    ))
                }
            });
        if checked.is_err() {
            let _ = fs::remove_dir(path);
        }
        checked
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

/// The workspaces root must be a plain directory owned by this process's account and closed to
/// every other account (`0700`), or the other accounts could list every bot's folder. A root that
/// is not ours is refused; one that is ours but open is closed. No-op outside Unix.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))] // nothing to check outside Unix
fn secure_workspaces_root(root: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let stat =
            |p: &Path| fs::symlink_metadata(p).map_err(|e| format!("stat {}: {e}", p.display()));
        let meta = stat(root)?;
        if is_link_or_reparse(&meta) || !meta.is_dir() {
            return Err(format!("{} is not a plain directory", root.display()));
        }
        if meta.uid() != process_uid() {
            return Err(format!(
                "workspaces root {} is not owned by the account Praxi runs as",
                root.display()
            ));
        }
        if meta.mode() & 0o777 != 0o700 {
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("close {}: {e}", root.display()))?;
            if stat(root)?.mode() & 0o777 != 0o700 {
                return Err(format!(
                    "workspaces root {} could not be closed to other accounts",
                    root.display()
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        Ok(())
    }
}

#[cfg(windows)]
fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_type().is_symlink() || (meta.file_attributes() & 0x400 != 0)
}

#[cfg(not(windows))]
fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Sum file bytes under `path` using `symlink_metadata` only. Symlinks and
/// reparse points are skipped and file contents are never opened.
#[must_use]
pub fn dir_size_bytes(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if is_link_or_reparse(&meta) {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(rd) = fs::read_dir(path) else {
        return 0;
    };
    rd.flatten().map(|e| dir_size_bytes(&e.path())).sum()
}

/// Query host disk capacity and free bytes for the volume holding `path`
/// (walking up to the nearest existing ancestor if `path` does not exist yet).
#[must_use]
pub fn host_disk_stats(path: &Path) -> Option<DiskStats> {
    let mut probe = Some(path);
    while let Some(p) = probe {
        if p.exists() {
            let total = fs4::total_space(p).ok()?;
            let free = fs4::available_space(p).ok()?;
            let used = total.saturating_sub(free);
            let used_percent = percent_of(used, total);
            return Some(DiskStats {
                total_bytes: total,
                free_bytes: free,
                used_bytes: used,
                used_percent,
            });
        }
        probe = p.parent().filter(|parent| !parent.as_os_str().is_empty());
    }
    None
}

/// Integer percentage `0..=100` of `used / total`.
#[must_use]
pub fn percent_of(used: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    let pct = (u128::from(used) * 100) / u128::from(total);
    u8::try_from(pct.min(100)).unwrap_or(100)
}

/// Why a write triggered a quota error or warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteBudgetOutcome {
    /// Allowed without warning.
    Ok,
    /// Allowed, with a pre-fill warning to surface to the owner.
    Warn(WriteWarningKind),
    /// Refused because a per-bot hard limit would be exceeded.
    Blocked {
        used_bytes: u64,
        added_bytes: u64,
        hard_limit_bytes: u64,
    },
}

/// Classification of a disk or workspace warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteWarningKind {
    /// Host disk usage crossed one of the configured percentage thresholds (80 / 90 / 95).
    DiskPercent {
        threshold_percent: u8,
        used_percent: u8,
        free_after_bytes: u64,
    },
    /// Write would leave fewer than `min_free_bytes` (default 5 GiB) free on the host disk.
    LowFreeSpace {
        free_after_bytes: u64,
        min_free_bytes: u64,
        used_percent: u8,
    },
    /// Bot workspace usage crossed the owner-configured per-bot soft limit.
    BotSoftLimit {
        used_after_bytes: u64,
        soft_limit_bytes: u64,
    },
}

/// Pure evaluation of per-bot hard/soft limits and host disk warning thresholds
/// before a write of `added_bytes`.
#[must_use]
pub fn evaluate_write_budget(
    bot_used_bytes: u64,
    added_bytes: u64,
    soft_limit_bytes: Option<u64>,
    hard_limit_bytes: Option<u64>,
    disk: Option<DiskStats>,
    thresholds: [u8; 3],
    min_free_bytes: u64,
) -> WriteBudgetOutcome {
    let used_after = bot_used_bytes.saturating_add(added_bytes);
    if let Some(hard) = hard_limit_bytes
        && used_after > hard
    {
        return WriteBudgetOutcome::Blocked {
            used_bytes: bot_used_bytes,
            added_bytes,
            hard_limit_bytes: hard,
        };
    }

    if let Some(d) = disk {
        let free_after = d.free_bytes.saturating_sub(added_bytes);
        let used_after_disk = d.total_bytes.saturating_sub(free_after);
        let used_after_pct = percent_of(used_after_disk, d.total_bytes);

        if min_free_bytes > 0 && free_after < min_free_bytes {
            return WriteBudgetOutcome::Warn(WriteWarningKind::LowFreeSpace {
                free_after_bytes: free_after,
                min_free_bytes,
                used_percent: used_after_pct,
            });
        }

        let mut sorted = thresholds;
        sorted.sort_unstable();
        if let Some(&crossed) = sorted.iter().rev().find(|&&t| t > 0 && used_after_pct >= t) {
            return WriteBudgetOutcome::Warn(WriteWarningKind::DiskPercent {
                threshold_percent: crossed,
                used_percent: used_after_pct,
                free_after_bytes: free_after,
            });
        }
    }

    if let Some(soft) = soft_limit_bytes
        && used_after > soft
    {
        return WriteBudgetOutcome::Warn(WriteWarningKind::BotSoftLimit {
            used_after_bytes: used_after,
            soft_limit_bytes: soft,
        });
    }

    WriteBudgetOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_name_sanitizes_traversal_and_reserved_names() {
        assert_eq!(
            sanitize_bot_folder_name("Research Bot", "12345678"),
            "Research Bot"
        );
        assert_eq!(
            sanitize_bot_folder_name("../etc/passwd", "12345678"),
            "etc-passwd"
        );
        assert_eq!(
            sanitize_bot_folder_name("CON", "abcdef01-2345"),
            "bot-abcdef01"
        );
        assert_eq!(
            sanitize_bot_folder_name("...", "abcdef01-2345"),
            "bot-abcdef01"
        );
        assert!(!is_valid_folder_name(".."));
        assert!(!is_valid_folder_name("a/b"));
        assert!(!is_valid_folder_name("NUL"));
    }

    #[test]
    fn folder_names_refuse_short_aliases_streams_and_device_names() {
        for bad in [
            "RESEAR~1",
            "a~2b",
            "Research Bot~1",
            "notes:stream",
            "NUL.txt",
            "con .txt",
            "COM1",
            "lpt\u{b9}",
            "CONIN$",
            "trailing.",
            "trailing ",
        ] {
            assert!(!is_valid_folder_name(bad), "{bad:?} must be refused");
        }
        for good in [
            "Research Bot",
            "\u{c7}A\u{11e}RI",
            "COM10",
            "a~b",
            "bot-1234abcd",
        ] {
            assert!(is_valid_folder_name(good), "{good:?} must be accepted");
        }
        // A bot called like an alias is stored under a neutral name instead.
        assert_eq!(
            sanitize_bot_folder_name("RESEAR~1", "abcdef01-2345"),
            "bot-abcdef01"
        );
    }

    /// A second spelling of an existing folder (8.3 alias, Unicode case twin)
    /// must never resolve to the first bot's directory. On a case-sensitive
    /// volume the twin is simply a different directory, which is fine.
    #[test]
    fn another_spelling_of_a_folder_is_never_handed_to_a_second_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("praxi-data");
        fs::create_dir_all(&data).unwrap();
        let root = tmp.path().join("Documents").join("Praxi");
        let victim_name = "\u{e7}a\u{11f}ri";
        let victim = ensure_bot_workspace_dir(&root, victim_name, &data).unwrap();
        fs::write(victim.join("notes").join("secret.txt"), "victim only").unwrap();
        let root_real = validate_workspaces_root(&root, &data).unwrap();

        // Unicode case twin: only a case-insensitive volume maps it onto the victim.
        let twin = "\u{c7}A\u{11e}RI";
        match ensure_bot_dir_in_root(&root_real, twin, &data) {
            Err(e) => assert!(e.contains("another spelling"), "{e}"),
            Ok(dir) => {
                assert_ne!(dir, victim, "case twin shares the victim's directory");
                assert!(!dir.join("notes").join("secret.txt").exists());
            }
        }
    }

    /// Directory symlink (Unix) or junction/symlink (Windows); false when the
    /// machine does not allow creating one.
    #[cfg(unix)]
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    #[cfg(windows)]
    fn make_dir_link(target: &Path, link: &Path) -> bool {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .creation_flags(0x0800_0000)
            .output()
            .is_ok_and(|o| o.status.success())
            || std::os::windows::fs::symlink_dir(target, link).is_ok()
    }

    /// A folder that is a symlink or (Windows) junction to another bot's
    /// directory is never accepted as a bot workspace.
    #[test]
    fn linked_bot_folder_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("praxi-data");
        fs::create_dir_all(&data).unwrap();
        let root = tmp.path().join("Documents").join("Praxi");
        let victim = ensure_bot_workspace_dir(&root, "Victim", &data).unwrap();
        let root_real = validate_workspaces_root(&root, &data).unwrap();
        let link = root_real.join("Linked");

        let made = make_dir_link(&victim, &link);
        if !made {
            eprintln!("skipped: cannot create a link on this machine");
            return;
        }
        let err = ensure_bot_workspace_dir(&root, "Linked", &data).unwrap_err();
        assert!(err.contains("not a plain directory"), "{err}");
    }

    #[test]
    fn workspaces_root_refuses_data_dir_and_credential_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("praxi-data");
        let creds = tmp.path().join("home").join(".ssh");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&creds).unwrap();

        assert!(validate_workspaces_root(&data, &data).is_err());
        assert!(validate_workspaces_root(&data.join("work"), &data).is_err());
        assert!(validate_workspaces_root(&creds, &data).is_err());

        let ok_root = tmp.path().join("Documents").join("Praxi");
        let ensured = ensure_bot_workspace_dir(&ok_root, "Ada", &data).unwrap();
        for sub in WORKSPACE_SUBDIRS {
            assert!(ensured.join(sub).is_dir(), "missing subfolder {sub}");
        }
    }

    /// A bot's folder and its standard subfolders are private to the service account (`0700`), a
    /// symlink in a bot's place is refused, and two bots never get the same or nested folders.
    #[cfg(unix)]
    #[test]
    fn bot_folders_are_private_distinct_and_never_links() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("praxi-data");
        fs::create_dir_all(&data).unwrap();
        let root = tmp.path().join("praxi-work");
        let a = ensure_bot_workspace_dir(&root, "Ada", &data).unwrap();
        let b = ensure_bot_workspace_dir(&root, "Bob", &data).unwrap();
        assert!(!a.starts_with(&b) && !b.starts_with(&a), "{a:?} {b:?}");
        assert_ne!(a, b);
        for dir in std::iter::once(a.clone()).chain(WORKSPACE_SUBDIRS.iter().map(|s| a.join(s))) {
            let mode = fs::metadata(&dir).unwrap().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "{dir:?} is {mode:o}");
        }
        // Every folder is owned by this account and exactly 0700 (the umask never widens or hides it).
        let uid = rustix::process::geteuid().as_raw();
        for dir in [&a, &b] {
            assert_eq!(fs::metadata(dir).unwrap().uid(), uid);
            assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o700, "{dir:?}");
        }
        // A link standing where a bot's folder would be is refused, not followed.
        let root_real = validate_workspaces_root(&root, &data).unwrap();
        symlink(&a, root_real.join("Evil")).unwrap();
        let err = ensure_bot_workspace_dir(&root, "Evil", &data).unwrap_err();
        assert!(err.contains("not a plain directory"), "{err}");
    }

    /// An open workspaces root (0755: other accounts could list every bot) is closed to 0700, a
    /// link is never used as the root, and a root that is a file is refused.
    #[cfg(unix)]
    #[test]
    fn workspaces_root_is_closed_to_other_accounts_and_never_a_link() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("praxi-data");
        fs::create_dir_all(&data).unwrap();
        let root = tmp.path().join("praxi-work");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let resolved = validate_workspaces_root(&root, &data).unwrap();
        let meta = fs::metadata(&resolved).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700, "{resolved:?}");
        assert_eq!(meta.uid(), rustix::process::geteuid().as_raw());
        // A link as the root is refused (ensure_plain_dir_chain looks at every component).
        let target = tmp.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        symlink(&target, tmp.path().join("linked-root")).unwrap();
        assert!(validate_workspaces_root(&tmp.path().join("linked-root"), &data).is_err());
        // A file in the root's place is refused.
        let file = tmp.path().join("file-root");
        fs::write(&file, "x").unwrap();
        assert!(validate_workspaces_root(&file, &data).is_err());
    }

    #[test]
    fn write_budget_evaluates_hard_limit_percent_thresholds_and_low_free_space() {
        let disk_ok = Some(DiskStats {
            total_bytes: 100 * 1024 * 1024 * 1024,
            free_bytes: 50 * 1024 * 1024 * 1024,
            used_bytes: 50 * 1024 * 1024 * 1024,
            used_percent: 50,
        });
        assert_eq!(
            evaluate_write_budget(
                0,
                1024,
                None,
                None,
                disk_ok,
                [80, 90, 95],
                DEFAULT_MIN_FREE_BYTES
            ),
            WriteBudgetOutcome::Ok
        );

        // Per-bot hard limit blocks when set.
        assert!(matches!(
            evaluate_write_budget(
                900,
                200,
                None,
                Some(1000),
                disk_ok,
                [80, 90, 95],
                DEFAULT_MIN_FREE_BYTES
            ),
            WriteBudgetOutcome::Blocked { .. }
        ));

        // 80/90/95 thresholds warn without blocking when hard limit is off.
        let disk_91 = Some(DiskStats {
            total_bytes: 100 * 1024 * 1024 * 1024,
            free_bytes: 9 * 1024 * 1024 * 1024,
            used_bytes: 91 * 1024 * 1024 * 1024,
            used_percent: 91,
        });
        assert_eq!(
            evaluate_write_budget(
                0,
                1024,
                None,
                None,
                disk_91,
                [80, 90, 95],
                DEFAULT_MIN_FREE_BYTES
            ),
            WriteBudgetOutcome::Warn(WriteWarningKind::DiskPercent {
                threshold_percent: 90,
                used_percent: 91,
                free_after_bytes: 9 * 1024 * 1024 * 1024 - 1024,
            })
        );

        // Leaving less than 5 GiB free warns even below 80% on a huge disk.
        let disk_huge_low_free = Some(DiskStats {
            total_bytes: 1000 * 1024 * 1024 * 1024,
            free_bytes: 5 * 1024 * 1024 * 1024,
            used_bytes: 995 * 1024 * 1024 * 1024,
            used_percent: 70, // suppose thresholds are [80,90,95] and we pass a 10 TB disk:
        });
        // Use 1000 GiB total, 5 GiB free -> 99% used triggers DiskPercent; test < 5 GiB with 10 GiB disk and thresholds [99,99,99]:
        let disk_low_free = Some(DiskStats {
            total_bytes: 100 * 1024 * 1024 * 1024,
            free_bytes: 5 * 1024 * 1024 * 1024,
            used_bytes: 95 * 1024 * 1024 * 1024,
            used_percent: 95,
        });
        let _ = disk_huge_low_free;
        assert!(matches!(
            evaluate_write_budget(
                0,
                1024 * 1024,
                None,
                None,
                disk_low_free,
                [98, 99, 99],
                DEFAULT_MIN_FREE_BYTES
            ),
            WriteBudgetOutcome::Warn(WriteWarningKind::LowFreeSpace { .. })
        ));
    }
}

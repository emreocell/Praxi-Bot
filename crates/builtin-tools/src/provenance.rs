//! Where the content of a file came from.
//!
//! An injected instruction can sit in any text the user did not write: a cloned repo's README,
//! a download, a mail attachment, a page a tool saved earlier. A pathname list alone cannot tell
//! these from the user's own documents (review of PR #138), so the file tools use PROVENANCE:
//!
//! 1. a persistent ledger ([`Provenance`], a table in the daemon's storage) of files and trees
//!    that Praxi's own tools wrote while the run held untrusted content, moved or copied from
//!    such a place, or fetched through the shell (`git clone`, `curl`, `wget`, `unzip` ...);
//! 2. locations that are external by rule (Downloads, temp dirs, browser and mail caches,
//!    package caches) and any git work tree that has a remote;
//! 3. a read or search is judged by what it returns, not by the folder it was asked about.
//!
//! Everything else is the user's own document and stays private. Not detected: a file the user
//! saved by hand from mail or the web into their documents, outside Praxi.

use std::path::{Path, PathBuf};

/// The persistent ledger of untrusted-origin paths. Paths are given already [`normalize`]d.
pub trait Provenance: Send + Sync {
    /// The path itself, or an ancestor directory marked as a tree, is recorded.
    fn is_marked(&self, normalized: &str) -> bool;
    /// Record a file, or with `tree` everything under a directory. Best effort.
    fn mark(&self, normalized: &str, tree: bool, origin: &str);
    /// The path is, or lies inside, a folder the owner trusts as their own content.
    fn is_trusted_folder(&self, _normalized: &str) -> bool {
        false
    }
}

/// The ledger kept in the daemon's storage (table `file_provenance`).
pub struct StorageProvenance(pub std::sync::Arc<storage::Storage>);

impl Provenance for StorageProvenance {
    fn is_marked(&self, normalized: &str) -> bool {
        if self.0.is_untrusted_path(normalized).unwrap_or(true) {
            return true;
        }
        if normalized == UNRESOLVED {
            return false;
        }
        // Old ledgers contain literal ../ and alias spellings. Reconcile on reads too,
        // so upgrading cannot leave already-fetched files protected only by folder trust.
        let Ok(paths) = self.0.untrusted_paths() else {
            return true;
        };
        paths.into_iter().any(|(path, tree)| {
            let Some(key) = identity(Path::new(&path)) else {
                return true;
            };
            key == normalized || (tree && normalized.starts_with(&format!("{key}/")))
        })
    }
    fn mark(&self, normalized: &str, tree: bool, origin: &str) {
        let _ = self.0.mark_untrusted_path(normalized, tree, origin);
    }
    fn is_trusted_folder(&self, normalized: &str) -> bool {
        // Stored entries may predate identity normalization (8.3 names, junctions, case).
        // Never grant trust from an entry whose filesystem identity cannot be resolved.
        self.0.trusted_folders().is_ok_and(|paths| {
            paths.into_iter().any(|path| {
                identity(Path::new(&path)).is_some_and(|key| {
                    key == normalized || normalized.starts_with(&format!("{key}/"))
                })
            })
        })
    }
}

/// `/` separators, no `\\?\` prefix, no trailing slash, lower case on Windows.
#[must_use]
pub fn normalize(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/UNC/") {
        s = format!("//{rest}");
    } else if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_owned();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

/// Resolve the filesystem's identity, or its nearest existing ancestor for a new destination.
/// Ask the OS about the WHOLE ancestor path: Windows and Unix resolve alias/.. differently.
#[must_use]
pub fn identity(path: &Path) -> Option<String> {
    fn resolve(path: &Path) -> Option<PathBuf> {
        match std::fs::canonicalize(path) {
            Ok(real) => {
                #[cfg(windows)]
                {
                    windows_long_path(&real)
                }
                #[cfg(not(windows))]
                {
                    Some(real)
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A dangling link is not a known new destination.
                if !std::fs::symlink_metadata(path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    return None;
                }
                let parent = resolve(path.parent()?)?;
                Some(parent.join(path.file_name()?))
            }
            Err(_) => None,
        }
    }
    resolve(&std::path::absolute(path).ok()?).map(|p| normalize(&p))
}

/// Canonicalization resolves junctions but can retain DOS 8.3 components on Windows.
/// Expand every existing component before deriving a comparison key; failure is unknown identity.
#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_long_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::{Win32::Storage::FileSystem::GetLongPathNameW, core::PCWSTR};
    let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: input is NUL-terminated; the API only reads it during each call.
    let needed = unsafe { GetLongPathNameW(PCWSTR(input.as_ptr()), None) };
    if needed == 0 {
        return None;
    }
    let mut output = vec![0; needed as usize];
    // SAFETY: both buffers live through the call; the output slice supplies its size.
    let written = unsafe { GetLongPathNameW(PCWSTR(input.as_ptr()), Some(&mut output)) };
    if written == 0 || written as usize >= output.len() {
        return None;
    }
    Some(std::ffi::OsString::from_wide(&output[..written as usize]).into())
}

// An unresolved fetch may have written anywhere through an alias. Persist a fail-closed
// marker: folder trust must not turn that uncertainty into trusted instructions.
const UNRESOLVED: &str = "praxi:unresolved-provenance";

pub fn mark_path(ledger: &dyn Provenance, path: &Path, tree: bool, origin: &str) {
    let literal = normalize(path);
    ledger.mark(&literal, tree, origin);
    match identity(path) {
        Some(key) if key != literal => ledger.mark(&key, tree, origin),
        Some(_) => {}
        None => ledger.mark(UNRESOLVED, false, origin),
    }
}

/// Reject roots, home itself, external directories and the Praxi data tree (or its ancestors).
#[must_use]
pub fn trust_entry_allowed(path: &Path, data: &Path) -> bool {
    let Some(key) = identity(path) else {
        return false;
    };
    let lower = format!("{}/", key.to_lowercase());
    if too_broad(path) || EXTERNAL_PARTS.iter().any(|part| lower.contains(part)) {
        return false;
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        && identity(Path::new(&home)).as_deref() == Some(key.as_str())
    {
        return false;
    }
    let Some(temp) = identity(&std::env::temp_dir()) else {
        return false;
    };
    if key == temp || key.starts_with(&format!("{temp}/")) {
        return false;
    }
    let Some(data) = identity(data) else {
        return false;
    };
    key != data && !key.starts_with(&format!("{data}/")) && !data.starts_with(&format!("{key}/"))
}

/// Locations whose content is external by rule: downloads, temp, browser / mail / package caches.
const EXTERNAL_PARTS: &[&str] = &[
    "/downloads/",
    "/download/",
    "/temp/",
    "/tmp/",
    "/attachments/",
    "/attachment/",
    "/quarantine/",
    "/inetcache/",
    "/google/chrome/",
    "/microsoft/edge/",
    "/bravesoftware/",
    "/mozilla/firefox/",
    "/.mozilla/",
    "/thunderbird/",
    "/content.outlook/",
    "/mail downloads/",
    "/.cache/",
    "/.npm/",
    "/node_modules/",
    "/.cargo/registry/",
    "/.cargo/git/",
    "/site-packages/",
];
const EXTERNAL_EXTENSIONS: &[&str] = &["eml", "mbox", "msg", "mhtml"];

/// Whether `path` lies in an external location by rule (see the module docs).
#[must_use]
pub fn external_location(path: &Path) -> bool {
    // rules match case-insensitively on every OS (`Downloads` on Linux too)
    let n = format!("{}/", normalize(path).to_lowercase());
    if EXTERNAL_PARTS.iter().any(|part| n.contains(part)) {
        return true;
    }
    if n.trim_end_matches('/')
        .rsplit_once('.')
        .is_some_and(|(_, ext)| EXTERNAL_EXTENSIONS.contains(&ext))
    {
        return true;
    }
    in_git_work_tree_with_remote(path)
}

/// A git work tree with a remote configured. Folder trust is applied by `is_untrusted`.
fn in_git_work_tree_with_remote(path: &Path) -> bool {
    let mut dir = Some(path);
    for _ in 0..16 {
        let Some(d) = dir else { return false };
        let git = d.join(".git");
        if git.is_file() {
            return true; // worktree / submodule pointer: its remote is elsewhere; be conservative
        }
        if git.is_dir() {
            return std::fs::read_to_string(git.join("config"))
                .is_ok_and(|c| c.contains("[remote \""));
        }
        dir = d.parent();
    }
    false
}

/// Untrusted by rule or by the ledger.
#[must_use]
pub fn is_untrusted(ledger: Option<&dyn Provenance>, path: &Path) -> bool {
    let Some(normalized) = identity(path) else {
        return true;
    };
    // What a bot fetched or wrote in an untrusted run stays untrusted even inside a folder the
    // owner trusts; the owner's trust only lifts the by-rule classification.
    if ledger.is_some_and(|l| {
        l.is_marked(UNRESOLVED) || l.is_marked(&normalize(path)) || l.is_marked(&normalized)
    }) {
        return true;
    }
    if ledger.is_some_and(|l| l.is_trusted_folder(&normalized)) {
        return false;
    }
    external_location(Path::new(&normalized)) || external_location(path)
}

/// The git work tree (with a remote) that contains `path`: the folder the owner can be offered
/// to trust. `None` when the path is not in such a tree.
#[must_use]
pub fn git_work_tree_root(path: &Path) -> Option<PathBuf> {
    let mut dir = Some(path);
    for _ in 0..16 {
        let d = dir?;
        let git = d.join(".git");
        if git.is_dir() {
            return std::fs::read_to_string(git.join("config"))
                .is_ok_and(|c| c.contains("[remote \""))
                .then(|| d.to_path_buf());
        }
        if git.is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Directories too broad to mark as a tree (a drive root or the home directory).
fn too_broad(path: &Path) -> bool {
    path.parent().is_none()
        || std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .is_some_and(|h| normalize(Path::new(&h)) == normalize(path))
}

/// What a shell command line fetches from outside, as `(path, whole_tree)`: `git clone` / `pull`
/// / `fetch`, `curl` / `wget` / `Invoke-WebRequest` downloads, `unzip -d`, `tar -C`,
/// `Expand-Archive`. Relative targets resolve against `cwd`. Detection is by token, so a
/// command assembled at run time (variables, `eval`) is not seen.
#[must_use]
pub fn shell_fetch_targets(command: &str, cwd: &Path) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    let resolve = |p: &str| -> PathBuf {
        let p = p.trim_matches(['"', '\'']);
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    };
    let name_of = |url: &str| -> Option<String> {
        let tail = url.trim_matches(['"', '\'']).split(['?', '#']).next()?;
        let seg = tail.trim_end_matches('/').rsplit(['/', ':']).next()?;
        let seg = seg.trim_end_matches(".git");
        (!seg.is_empty()).then(|| seg.to_owned())
    };
    for segment in command.split(['\n', ';', '|', '&']) {
        let t: Vec<&str> = segment.split_whitespace().collect();
        let Some(head) = t.first().map(|h| h.to_ascii_lowercase()) else {
            continue;
        };
        let after = |flags: &[&str]| -> Option<&str> {
            t.iter()
                .position(|w| flags.iter().any(|f| w.eq_ignore_ascii_case(f)))
                .and_then(|i| t.get(i + 1).copied())
        };
        let is_url = |w: &&str| w.contains("://") || w.starts_with("git@");
        match head.trim_end_matches(".exe") {
            "git" => {
                let (mut base, mut sub, mut i) = (cwd.to_path_buf(), None, 1);
                while i < t.len() {
                    let w = t[i];
                    if w == "-C" {
                        if let Some(d) = t.get(i + 1) {
                            base = resolve(d);
                        }
                        i += 2;
                    } else if w.starts_with('-') {
                        i += 1;
                    } else {
                        sub = Some((i, w.to_ascii_lowercase()));
                        break;
                    }
                }
                match sub {
                    Some((at, s)) if s == "clone" => {
                        let operands: Vec<&str> = t[at + 1..]
                            .iter()
                            .copied()
                            .filter(|w| !w.starts_with('-'))
                            .collect();
                        if let Some(u) = operands
                            .iter()
                            .position(|w| is_url(w) || w.contains('/') || w.contains('\\'))
                        {
                            let target = operands
                                .get(u + 1)
                                .map(|d| base.join(resolve(d)))
                                .or_else(|| name_of(operands[u]).map(|n| base.join(n)));
                            if let Some(target) = target {
                                out.push((target, true));
                            }
                        }
                    }
                    Some((_, s))
                        if matches!(
                            s.as_str(),
                            "pull" | "fetch" | "submodule" | "checkout" | "switch" | "merge"
                        ) =>
                    {
                        out.push((base, true));
                    }
                    _ => {}
                }
            }
            "curl" => {
                if let Some(f) = after(&["-o", "--output"]) {
                    out.push((resolve(f), false));
                } else if t.iter().any(|w| *w == "-O" || *w == "--remote-name")
                    && let Some(n) = t.iter().find(|w| is_url(w)).and_then(|u| name_of(u))
                {
                    out.push((cwd.join(n), false));
                }
            }
            "wget" => {
                if let Some(f) = after(&["-O", "--output-document"]) {
                    out.push((resolve(f), false));
                } else if let Some(d) = after(&["-P", "--directory-prefix"]) {
                    out.push((resolve(d), true));
                } else if let Some(n) = t.iter().find(|w| is_url(w)).and_then(|u| name_of(u)) {
                    out.push((cwd.join(n), false));
                }
            }
            "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" => {
                if let Some(f) = after(&["-outfile"]) {
                    out.push((resolve(f), false));
                }
            }
            "unzip" => {
                if let Some(d) = after(&["-d"]) {
                    out.push((resolve(d), true));
                }
            }
            "tar" => {
                if t.iter().any(|w| {
                    w.starts_with('-') && w.contains('x')
                        || *w == "x"
                        || w.starts_with("xf")
                        || w.starts_with("xzf")
                }) {
                    out.push((
                        after(&["-C"]).map_or_else(|| cwd.to_path_buf(), resolve),
                        true,
                    ));
                }
            }
            "expand-archive" => {
                if let Some(d) = after(&["-destinationpath"]) {
                    out.push((resolve(d), true));
                }
            }
            _ => {}
        }
    }
    out.retain(|(p, tree)| !(*tree && too_broad(p)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct Mem(Mutex<Vec<(String, bool)>>, Mutex<Vec<String>>);
    impl Provenance for Mem {
        fn is_marked(&self, n: &str) -> bool {
            self.0
                .lock()
                .iter()
                .any(|(p, tree)| p == n || (*tree && n.starts_with(&format!("{p}/"))))
        }
        fn mark(&self, n: &str, tree: bool, _: &str) {
            self.0.lock().push((n.to_owned(), tree));
        }
        fn is_trusted_folder(&self, n: &str) -> bool {
            self.1
                .lock()
                .iter()
                .any(|p| n == p || n.starts_with(&format!("{p}/")))
        }
    }

    #[test]
    fn external_by_rule_and_by_ledger_but_a_plain_document_is_private() {
        assert!(external_location(Path::new(r"C:\Users\a\Downloads\x.pdf")));
        assert!(external_location(Path::new(
            "/home/a/.cargo/registry/src/x/README.md"
        )));
        assert!(external_location(Path::new("/home/a/mail/msg.eml")));
        // (fake paths: a real temp dir is itself external by rule)
        let doc = Path::new("/home/a/Documents/notes.txt");
        assert!(!is_untrusted(None, doc));
        let ledger = Mem::default();
        ledger.mark(&normalize(Path::new("/home/a/Documents/saved")), true, "x");
        assert!(is_untrusted(
            Some(&ledger),
            Path::new("/home/a/Documents/saved/a.md")
        ));
        assert!(!is_untrusted(Some(&ledger), doc));
    }

    #[test]
    fn a_git_work_tree_with_a_remote_is_external_and_one_without_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join(".git").join("config"),
            "[core]
",
        )
        .unwrap();
        assert!(!in_git_work_tree_with_remote(&repo.join("README.md")));
        std::fs::write(
            repo.join(".git").join("config"),
            "[remote \"origin\"]
 url = x
",
        )
        .unwrap();
        assert!(in_git_work_tree_with_remote(
            &repo.join("sub").join("README.md")
        ));
    }

    #[test]
    fn shell_fetches_name_their_target() {
        let cwd = Path::new("/work");
        let t = |c: &str| -> Vec<(String, bool)> {
            shell_fetch_targets(c, cwd)
                .into_iter()
                .map(|(p, tree)| (normalize(&p), tree))
                .collect()
        };
        let one = |p: &str, tree: bool| vec![(p.to_owned(), tree)];
        assert_eq!(t("git clone https://x.test/a/b.git"), one("/work/b", true));
        assert_eq!(
            t("git clone https://x.test/a/b.git dst && ls"),
            one("/work/dst", true)
        );
        assert_eq!(
            t("curl -sL https://x.test/f.zip -o out/f.zip"),
            one("/work/out/f.zip", false)
        );
        assert_eq!(t("wget https://x.test/p/f.tgz"), one("/work/f.tgz", false));
        assert_eq!(t("unzip a.zip -d /work/u"), one("/work/u", true));
        assert_eq!(t("ls -la"), Vec::<(String, bool)>::new());
    }

    #[test]
    fn fetched_identity_survives_parent_traversal_aliases_and_fetch_completion() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let ledger = Mem::default();
        ledger.1.lock().push(identity(&repo).unwrap());
        let spelling = repo.join("sub").join("..").join("payload.txt");
        mark_path(&ledger, &spelling, false, "curl");
        std::fs::write(&spelling, "untrusted instructions").unwrap();
        mark_path(&ledger, &spelling, false, "curl completed");
        assert!(is_untrusted(Some(&ledger), &repo.join("payload.txt")));
        let storage = std::sync::Arc::new(storage::Storage::open_in_memory(dir.path()).unwrap());
        storage.trust_folder(&identity(&repo).unwrap()).unwrap();
        storage
            .mark_untrusted_path(&normalize(&spelling), false, "legacy curl")
            .unwrap();
        assert!(is_untrusted(
            Some(&StorageProvenance(storage)),
            &repo.join("payload.txt")
        ));

        let aliases = dir.path().join("aliases");
        std::fs::create_dir(&aliases).unwrap();
        let alias = aliases.join("alias");
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&alias)
                .arg(&repo)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&repo, &alias).unwrap();
        let parent_spelling = alias.join("..");
        assert_eq!(
            identity(&parent_spelling.join("ancestor.txt")),
            Some(normalize(
                &std::fs::canonicalize(&parent_spelling)
                    .unwrap()
                    .join("ancestor.txt")
            ))
        );
        let destination = alias.join("new.txt");
        mark_path(&ledger, &destination, false, "curl started");
        assert_eq!(identity(&destination), identity(&repo.join("new.txt")));
        // The fetch may replace an alias: reconcile its final destination as well.
        let final_repo = dir.path().join("final-repo");
        std::fs::create_dir(&final_repo).unwrap();
        ledger.1.lock().push(identity(&final_repo).unwrap());
        #[cfg(windows)]
        std::fs::remove_dir(&alias).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&alias).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&alias)
                .arg(&final_repo)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&final_repo, &alias).unwrap();
        std::fs::write(&destination, "download").unwrap();
        mark_path(&ledger, &destination, false, "curl completed");
        assert!(is_untrusted(Some(&ledger), &final_repo.join("new.txt")));
        assert!(is_untrusted(Some(&ledger), &repo.join("new.txt")));
        assert!(!trust_entry_allowed(dir.path(), &repo));
        assert!(!trust_entry_allowed(&std::env::temp_dir(), &repo));
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            assert!(!trust_entry_allowed(Path::new(&home), &repo));
        }
    }

    #[cfg(windows)]
    fn assert_windows_spellings_share_provenance(repo: &Path, alias: &Path) {
        assert_eq!(identity(repo), identity(alias));
        let file = repo.join("payload.txt");
        let alias_file = alias.join("payload.txt");
        // A new destination must retain its identity when the fetch completes.
        assert_eq!(identity(&file), identity(&alias_file));
        std::fs::write(&file, "download").unwrap();
        for (trusted, marked, read) in [(repo, &alias_file, &file), (alias, &file, &alias_file)] {
            let storage = std::sync::Arc::new(storage::Storage::open_in_memory(repo).unwrap());
            storage.trust_folder(&normalize(trusted)).unwrap();
            let ledger = StorageProvenance(storage.clone());
            assert!(
                !is_untrusted(Some(&ledger), read),
                "owner trust must match aliases"
            );
            // Simulate a legacy literal mark, then exercise new marks too.
            storage
                .mark_untrusted_path(&normalize(marked), false, "legacy curl")
                .unwrap();
            assert!(
                is_untrusted(Some(&ledger), read),
                "a legacy fetch overrides trust"
            );
            mark_path(&ledger, marked, false, "curl");
            assert!(is_untrusted(Some(&ledger), read), "a fetch overrides trust");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_mixed_case_paths_share_trust_and_fetch_marks() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("OwnerFolderMixedCase");
        std::fs::create_dir(&repo).unwrap();
        let alias = dir.path().join("oWNERfOLDERmIXEDcASE");
        assert_windows_spellings_share_provenance(&repo, &alias);
    }

    #[cfg(windows)]
    #[test]
    #[allow(unsafe_code)]
    fn windows_short_names_share_trust_and_fetch_marks() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows::{Win32::Storage::FileSystem::GetShortPathNameW, core::PCWSTR};
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("Owner Folder With Long Name");
        std::fs::create_dir(&repo).unwrap();
        let input: Vec<u16> = repo.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: input is NUL-terminated and output is a valid, sized writable buffer.
        let needed = unsafe { GetShortPathNameW(PCWSTR(input.as_ptr()), None) };
        if needed == 0 {
            eprintln!("8.3 names unavailable; skipping");
            return;
        }
        let mut output = vec![0; needed as usize];
        // SAFETY: both buffers remain valid for the call.
        let written = unsafe { GetShortPathNameW(PCWSTR(input.as_ptr()), Some(&mut output)) };
        if written == 0 || written as usize >= output.len() {
            eprintln!("8.3 names unavailable; skipping");
            return;
        }
        let alias = PathBuf::from(std::ffi::OsString::from_wide(&output[..written as usize]));
        if normalize(&alias) == normalize(&repo) {
            eprintln!("volume has no short-name alias; skipping");
            return;
        }
        assert_windows_spellings_share_provenance(&repo, &alias);
    }

    #[test]
    fn a_trusted_folder_lifts_the_rules_but_not_what_a_bot_fetched_there() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("mine");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join(".git").join("config"),
            "[remote \"origin\"]
 url = x
",
        )
        .unwrap();
        let readme = repo.join("README.md");
        let ledger = Mem::default();
        assert!(
            is_untrusted(Some(&ledger), &readme),
            "a work tree with a remote is outside content"
        );
        assert_eq!(
            git_work_tree_root(&readme).as_deref(),
            Some(repo.as_path()),
            "the folder to offer"
        );
        ledger.1.lock().push(identity(&repo).unwrap());
        assert!(
            !is_untrusted(Some(&ledger), &readme),
            "trusted by the owner"
        );
        assert!(!is_untrusted(
            Some(&ledger),
            &repo.join("src").join("lib.rs")
        ));
        // a bot cloned something into the trusted folder: the ledger mark wins
        ledger.mark(&normalize(&repo.join("vendor")), true, "git clone");
        assert!(is_untrusted(
            Some(&ledger),
            &repo.join("vendor").join("x").join("README.md")
        ));
    }
}

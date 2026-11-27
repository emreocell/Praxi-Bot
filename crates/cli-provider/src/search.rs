//! Deterministic GUI-safe lookup. No shell startup files are executed.
use crate::{CliEngineSpec, PathEnv};
use std::path::{Path, PathBuf};

/// Drop the verbatim prefix that `canonicalize` adds on Windows (dunce-style).
/// Node and many CLIs cannot `realpath` such a path (`EISDIR: lstat 'C:'`), and it is
/// noise to show. The UNC form becomes a plain `\\server\share` path; anything that is
/// not a plain drive or UNC form is returned unchanged. A no-op off Windows.
#[must_use]
pub fn plain_path(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let mut chars = rest.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.next() == Some(':')
            && matches!(chars.next(), Some('\\') | None)
        {
            return PathBuf::from(rest);
        }
    }
    path
}

pub(crate) fn trusted_directory(dir: &Path) -> bool {
    dir.is_absolute() && trusted_candidate(dir)
}

pub(crate) fn trusted_candidate(path: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(cwd) = std::env::current_dir().and_then(std::fs::canonicalize) else {
        return false;
    };
    let home = std::env::var_os("HOME").and_then(|h| std::fs::canonicalize(h).ok());
    outside_working_dir(path, &real, &cwd, home.as_deref())
}

/// Folders where the system's own programs live. When the working directory is `/` (systemd's
/// default `WorkingDirectory`) it contains every path, so "outside the working directory" can
/// say nothing: only these are accepted then, besides the private home below.
#[cfg(unix)]
const SYSTEM_DIRS: [&str; 12] = [
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/opt",
    "/nix",
    "/snap",
    "/Applications",
    "/System",
    "/Library",
];

/// A program inside the working directory is refused (a planted file must never run by
/// accident). `path` is the candidate as it was named, `real` the same file with every symlink
/// resolved.
///
/// Two exceptions, Unix only, for a daemon that lives in its own private home (a service user's
/// `WorkingDirectory` is often its home, and the CLI install tree is `~/.local/bin`,
/// `~/.npm-global/bin`, ...): the working directory is the home folder, or it is `/`. Then a
/// program is accepted only when [`private_home_program`] vouches for it. The home folder as a
/// whole, and the Praxi data folder, are never trusted.
fn outside_working_dir(path: &Path, real: &Path, cwd: &Path, home: Option<&Path>) -> bool {
    #[cfg(unix)]
    {
        let euid = rustix::process::geteuid().as_raw();
        outside_working_dir_as(euid, path, real, cwd, home)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, home);
        !real.starts_with(cwd)
    }
}

#[cfg(unix)]
fn outside_working_dir_as(
    euid: u32,
    path: &Path,
    real: &Path,
    cwd: &Path,
    home: Option<&Path>,
) -> bool {
    let private = || home.is_some_and(|h| private_home_program(euid, path, real, h));
    if cwd.parent().is_none() {
        return SYSTEM_DIRS.iter().any(|d| real.starts_with(d)) || private();
    }
    !real.starts_with(cwd) || (home == Some(cwd) && private())
}

/// A program in the service user's own home that is safe to run although the working directory
/// is that home (or `/`):
/// - it was named inside one of the known per-user CLI folders ([`HOME_BIN_DIRS`]; a symlink in
///   one that points elsewhere in the same home is fine), and
/// - the whole chain from the home folder down to the file belongs to the effective user and is
///   not writable by group or others, and the folders above the home belong to root or that
///   user and are not writable by group or others (a sticky shared folder such as `/tmp` is fine).
///
/// Without that, a `HOME` that someone else controls (`HOME=/tmp/evil`) would be enough to run
/// a planted `agy`.
#[cfg(unix)]
fn private_home_program(euid: u32, path: &Path, real: &Path, home: &Path) -> bool {
    if real == home || !real.starts_with(home) {
        return false;
    }
    // The name as given, with its folder resolved but the file itself not followed.
    let named = match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => std::fs::canonicalize(dir).map(|d| d.join(name)),
        _ => return false,
    };
    let Ok(named) = named else {
        return false;
    };
    let in_bin_dir = HOME_BIN_DIRS.iter().any(|rel| {
        std::fs::canonicalize(home.join(rel))
            .is_ok_and(|bin| bin.starts_with(home) && named.starts_with(bin))
    });
    in_bin_dir && owned_private_chain(euid, home, real)
}

/// `path` is `top` or inside it, and `top` down to `path` (each folder and the file) belongs to
/// `euid` and cannot be written by group or others; the folders above `top` belong to root or
/// `euid` and cannot be written by group or others unless they are sticky. Nothing is followed: both
/// paths must already be canonical.
#[cfg(unix)]
fn owned_private_chain(euid: u32, top: &Path, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(rest) = path.strip_prefix(top) else {
        return false;
    };
    let private =
        |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.uid() == euid && m.mode() & 0o022 == 0);
    let mut cur = top.to_path_buf();
    if !private(&cur) {
        return false;
    }
    for part in rest.components() {
        cur.push(part);
        if !private(&cur) {
            return false;
        }
    }
    top.ancestors().skip(1).all(|dir| {
        std::fs::metadata(dir).is_ok_and(|m| {
            (m.uid() == 0 || m.uid() == euid) && (m.mode() & 0o022 == 0 || m.mode() & 0o1000 != 0)
        })
    })
}

/// Whether `dir` (a folder of `path_dirs` taken from the environment, such as the npm prefix)
/// may be searched: on Unix it must be `top` or inside it and pass [`owned_private_chain`] for
/// the effective user. Elsewhere the check does not exist and the folder is accepted.
pub(crate) fn private_to_user(top: &Path, dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let (Ok(top), Ok(dir)) = (std::fs::canonicalize(top), std::fs::canonicalize(dir)) else {
            return false;
        };
        owned_private_chain(rustix::process::geteuid().as_raw(), &top, &dir)
    }
    #[cfg(not(unix))]
    {
        let _ = (top, dir);
        true
    }
}

/// Folders under the home directory where coding CLIs install themselves.
pub(crate) const HOME_BIN_DIRS: [&str; 7] = [
    ".local/bin",
    ".npm-global/bin",
    ".opencode/bin",
    ".claude/local",
    ".volta/bin",
    ".bun/bin",
    "bin",
];

pub(crate) fn augment(env: &mut PathEnv) {
    if let Some(home) = &env.home_dir {
        for rel in HOME_BIN_DIRS {
            env.path_dirs.push(home.join(rel));
        }
        // npm's user prefix on Windows, even after a GUI was launched.
        env.path_dirs.push(home.join("AppData/Roaming/npm"));
        env.path_dirs.push(home.join(".npm-global"));
        // Scoop puts one .exe shim per tool here (`scoop install opencode`).
        env.path_dirs.push(home.join("scoop/shims"));
        // The prefix comes from the environment: only a folder of this user that nobody else
        // can write to (a planted `agy` there would win for every name nothing else has).
        if let Some(prefix) = env.env_vars.get("NPM_CONFIG_PREFIX") {
            let prefix = PathBuf::from(prefix);
            for dir in [prefix.clone(), prefix.join("bin")] {
                if private_to_user(&prefix, &dir) {
                    env.path_dirs.push(dir);
                }
            }
        }
        let nvm = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(nvm) {
            let mut bins: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|e| e.path().join("bin"))
                .collect();
            bins.sort();
            bins.reverse();
            env.path_dirs.extend(bins);
        }
    }
    if let Some(local) = &env.local_app_data {
        env.path_dirs.push(local.join("cursor-agent"));
        // Antigravity CLI (`agy`) installs here on Windows.
        env.path_dirs.push(local.join("agy/bin"));
        env.path_dirs.push(local.join("Microsoft/WinGet/Links"));
    }
    if let Some(roaming) = env.env_vars.get("APPDATA") {
        env.path_dirs.push(PathBuf::from(roaming).join("npm"));
    }
    for key in ["ProgramFiles", "PROGRAMFILES", "NVM_SYMLINK"] {
        if let Some(dir) = env.env_vars.get(key) {
            let path = PathBuf::from(dir);
            env.path_dirs.push(if key == "NVM_SYMLINK" {
                path
            } else {
                path.join("nodejs")
            });
        }
    }
    #[cfg(unix)]
    env.path_dirs
        .extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    env.path_dirs.retain(|p| p.is_absolute());
    let mut seen = std::collections::HashSet::new();
    env.path_dirs.retain(|p| seen.insert(p.clone()));
}

#[must_use]
pub fn find_engine(spec: &CliEngineSpec, env: &PathEnv) -> Option<PathBuf> {
    crate::detect::find_candidate(spec.command, env).or_else(|| {
        // Generic `agent` is shared by unrelated products. Accept only from the
        // vendor-specific installation directory, never an arbitrary PATH hit.
        let mut vendor_env = env.clone();
        vendor_env.path_dirs.retain(|dir| {
            dir.file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cursor-agent"))
        });
        spec.aliases.iter().find_map(|alias| {
            let candidate = crate::detect::find_candidate(alias, &vendor_env)?;
            let parent = candidate.parent()?.file_name()?.to_str()?;
            parent
                .eq_ignore_ascii_case("cursor-agent")
                .then_some(candidate)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_relative_cwd_and_redirected_candidates() {
        let cwd = std::env::current_dir().unwrap();
        assert!(!trusted_directory(Path::new(".")));
        assert!(!trusted_directory(&cwd));
        assert!(!trusted_candidate(&cwd.join("Cargo.toml")));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.cmd"), "fixture").unwrap();
        let env = PathEnv::for_test(dir.path().to_path_buf());
        assert!(
            crate::detect::find_candidate("claude", &env)
                .unwrap()
                .is_absolute()
        );
        assert!(crate::detect::find_candidate("../claude", &env).is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(cwd.join("Cargo.toml"), dir.path().join("hijack")).unwrap();
            assert!(crate::detect::find_candidate("hijack", &env).is_none());
        }
    }
    /// A private home like the service user's: `<tmp>/home` with `.local/bin/agy` and `data/`,
    /// every folder 0750 and the program 0755, owned by the test's user.
    #[cfg(unix)]
    struct PrivateHome {
        _tmp: tempfile::TempDir,
        outer: PathBuf,
        home: PathBuf,
        agy: PathBuf,
        euid: u32,
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    fn private_home() -> PrivateHome {
        let tmp = tempfile::tempdir().unwrap();
        let outer = std::fs::canonicalize(tmp.path()).unwrap();
        let home = outer.join("home");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join("data")).unwrap();
        let agy = bin.join("agy");
        std::fs::write(&agy, b"fixture").unwrap();
        for dir in [
            &outer,
            &home,
            &home.join(".local"),
            &bin,
            &home.join("data"),
        ] {
            chmod(dir, 0o750);
        }
        chmod(&agy, 0o755);
        PrivateHome {
            _tmp: tmp,
            outer,
            home,
            agy,
            euid: rustix::process::geteuid().as_raw(),
        }
    }

    #[cfg(unix)]
    impl PrivateHome {
        /// The verdict for `path` (already canonical) with the home as the working directory.
        fn trusted(&self, path: &Path) -> bool {
            outside_working_dir_as(self.euid, path, path, &self.home, Some(&self.home))
        }
    }

    /// The service user's layout: HOME = the working directory, CLIs under `~/.local/bin`.
    #[cfg(unix)]
    #[test]
    fn a_service_home_that_is_the_working_directory_still_finds_its_own_clis() {
        let h = private_home();
        assert!(h.trusted(&h.agy), "home is cwd");
        let bin = h.agy.parent().unwrap();
        assert!(h.trusted(bin), "the folder itself is searched too");
        // Without the home exception the same file is refused: the rule itself is unchanged.
        assert!(!outside_working_dir_as(
            h.euid, &h.agy, &h.agy, &h.home, None
        ));
        // Another folder that merely contains the file stays refused, even beside a home.
        let project = h.home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let planted = project.join("claude");
        std::fs::write(&planted, b"fixture").unwrap();
        assert!(!outside_working_dir_as(
            h.euid,
            &planted,
            &planted,
            &project,
            Some(&h.home)
        ));
    }

    /// The installer's Node.js lives beside the release tree, not inside it: the daemon's
    /// `WorkingDirectory` is `/opt/praxi`, and a program below it is refused as a planted file, so
    /// `/opt/praxi/node/bin/npm` could never be run for a CLI install.
    #[cfg(unix)]
    #[test]
    fn the_installers_node_runtime_is_not_inside_the_daemons_working_directory() {
        let cwd = Path::new("/opt/praxi");
        let outside = Path::new("/opt/praxi-nodejs/lib/node_modules/npm/bin/npm-cli.js");
        let inside = Path::new("/opt/praxi/node/lib/node_modules/npm/bin/npm-cli.js");
        let home = Some(Path::new("/var/lib/praxi"));
        assert!(outside_working_dir(outside, outside, cwd, home));
        assert!(!outside_working_dir(inside, inside, cwd, home));
    }

    /// Only the known per-user CLI folders are exempt: not the home as a whole, not the data
    /// folder that sits in it (the service user's home IS its data folder), not any other tree.
    #[cfg(unix)]
    #[test]
    fn the_home_exception_covers_only_the_known_cli_folders() {
        let h = private_home();
        let in_data = h.home.join("data/agy");
        std::fs::write(&in_data, b"fixture").unwrap();
        chmod(&in_data, 0o755);
        assert!(!h.trusted(&in_data), "data folder");
        assert!(!h.trusted(&h.home.join("data")));
        assert!(!h.trusted(&h.home), "the home itself");
        let loose = h.home.join("agy");
        std::fs::write(&loose, b"fixture").unwrap();
        chmod(&loose, 0o755);
        assert!(!h.trusted(&loose), "directly in the home");
        // A known folder that is not one of the list (`.nvm`, `.cache`) is not exempt either.
        let cache = h.home.join(".cache");
        std::fs::create_dir_all(&cache).unwrap();
        chmod(&cache, 0o750);
        std::fs::write(cache.join("agy"), b"fixture").unwrap();
        chmod(&cache.join("agy"), 0o755);
        assert!(!h.trusted(&cache.join("agy")));
    }

    /// A `HOME` somebody else controls is not a private home: wrong owner, or writable by others.
    #[cfg(unix)]
    #[test]
    fn a_home_of_another_user_or_one_others_can_write_is_not_trusted() {
        let h = private_home();
        assert!(h.trusted(&h.agy));
        // `HOME=/tmp/evil` made by another account: every folder there has another owner.
        let other = h.euid.wrapping_add(1);
        assert!(!outside_working_dir_as(
            other,
            &h.agy,
            &h.agy,
            &h.home,
            Some(&h.home)
        ));
        let bin = h.agy.parent().unwrap().to_path_buf();
        let dot_local = h.home.join(".local");
        for dir in [&h.home, &dot_local, &bin] {
            for mode in [0o770, 0o707, 0o777] {
                chmod(dir, mode);
                assert!(!h.trusted(&h.agy), "{} at {mode:o}", dir.display());
            }
            chmod(dir, 0o750);
        }
        for mode in [0o775, 0o757, 0o777] {
            chmod(&h.agy, mode);
            assert!(!h.trusted(&h.agy), "program at {mode:o}");
        }
        chmod(&h.agy, 0o755);
        assert!(h.trusted(&h.agy));
        // A shared, non-sticky folder above the home lets others swap the whole home.
        chmod(&h.outer, 0o777);
        assert!(!h.trusted(&h.agy), "world-writable parent");
        chmod(&h.outer, 0o1777);
        assert!(
            h.trusted(&h.agy),
            "a sticky shared parent like /tmp is fine"
        );
        chmod(&h.outer, 0o750);
    }

    /// A link in a CLI folder to a program elsewhere in the same private home is how native
    /// installers lay themselves out.
    #[cfg(unix)]
    #[test]
    fn links_in_a_cli_folder_stay_inside_the_private_home() {
        let h = private_home();
        let bin = h.agy.parent().unwrap();
        let share = h.home.join(".local/share/claude");
        std::fs::create_dir_all(&share).unwrap();
        chmod(&h.home.join(".local/share"), 0o750);
        chmod(&share, 0o750);
        let real = share.join("claude-2.1");
        std::fs::write(&real, b"fixture").unwrap();
        chmod(&real, 0o755);
        std::os::unix::fs::symlink(&real, bin.join("claude")).unwrap();
        assert!(outside_working_dir_as(
            h.euid,
            &bin.join("claude"),
            &real,
            &h.home,
            Some(&h.home)
        ));
        // Reaching the same file by its own path, outside a CLI folder, is not enough.
        assert!(!h.trusted(&real));
        // The link target is held to the same rules as the folder: a folder others can write on
        // the way there (here `share`) lets them swap what the link points at.
        chmod(&share, 0o770);
        assert!(!outside_working_dir_as(
            h.euid,
            &bin.join("claude"),
            &real,
            &h.home,
            Some(&h.home)
        ));
    }

    /// systemd starts a service in `/` when no `WorkingDirectory` is set. `/` contains every
    /// path, so it must not switch the check off: only system folders (and the private home's
    /// CLI folders) are accepted.
    #[cfg(unix)]
    #[test]
    fn a_service_started_in_the_root_directory_still_refuses_planted_programs() {
        let root = Path::new("/");
        let euid = rustix::process::geteuid().as_raw();
        let env = std::fs::canonicalize("/usr/bin/env").unwrap();
        assert!(outside_working_dir_as(euid, &env, &env, root, None));
        let h = private_home();
        // A program anywhere else (here: a private temp folder) is refused, not waved through.
        let elsewhere = h.outer.join("elsewhere");
        std::fs::write(&elsewhere, b"fixture").unwrap();
        chmod(&elsewhere, 0o755);
        assert!(!outside_working_dir_as(
            euid, &elsewhere, &elsewhere, root, None
        ));
        assert!(!outside_working_dir_as(
            euid,
            &elsewhere,
            &elsewhere,
            root,
            Some(&h.home)
        ));
        assert!(!outside_working_dir_as(
            euid,
            &h.home,
            &h.home,
            root,
            Some(&h.home)
        ));
        // The service user's own CLI folders still work from `/`.
        assert!(outside_working_dir_as(
            euid,
            &h.agy,
            &h.agy,
            root,
            Some(&h.home)
        ));
        assert!(!outside_working_dir_as(
            euid.wrapping_add(1),
            &h.agy,
            &h.agy,
            root,
            Some(&h.home)
        ));
    }

    /// `NPM_CONFIG_PREFIX` is read from the environment, so it is searched only when it is the
    /// user's own and closed to others.
    #[cfg(unix)]
    #[test]
    fn an_npm_prefix_is_searched_only_when_only_this_user_can_write_it() {
        let h = private_home();
        let prefix = h.outer.join("npm");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        chmod(&prefix, 0o750);
        chmod(&prefix.join("bin"), 0o750);
        let env_for = || {
            let mut env = PathEnv::for_test(h.outer.join("empty-path")).with_home(h.home.clone());
            env.env_vars
                .insert("NPM_CONFIG_PREFIX".into(), prefix.display().to_string());
            augment(&mut env);
            env
        };
        assert!(env_for().path_dirs.contains(&prefix.join("bin")));
        chmod(&prefix.join("bin"), 0o775);
        let env = env_for();
        assert!(!env.path_dirs.contains(&prefix.join("bin")));
        assert!(
            env.path_dirs.contains(&prefix),
            "the prefix itself is still private"
        );
        chmod(&prefix, 0o777);
        let env = env_for();
        assert!(!env.path_dirs.contains(&prefix) && !env.path_dirs.contains(&prefix.join("bin")));
        // The same prefix seen by another user is not theirs.
        assert!(!owned_private_chain(
            h.euid.wrapping_add(1),
            &prefix,
            &prefix
        ));
    }

    #[test]
    fn verbatim_prefix_is_removed_only_for_drive_and_unc_paths() {
        let p = |s: &str| plain_path(PathBuf::from(s));
        assert_eq!(
            p(r"\\?\C:\Users\a\x.cmd"),
            PathBuf::from(r"C:\Users\a\x.cmd")
        );
        assert_eq!(p(r"\\?\UNC\srv\share\x"), PathBuf::from(r"\\srv\share\x"));
        assert_eq!(p(r"\\?\Volume{1}\x"), PathBuf::from(r"\\?\Volume{1}\x"));
        assert_eq!(p("/usr/bin/x"), PathBuf::from("/usr/bin/x"));
    }
    #[test]
    fn scoop_shims_are_searched_under_home() {
        let mut env =
            PathEnv::for_test(std::env::temp_dir()).with_home(std::env::temp_dir().join("h"));
        augment(&mut env);
        assert!(env.path_dirs.iter().any(|d| d.ends_with("scoop/shims")));
    }
    #[test]
    fn generic_agent_is_not_cursor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.cmd"), "fixture").unwrap();
        let env = PathEnv::for_test(dir.path().to_path_buf());
        assert!(find_engine(crate::engine_spec("cursor").unwrap(), &env).is_none());
        let vendor = dir.path().join("cursor-agent");
        std::fs::create_dir(&vendor).unwrap();
        std::fs::write(vendor.join("agent.cmd"), b"fixture").unwrap();
        let mut env = env;
        env.path_dirs.push(vendor.clone());
        let found = find_engine(crate::engine_spec("cursor").unwrap(), &env).unwrap();
        let vendor = plain_path(std::fs::canonicalize(vendor).unwrap());
        assert_eq!(found.parent(), Some(vendor.as_path()));
    }

    #[test]
    fn gui_fallback_directories_find_installations_created_after_startup() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = PathEnv::for_test(dir.path().join("empty-path"))
            .with_home(dir.path().join("home"))
            .with_local_app_data(dir.path().join("local"));
        augment(&mut env);
        let installed = dir.path().join("home/.local/bin");
        std::fs::create_dir_all(&installed).unwrap();
        let name = format!("claude{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(installed.join(name), b"fixture").unwrap();
        assert!(find_engine(crate::engine_spec("claude").unwrap(), &env).is_some());
        let cursor = dir.path().join("local/cursor-agent");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(cursor.join("agent.cmd"), b"fixture").unwrap();
        assert!(find_engine(crate::engine_spec("cursor").unwrap(), &env).is_some());
    }
}

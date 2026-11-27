//! Which user the coding CLIs run as on this machine, and where that user keeps them.
//!
//! Detection, installs and sign-ins all run inside the daemon, so they always see the daemon's
//! user, `HOME` and search path, never those of the person who opened an SSH session. This
//! module only *names* that user, so the app, `bot cli` and `bot doctor` can say it in plain
//! words and tell a person who typed a command as someone else what to do instead.
use crate::PathEnv;
use std::path::{Path, PathBuf};

/// The account the daemon (and so every coding CLI it starts) runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAs {
    /// Account name, or `None` when the system cannot tell.
    pub user: Option<String>,
    /// The account is the administrator (`root`): CLIs installed or signed in as root are
    /// invisible to a service that runs as someone else, and the other way round.
    pub is_root: bool,
    /// Home folder the CLIs use for their installs and sign-in files.
    pub home: Option<PathBuf>,
}

impl RunAs {
    /// The user for display (`unknown` when the system did not say).
    #[must_use]
    pub fn label(&self) -> &str {
        self.user.as_deref().unwrap_or("unknown")
    }
}

/// The running process's account, with `env`'s home folder (the one detection searches).
#[must_use]
pub fn run_as(env: &PathEnv) -> RunAs {
    #[cfg(unix)]
    let (user, is_root) = {
        let uid = rustix::process::geteuid().as_raw();
        let user = std::fs::read_to_string("/etc/passwd")
            .ok()
            .and_then(|text| passwd_name(&text, uid))
            .or_else(|| env_name(&["USER", "LOGNAME"]));
        (user, uid == 0)
    };
    #[cfg(not(unix))]
    let (user, is_root) = (env_name(&["USERNAME"]), false);
    RunAs {
        user,
        is_root,
        home: env.home_dir.clone(),
    }
}

fn env_name(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
}

/// Account name of `uid` in `/etc/passwd` text (`name:x:uid:gid:...`).
#[cfg(any(unix, test))]
fn passwd_name(text: &str, uid: u32) -> Option<String> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let id: u32 = f.nth(1)?.parse().ok()?;
            Some((name, id))
        })
        .find(|(_, id)| *id == uid)
        .map(|(name, _)| name.to_owned())
        .filter(|name| !name.is_empty())
}

/// This process runs as a different user than the one that owns Praxi's data folder, so what
/// it sees (its own `HOME`, its own CLIs and sign-ins) is not what the Praxi service uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrongUser {
    /// The account this command runs as.
    pub me: String,
    /// The account that owns the Praxi data folder (the service user).
    pub owner: String,
}

/// Where the installer keeps a server's data, and the command it puts in `/usr/local/bin`.
#[cfg(unix)]
const SERVICE_DATA_DIR: &str = "/var/lib/praxi";
#[cfg(unix)]
const SERVICE_WRAPPER: &str = "/usr/local/bin/praxi";

/// Whether this command runs as someone other than the Praxi service user: the data folder in
/// use, or (where the installer's `praxi` command exists) the server's own data folder, belongs
/// to a different account. Typical case: `bot doctor` typed in an SSH session as `ubuntu`.
#[cfg(unix)]
#[must_use]
pub fn wrong_user(data_dir: &Path) -> Option<WrongUser> {
    use std::os::unix::fs::MetadataExt;
    let me = rustix::process::geteuid().as_raw();
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let mut dirs = vec![data_dir];
    if Path::new(SERVICE_WRAPPER).is_file() {
        dirs.push(Path::new(SERVICE_DATA_DIR));
    }
    dirs.into_iter().find_map(|dir| {
        let owner = std::fs::metadata(dir).ok()?.uid();
        mismatch(me, owner, &passwd)
    })
}

/// Not applicable off Unix: the desktop app runs everything as the signed-in user.
#[cfg(not(unix))]
#[must_use]
pub fn wrong_user(_data_dir: &Path) -> Option<WrongUser> {
    None
}

#[cfg(any(unix, test))]
fn mismatch(me: u32, owner: u32, passwd: &str) -> Option<WrongUser> {
    if me == owner {
        return None;
    }
    let name = |uid: u32| passwd_name(passwd, uid).unwrap_or_else(|| format!("user {uid}"));
    Some(WrongUser {
        me: name(me),
        owner: name(owner),
    })
}

/// A plain Unix account name: safe to show inside a command a person copies.
fn plain_account(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The start of a terminal command that runs `bot cli ...` as `who`, for a person logged in
/// as somebody else (the SSH user). The installer's `praxi` command already does that; without
/// it, `sudo -u <user>` does.
#[must_use]
pub fn terminal_command(who: &RunAs) -> String {
    if cfg!(unix) && Path::new("/usr/local/bin/praxi").is_file() {
        return "sudo praxi cli".to_owned();
    }
    match who.user.as_deref() {
        Some(user) if cfg!(unix) && !who.is_root && plain_account(user) => {
            format!("sudo -u {user} -H bot cli")
        }
        _ => "bot cli".to_owned(),
    }
}

/// Folders (under `home`) where coding CLIs put themselves, the ones that exist right now.
/// The terminal and the shell must find what the detector finds, so both use this list.
#[must_use]
pub fn install_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = crate::search::HOME_BIN_DIRS
        .iter()
        .map(|rel| home.join(rel))
        .collect();
    if let Some(prefix) = std::env::var_os("NPM_CONFIG_PREFIX") {
        dirs.push(PathBuf::from(prefix).join("bin"));
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// `path` (a `PATH` value) with the existing install folders of `home` appended when they are
/// missing, or `None` when nothing changes. Appended, never prepended: a program found by the
/// system's own directories keeps winning.
#[must_use]
pub fn path_with_install_dirs(
    path: Option<&std::ffi::OsStr>,
    home: &Path,
) -> Option<std::ffi::OsString> {
    let mut dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let before = dirs.len();
    for dir in install_dirs(home) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if dirs.len() == before {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_is_found_by_user_id() {
        let text = "root:x:0:0:root:/root:/bin/bash\n# c\npraxi:x:998:998::/var/lib/praxi:/usr/sbin/nologin\n";
        assert_eq!(passwd_name(text, 998).as_deref(), Some("praxi"));
        assert_eq!(passwd_name(text, 0).as_deref(), Some("root"));
        assert_eq!(passwd_name(text, 5), None);
        assert_eq!(passwd_name("garbage\n:x:7:7\n", 7), None);
    }

    #[test]
    fn a_data_folder_of_another_account_is_a_wrong_user() {
        let passwd = "ubuntu:x:1000:1000::/home/ubuntu:/bin/bash\npraxi:x:998:998::/var/lib/praxi:/usr/sbin/nologin\n";
        assert_eq!(mismatch(998, 998, passwd), None);
        assert_eq!(
            mismatch(1000, 998, passwd),
            Some(WrongUser {
                me: "ubuntu".into(),
                owner: "praxi".into()
            })
        );
        // An account the system cannot name is still said plainly.
        assert_eq!(mismatch(0, 998, "").unwrap().me, "user 0");
    }

    #[cfg(unix)]
    #[test]
    fn a_data_folder_of_this_user_is_not_a_wrong_user() {
        let dir = tempfile::tempdir().unwrap();
        // The installer's `praxi` command may exist on the machine running the tests; the
        // folder under test is owned by this user either way.
        let got = wrong_user(dir.path());
        if !Path::new(SERVICE_WRAPPER).is_file() {
            assert_eq!(got, None);
        }
    }

    #[test]
    fn the_terminal_command_runs_as_the_service_account() {
        let who = |user: Option<&str>, is_root| RunAs {
            user: user.map(str::to_owned),
            is_root,
            home: None,
        };
        if !Path::new("/usr/local/bin/praxi").is_file() && cfg!(unix) {
            assert_eq!(
                terminal_command(&who(Some("praxi"), false)),
                "sudo -u praxi -H bot cli"
            );
            // A name that is not plain is never pasted into a command a person copies.
            assert_eq!(terminal_command(&who(Some("a b;rm"), false)), "bot cli");
        }
        assert!(
            terminal_command(&who(None, false)) == "bot cli"
                || terminal_command(&who(None, false)) == "sudo praxi cli"
        );
    }

    #[test]
    fn install_dirs_are_appended_once_and_only_when_they_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("var-lib-praxi");
        assert!(
            path_with_install_dirs(None, &home).is_none(),
            "no folders yet"
        );
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::create_dir_all(home.join(".npm-global/bin")).unwrap();
        let system = std::ffi::OsString::from("/usr/bin:/bin");
        let joined = path_with_install_dirs(Some(system.as_os_str()), &home).unwrap();
        let dirs: Vec<_> = std::env::split_paths(&joined).collect();
        assert_eq!(
            dirs[0],
            PathBuf::from("/usr/bin"),
            "system folders keep priority"
        );
        assert!(dirs.contains(&home.join(".local/bin")));
        assert!(dirs.contains(&home.join(".npm-global/bin")));
        assert!(
            path_with_install_dirs(Some(joined.as_os_str()), &home).is_none(),
            "second pass changes nothing"
        );
    }
}

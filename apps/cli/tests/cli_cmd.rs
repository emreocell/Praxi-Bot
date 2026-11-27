//! `bot cli` against a fake service-user layout: a private home folder with the CLIs below
//! `~/.local/bin` and a sign-in file, and a search path that does not mention that folder
//! (what a systemd unit without a `PATH` line looks like). The real `bot` binary runs with that
//! `HOME`, so what it reports is what the daemon would see for the same user.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Layout {
    _root: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    seen: PathBuf,
}

fn script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `/var/lib/praxi` in miniature. `agy` answers `--version`; `claude` records how it was run.
fn layout() -> Layout {
    let root = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let home = base.join("var-lib-praxi");
    let bin = home.join(".local/bin");
    let data = base.join("data");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let seen = base.join("seen");
    script(&bin, "agy", "echo 1.5.18");
    script(
        &bin,
        "claude",
        &format!("printf '%s|%s' \"$*\" \"$HOME\" > '{}'", seen.display()),
    );
    let token = home.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&token).unwrap();
    std::fs::write(token.join("antigravity-oauth-token"), "SENTINEL_TOKEN").unwrap();
    // Private, like the service's own home: nobody but the owner can write any folder of it
    // (the temp directory and what `create_dir_all` makes may be group-writable here).
    for dir in [
        &base,
        &home,
        &home.join(".local"),
        &bin,
        &home.join(".gemini"),
        &token,
    ] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750)).unwrap();
    }
    Layout {
        _root: root,
        home,
        data,
        seen,
    }
}

fn bot(l: &Layout, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bot"))
        .arg("--data-dir")
        .arg(&l.data)
        .args(args)
        .env_clear()
        .env("HOME", &l.home)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

#[test]
fn status_finds_the_service_users_clis_and_names_that_user() {
    let l = layout();
    let out = bot(&l, &["--json", "cli", "status"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        !text.contains("SENTINEL_TOKEN"),
        "a sign-in file is never read: {text}"
    );
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["home"], l.home.to_str().unwrap());
    let engines = report["engines"].as_array().unwrap();
    let find = |id: &str| engines.iter().find(|e| e["id"] == id).unwrap();
    let agy = find("agy");
    assert_eq!(agy["installed"], true);
    assert_eq!(agy["version"], "1.5.18");
    assert!(
        agy["path"]
            .as_str()
            .unwrap()
            .ends_with("var-lib-praxi/.local/bin/agy")
    );
    // The token file proves a sign-in happened, not that it works: unknown, with the file noted.
    assert_eq!(agy["signed_in"], serde_json::Value::Null);
    assert_eq!(agy["credential_file_present"], true);
    assert_eq!(find("codex")["installed"], false);
    assert!(
        report["terminal_command"]
            .as_str()
            .unwrap()
            .ends_with("cli")
    );
}

#[test]
fn status_in_words_says_what_to_run_for_each_cli() {
    let l = layout();
    let out = bot(&l, &["cli", "status"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("looked up as"), "{text}");
    assert!(text.contains("sign-in file found (not checked)"), "{text}");
    assert!(text.contains("login agy"), "{text}");
    assert!(text.contains("install codex"), "{text}");
}

#[test]
fn login_runs_the_clis_own_command_as_this_user() {
    let l = layout();
    let out = bot(&l, &["cli", "login", "claude"]);
    assert!(out.status.success(), "{out:?}");
    let seen = std::fs::read_to_string(&l.seen).unwrap();
    assert_eq!(seen, format!("auth login|{}", l.home.display()));
}

#[test]
fn a_missing_cli_or_a_wrong_name_says_what_to_do() {
    let l = layout();
    let out = bot(&l, &["cli", "login", "codex"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bot cli install codex"), "{err}");
    let out = bot(&l, &["cli", "login", "nope"]);
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("claude") && err.contains("agy"), "{err}");
}

#[test]
fn a_cli_without_a_verified_installer_prints_the_vendors_command() {
    let l = layout();
    let out = bot(&l, &["cli", "install", "vibe"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("uv tool install mistral-vibe"), "{err}");
}

/// An install typed as somebody else than the user that owns the data folder must stop before
/// anything is installed or pinned: the pin file decides which programs the service later runs.
/// (A root-owned folder stands for the service user's; skipped when the tests run as root.)
#[test]
fn an_install_as_the_wrong_user_stops_before_installing_or_pinning() {
    use std::os::unix::fs::MetadataExt;
    let l = layout();
    let uid_of = |p: &Path| std::fs::metadata(p).unwrap().uid();
    if uid_of(Path::new("/usr")) == uid_of(&l.home) {
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_bot"))
        .arg("--data-dir")
        .arg("/usr")
        .args(["cli", "install", "claude"])
        .env_clear()
        .env("HOME", &l.home)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Praxi runs as"), "{err}");
    assert!(err.contains("Nothing was installed"), "{err}");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(!said.contains("Installing"), "{said}");
    assert!(!Path::new("/usr/cli-install-pins.json").exists());
}

/// `bot cli` starts in the home folder, so the home exception is what finds the CLIs. A home
/// whose CLI folder others can write is a plant spot, not a service user's home: nothing in it
/// is found (and so nothing would be run).
#[test]
fn a_cli_folder_that_others_can_write_is_not_searched() {
    let l = layout();
    let installed = |l: &Layout| {
        let out = bot(l, &["--json", "cli", "status"]);
        assert!(out.status.success(), "{out:?}");
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        report["engines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "agy")
            .unwrap()["installed"]
            .clone()
    };
    assert_eq!(installed(&l), true);
    let bin = l.home.join(".local/bin");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(installed(&l), false);
}

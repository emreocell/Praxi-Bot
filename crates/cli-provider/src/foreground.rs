//! Run a coding CLI's own sign-in in the caller's terminal (`bot cli login`).
//!
//! The in-app sign-in drives the same command through a pseudo terminal; this is the plain
//! version for a person at a real terminal (an SSH session). It finds the program the same way
//! detection does (the service user's `HOME` and search path), so what runs is exactly what the
//! app reports as installed, as the user that runs `bot`.
use crate::{
    PathEnv,
    detect::{ResolvedCommand, apply_sanitized_env, find_candidate, resolve_spawn},
};

/// The program and arguments that start `id`'s sign-in.
///
/// # Errors
/// A stable code: `unknown_engine`, `not_installed` or `shim_refused`.
pub fn login_command(id: &str, env: &PathEnv) -> Result<ResolvedCommand, &'static str> {
    let spec = crate::engine_spec(id).ok_or("unknown_engine")?;
    // A CLI that is not installed is not signed in to, whatever else sits on the path.
    let installed = crate::search::find_engine(spec, env).ok_or("not_installed")?;
    let mut words = spec.login.split_whitespace();
    let program = words.next().ok_or("unknown_engine")?;
    // The registry text starts with the program (`claude auth login`). It is the detected one,
    // except for a CLI whose agent binary is not the one that signs in (Vibe: `vibe-acp` is found,
    // `vibe --setup` signs in), which sits beside it.
    let candidate = if program == spec.command || spec.aliases.contains(&program) {
        installed
    } else {
        find_candidate(program, env).ok_or("not_installed")?
    };
    let mut resolved = resolve_spawn(&candidate, env).map_err(|_| "shim_refused")?;
    resolved.args.extend(words.map(str::to_owned));
    Ok(resolved)
}

/// Run the sign-in with the terminal's input and output until it ends.
///
/// The environment is the detector's (no vault or API keys), plus `TERM` so a full-screen
/// prompt draws correctly. The folder it starts in is the home folder, never a project.
///
/// # Errors
/// The codes of [`login_command`], or `spawn_failed`.
pub async fn run_login(id: &str, env: &PathEnv) -> Result<std::process::ExitStatus, &'static str> {
    let resolved = login_command(id, env)?;
    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.args(&resolved.args);
    apply_sanitized_env(&mut cmd, env);
    if let Some(term) = std::env::var_os("TERM") {
        cmd.env("TERM", term);
    }
    if let Some(home) = env.home_dir.as_ref().filter(|h| h.is_dir()) {
        cmd.current_dir(home);
    }
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    cmd.status().await.map_err(|_| "spawn_failed")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A service user's layout: each `root/home/.local/bin/…` is a script that records
    /// its name, arguments and `HOME` into `<root>/seen`.
    fn service_layout(names: &[&str]) -> (tempfile::TempDir, PathEnv) {
        service_layout_in("home", names)
    }

    fn service_layout_in(home_name: &str, names: &[&str]) -> (tempfile::TempDir, PathEnv) {
        let root = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(root.path()).unwrap().join(home_name);
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in names {
            let script = bin.join(name);
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '%s|%s|%s' \"{name}\" \"$*\" \"$HOME\" > '{}'\n",
                    root.path().join("seen").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // No search path of its own: only the service user's home folder, as under systemd.
        let env = PathEnv::for_test(root.path().join("empty")).with_home(home);
        let mut env = env;
        crate::search::augment(&mut env);
        (root, env)
    }

    /// The sign-in is started with `execve` (program + argument vector), never through a shell:
    /// shell syntax in the path of the program is just characters in a file name.
    #[tokio::test]
    async fn the_sign_in_never_goes_through_a_shell() {
        let (root, env) = service_layout_in("h o;touch PWNED;$(touch PWNED2)`x`", &["agy"]);
        let status = run_login("agy", &env).await.unwrap();
        assert!(status.success());
        let seen = std::fs::read_to_string(root.path().join("seen")).unwrap();
        assert!(seen.starts_with("agy||"), "{seen}");
        for planted in ["PWNED", "PWNED2"] {
            let home = env.home_dir.as_ref().unwrap();
            assert!(!root.path().join(planted).exists() && !home.join(planted).exists());
            assert!(!std::path::Path::new(planted).exists());
        }
    }

    #[test]
    fn the_login_arguments_follow_the_command_word() {
        let (_root, env) = service_layout(&["claude"]);
        let cmd = login_command("claude", &env).unwrap();
        assert!(cmd.program.ends_with("home/.local/bin/claude"));
        assert_eq!(cmd.args, ["auth", "login"]);
        assert_eq!(login_command("nope", &env).unwrap_err(), "unknown_engine");
        assert_eq!(login_command("codex", &env).unwrap_err(), "not_installed");
    }

    #[tokio::test]
    async fn the_sign_in_runs_as_the_service_user_with_its_home() {
        let (root, env) = service_layout(&["agy"]);
        let status = run_login("agy", &env).await.unwrap();
        assert!(status.success());
        let seen = std::fs::read_to_string(root.path().join("seen")).unwrap();
        let home = env.home_dir.as_ref().unwrap().display().to_string();
        assert_eq!(seen, format!("agy||{home}"));
    }

    /// Vibe is detected by `vibe-acp` but signs in with `vibe --setup`: the sign-in program
    /// is the one the registry names, found beside the detected one.
    #[tokio::test]
    async fn a_cli_whose_agent_binary_differs_signs_in_with_its_own_command() {
        let (root, env) = service_layout(&["vibe-acp", "vibe"]);
        run_login("vibe", &env).await.unwrap();
        let seen = std::fs::read_to_string(root.path().join("seen")).unwrap();
        assert!(seen.starts_with("vibe|--setup|"), "{seen}");
        // Without the agent binary the CLI is not installed, even with `vibe` on the path.
        let (_root, env) = service_layout(&["vibe"]);
        assert_eq!(login_command("vibe", &env).unwrap_err(), "not_installed");
    }
}

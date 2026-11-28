//! `bot cli`: the coding CLIs (Claude Code, Codex, Antigravity, ...) of the user that runs this
//! command, which on a server must be the user the Praxi service runs as. Install, sign-in and
//! status all use that user's home and search path, exactly like the app does, so what you set up
//! here is what Praxi sees. On a server use the installer's `praxi` command
//! (`sudo praxi cli login agy`): it runs this as the service user.
use std::path::Path;
use std::time::Duration;

use cli_provider::{AuthState, Detector, PathEnv, identity};
use serde_json::json;

use crate::{Result, client};

#[derive(clap::Subcommand)]
pub enum CliAction {
    /// Show which coding CLIs this user has, where they are and whether each is signed in.
    Status,
    /// Install a coding CLI as this user (the same fixed installer the app runs on a server).
    Install {
        /// Engine id, e.g. `claude`, `codex`, `agy` (see `bot cli status`).
        engine: String,
    },
    /// Sign in to a coding CLI in this terminal (its own sign-in, as this user).
    Login {
        /// Engine id, e.g. `claude`, `codex`, `agy` (see `bot cli status`).
        engine: String,
    },
}

pub fn run(
    action: &CliAction,
    data_dir: &Path,
    json_out: bool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    // The Node.js runtime Praxi may have installed under the data folder is part of the search path.
    cli_provider::node_runtime::register_data_dir(data_dir);
    // Start from the home folder: a shell left in somebody else's private folder (an SSH user's
    // home, after `sudo -u`) has a working directory this user cannot read, and a program inside
    // the working directory is never run.
    if let Some(home) = std::env::var_os("HOME").filter(|h| Path::new(h).is_dir()) {
        let _ = std::env::set_current_dir(home);
    }
    let rt = crate::current_thread_runtime()?;
    match action {
        CliAction::Status => rt.block_on(status(data_dir, json_out, out)),
        CliAction::Install { engine } => rt.block_on(install(engine, data_dir, out)),
        CliAction::Login { engine } => rt.block_on(login(engine, data_dir, out)),
    }
}

/// What to say when this command runs as someone other than the service user.
fn wrong_user_note(data_dir: &Path) -> Option<String> {
    let wrong = identity::wrong_user(data_dir)?;
    let who = identity::run_as(&PathEnv::from_ambient());
    Some(format!(
        "You are {me}, but Praxi runs as {owner}. A coding CLI installed or signed in as {me} is not the one Praxi uses. Run this as {owner}: {command} ...",
        me = wrong.me,
        owner = wrong.owner,
        command = identity::terminal_command(&who),
    ))
}

fn known_engine(id: &str) -> Result<&'static cli_provider::CliEngineSpec> {
    cli_provider::engine_spec(id).ok_or_else(|| {
        let known: Vec<_> = cli_provider::CLI_ENGINES.iter().map(|e| e.id).collect();
        client::fail(
            client::exit::NOT_FOUND,
            format!(
                "There is no coding CLI called \"{id}\". Choose one of: {}.",
                known.join(", ")
            ),
        )
    })
}

const SIGN_IN_SEEN_LOCALLY: &str = "signed in (seen locally, not verified)";

fn sign_in_text(row: &cli_provider::DetectedCli) -> &'static str {
    match (row.auth, row.detail.as_deref()) {
        (AuthState::SignedIn, _) => "signed in",
        (AuthState::SignedOut, _) => "NOT signed in",
        (AuthState::Unknown, Some("auth_metadata_only" | "auth_unverified" | "auth_unparsed")) => {
            SIGN_IN_SEEN_LOCALLY
        }
        (AuthState::Unknown, _) => "sign-in unknown",
    }
}

async fn status(data_dir: &Path, json_out: bool, out: &mut dyn std::io::Write) -> Result<()> {
    let detector = Detector::new();
    let env = detector.environment();
    let who = identity::run_as(env);
    let wrong = identity::wrong_user(data_dir);
    let command = identity::terminal_command(&who);
    let rows = tokio::time::timeout(Duration::from_secs(30), detector.detect(true))
        .await
        .map_err(|_| "Checking the coding CLIs took too long. Try again.")?;
    let path_of = |id: &str| {
        cli_provider::engine_spec(id)
            .and_then(|spec| cli_provider::search::find_engine(spec, env))
            .map(|p| p.display().to_string())
    };
    if json_out {
        let engines: Vec<_> = rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.id, "name": r.display_name, "installed": r.installed,
                    "version": r.version, "path": r.installed.then(|| path_of(&r.id)).flatten(),
                    "signed_in": match r.auth {
                        AuthState::SignedIn => json!(true),
                        AuthState::SignedOut => json!(false),
                        AuthState::Unknown => json!(null),
                    },
                    "credential_file_present": r.detail.as_deref() == Some("auth_metadata_only"),
                })
            })
            .collect();
        let report = json!({
            "user": who.user, "home": who.home, "is_root": who.is_root,
            "wrong_user": wrong.as_ref().map(|w| json!({"me": w.me, "owner": w.owner})),
            "terminal_command": command, "engines": engines,
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    writeln!(
        out,
        "Coding CLIs are looked up as {} (home {}).",
        who.label(),
        who.home
            .as_ref()
            .map_or("unknown".into(), |h| h.display().to_string())
    )?;
    if let Some(note) = wrong_user_note(data_dir) {
        writeln!(out, "WARNING: {note}")?;
    }
    for row in &rows {
        if !row.installed {
            writeln!(
                out,
                "  {:<18} not installed   ({command} install {})",
                row.display_name, row.id
            )?;
            continue;
        }
        let version = row.version.as_deref().unwrap_or("?");
        writeln!(
            out,
            "  {:<18} {version}, {}   {}",
            row.display_name,
            sign_in_text(row),
            path_of(&row.id).unwrap_or_default()
        )?;
        if row.auth != AuthState::SignedIn {
            writeln!(out, "  {:<18} sign in: {command} login {}", "", row.id)?;
        }
    }
    Ok(())
}

/// A plain sentence for a stable error code of the installer or the login helper.
fn explain(code: &str, id: &str) -> String {
    match code {
        "not_installed" => {
            format!("{id} is not installed for this user. Install it first: bot cli install {id}")
        }
        "copy_only" => format!(
            "Praxi has no verified installer for {id}. Use the vendor's own instructions (see the guide, \"Coding CLI engines\")."
        ),
        "npm_missing" | "node_missing" => {
            "This machine has no Node.js (npm). Install Node.js 22 LTS (on a Praxi server, run the installer again with --update: it installs one for the praxi user), then try again.".to_owned()
        }
        "timeout" => "The installer took too long and was stopped. Try again.".to_owned(),
        "shim_refused" | "npm_shim_refused" => {
            "The program could not be started safely.".to_owned()
        }
        "spawn_failed" => "The program could not be started.".to_owned(),
        other => format!("It did not work ({other})."),
    }
}

async fn install(id: &str, data_dir: &Path, out: &mut dyn std::io::Write) -> Result<()> {
    let spec = known_engine(id)?;
    // An install as the wrong user would put a program in the wrong home and then vouch for it in
    // the service's trust record (the pin file in the data folder). Refuse before anything runs.
    if let Some(note) = wrong_user_note(data_dir) {
        return Err(client::fail(
            client::exit::REJECTED,
            format!("{note}\nNothing was installed."),
        ));
    }
    // No verified fixed installer (Vibe's `uv tool install`): show the vendor's command, to be run
    // by hand as this user, instead of a bare refusal.
    if !spec.install().is_some_and(|i| i.verified && i.executable) {
        let how = spec.install().map_or_else(
            || format!("see {}", spec.docs_url),
            |i| {
                format!(
                    "run it yourself, as {}: {}",
                    identity::run_as(&PathEnv::from_ambient()).label(),
                    i.display()
                )
            },
        );
        return Err(client::fail(
            client::exit::REJECTED,
            format!(
                "Praxi has no verified installer for {}; {how}",
                spec.display_name
            ),
        ));
    }
    let mut detector = Detector::new();
    let needs_npm = spec.install().is_some_and(|i| i.program == "npm");
    if needs_npm && cli_provider::install::resolve_installer(detector.environment()).is_err() {
        let Some(target) = cli_provider::node_runtime::supported_target() else {
            return Err(client::fail(
                client::exit::REJECTED,
                explain("npm_missing", id),
            ));
        };
        writeln!(
            out,
            "Installing Node.js (LTS) into the Praxi data folder first..."
        )?;
        let version = cli_provider::node_runtime::install_node(
            cli_provider::node_runtime::DIST_BASE,
            target,
            data_dir,
            true,
        )
        .await
        .map_err(|code| client::fail(client::exit::RUN_FAILED, explain(code, id)))?;
        writeln!(out, "Node.js {version} installed.")?;
        detector = Detector::new();
    }
    let mut lines = Vec::new();
    let result = cli_provider::install::run_install(
        id,
        detector.environment(),
        data_dir,
        cli_provider::install::INSTALL_TIMEOUT,
        |line| lines.push(line),
    )
    .await;
    for line in &lines {
        writeln!(out, "{line}")?;
    }
    result.map_err(|code| client::fail(client::exit::RUN_FAILED, explain(code, id)))?;
    let who = identity::run_as(detector.environment());
    let found = detector
        .detect(true)
        .await
        .into_iter()
        .any(|r| r.id == id && r.installed);
    if !found {
        return Err(client::fail(
            client::exit::RUN_FAILED,
            format!(
                "The installer finished, but {id} is not where Praxi looks for it ({}). Run `bot cli status` to see where Praxi looks.",
                who.home
                    .as_ref()
                    .map_or("unknown home".into(), |h| h.display().to_string())
            ),
        ));
    }
    // The app signs in only to an install Praxi itself made: record it as the app's install does.
    if !daemon_api::record_cli_install_pin(data_dir, detector.environment(), id) {
        return Err(client::fail(
            client::exit::RUN_FAILED,
            format!(
                "{id} is installed, but the Praxi data folder ({}) does not belong to this user, so the install was not recorded. Run this as the user Praxi runs as.",
                data_dir.display()
            ),
        ));
    }
    writeln!(
        out,
        "{} is installed for {}. Sign in: bot cli login {id}",
        spec.display_name,
        who.label()
    )?;
    Ok(())
}

async fn login(id: &str, data_dir: &Path, out: &mut dyn std::io::Write) -> Result<()> {
    let spec = known_engine(id)?;
    if let Some(note) = wrong_user_note(data_dir) {
        writeln!(out, "WARNING: {note}")?;
    }
    let env = PathEnv::from_ambient();
    writeln!(
        out,
        "Signing in to {} as {}. Follow what it shows; press Ctrl-C to stop.",
        spec.display_name,
        identity::run_as(&env).label()
    )?;
    out.flush()?;
    let status = cli_provider::foreground::run_login(id, &env)
        .await
        .map_err(|code| client::fail(client::exit::RUN_FAILED, explain(code, id)))?;
    if !status.success() {
        return Err(client::fail(
            client::exit::RUN_FAILED,
            format!(
                "The {} sign-in did not finish. Run it again: bot cli login {id}",
                spec.display_name
            ),
        ));
    }
    writeln!(out, "Done. Check the result with: bot cli status")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_engine_lists_the_known_ones() {
        let err = known_engine("nope").unwrap_err().to_string();
        assert!(err.contains("claude") && err.contains("agy"), "{err}");
    }

    #[test]
    fn codes_become_sentences_that_say_what_to_do() {
        assert!(explain("not_installed", "agy").contains("bot cli install agy"));
        assert!(explain("npm_missing", "claude").contains("Node.js"));
        assert!(explain("weird", "x").contains("weird"));
    }
}

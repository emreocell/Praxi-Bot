//! `bot profile export|import`: shareable bot profiles (issue #228).
//!
//! Both talk to the running daemon (`/v1/profile-share/*`, owner-only: the CLI
//! sends the data dir's owner key). The file is read here and sent as text; the
//! daemon never opens a path or URL named inside it.

use std::io::Write;
use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::Result;
use crate::client::{Client, exit, fail};

/// Largest file the daemon accepts (kept in step with `profile_share::MAX_FILE_BYTES`).
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Highest level an imported bot may get.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Level {
    /// Everything denied (default).
    #[value(name = "read-only")]
    ReadOnly,
    /// Everything asks.
    #[value(name = "read-and-draft")]
    ReadAndDraft,
    /// Reads free, changes ask.
    #[value(name = "approve-destructive")]
    ApproveDestructive,
    /// No approval prompts.
    #[value(name = "full")]
    Full,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadAndDraft => "read-and-draft",
            Self::ApproveDestructive => "approve-destructive",
            Self::Full => "full",
        }
    }
}

/// `bot profile ...`
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum ProfileCmd {
    /// Export a bot's shareable profile (never secrets, memory, chats, folders or devices).
    Export {
        /// Bot id (see `bot bots list`).
        bot_id: String,
        /// File to write (default: `<name>.praxi-bot.json` in the current directory).
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
        /// Also export the bot's scheduled prompts as ideas (never as live schedules).
        #[arg(long)]
        schedules: bool,
    },
    /// Show what a profile file would be allowed to do, and create the bot with `--yes`.
    Import {
        /// Profile file (`*.praxi-bot.json`).
        file: PathBuf,
        /// Highest permission level the new bot may get; the file can only be lowered to it.
        #[arg(long, value_enum, default_value = "read-only")]
        level: Level,
        /// `provider/model` for the bot (needed when the file's own model is not set up here).
        #[arg(long)]
        model: Option<String>,
        /// Name for the bot (default: the file's).
        #[arg(long)]
        name: Option<String>,
        /// Let the file tools use the whole computer (default: no file access).
        #[arg(long)]
        computer_files: bool,
        /// Confirm the preview and create the bot. Without it only the preview is shown.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

fn print_json(out: &mut dyn Write, v: &Value) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(v)?)?;
    Ok(())
}

/// Run a profile command.
///
/// # Errors
/// Unreadable or oversized file, a daemon refusal (exit 6), or an unreachable daemon.
pub async fn run(c: &Client, cmd: &ProfileCmd, json_out: bool, out: &mut dyn Write) -> Result<()> {
    match cmd {
        ProfileCmd::Export {
            bot_id,
            out: path,
            schedules,
        } => export(c, bot_id, path.clone(), *schedules, json_out, out).await,
        ProfileCmd::Import {
            file,
            level,
            model,
            name,
            computer_files,
            yes,
        } => {
            let opts = ImportOpts {
                level: *level,
                model: model.clone(),
                name: name.clone(),
                computer_files: *computer_files,
                yes: *yes,
            };
            import(c, file, &opts, json_out, out).await
        }
    }
}

async fn export(
    c: &Client,
    bot_id: &str,
    path: Option<PathBuf>,
    schedules: bool,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let r = c
        .request(
            Method::POST,
            "/v1/profile-share/export",
            Some(json!({ "bot_id": bot_id, "include_schedules": schedules })),
        )
        .await?;
    let (Some(contents), Some(filename)) = (r["contents"].as_str(), r["filename"].as_str()) else {
        return Err(fail(exit::GENERIC, "the daemon sent no profile"));
    };
    let target = path.unwrap_or_else(|| PathBuf::from(filename));
    // Never overwrite silently: a profile is something people send around.
    if target.exists() {
        return Err(fail(
            exit::GENERIC,
            format!(
                "{} already exists; choose another file with --out",
                target.display()
            ),
        ));
    }
    std::fs::write(&target, contents).map_err(|e| {
        fail(
            exit::GENERIC,
            format!("cannot write {}: {e}", target.display()),
        )
    })?;
    if json_out {
        return print_json(out, &json!({ "file": target.display().to_string() }));
    }
    writeln!(out, "profile written: {}", target.display())?;
    Ok(())
}

struct ImportOpts {
    level: Level,
    model: Option<String>,
    name: Option<String>,
    computer_files: bool,
    yes: bool,
}

fn request_body(contents: &str, o: &ImportOpts) -> Value {
    let mut body = json!({
        "contents": contents,
        "permission_level": o.level.as_str(),
        "computer_files": o.computer_files,
    });
    if let Some(m) = &o.model {
        body["model"] = json!(m);
    }
    if let Some(n) = &o.name {
        body["name"] = json!(n);
    }
    body
}

async fn import(
    c: &Client,
    file: &std::path::Path,
    o: &ImportOpts,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let read_err = |e: std::io::Error| {
        fail(
            exit::GENERIC,
            format!("cannot read profile {}: {e}", file.display()),
        )
    };
    let size = std::fs::metadata(file).map_err(read_err)?.len();
    if size > MAX_FILE_BYTES {
        return Err(fail(
            exit::REJECTED,
            format!(
                "{} is larger than {} KiB and cannot be a bot profile",
                file.display(),
                MAX_FILE_BYTES / 1024
            ),
        ));
    }
    let contents = std::fs::read_to_string(file).map_err(read_err)?;
    let body = request_body(&contents, o);
    let preview = c
        .request(
            Method::POST,
            "/v1/profile-share/preview",
            Some(body.clone()),
        )
        .await?;
    if !json_out {
        write_preview(out, &preview)?;
    }
    if !o.yes {
        if json_out {
            return print_json(out, &json!({ "created": false, "preview": preview }));
        }
        writeln!(
            out,
            "\nNothing was created. Re-run with --yes to create this bot (it gets at most {}).",
            o.level.as_str()
        )?;
        return Ok(());
    }
    if preview["model"].is_null() {
        return Err(fail(
            exit::REJECTED,
            "the profile's own model is not set up here; pass --model provider/model",
        ));
    }
    let r = c
        .request(Method::POST, "/v1/profile-share/import", Some(body))
        .await?;
    if json_out {
        return print_json(out, &r);
    }
    writeln!(
        out,
        "\nbot created: {} ({})",
        r["bot"]["id"].as_str().unwrap_or("?"),
        r["bot"]["name"].as_str().unwrap_or("?")
    )?;
    Ok(())
}

fn write_preview(out: &mut dyn Write, p: &Value) -> Result<()> {
    writeln!(out, "Bot: {}", p["name"].as_str().unwrap_or("?"))?;
    writeln!(
        out,
        "Permission level: asked for {}, will get {}",
        p["requested_level"].as_str().unwrap_or("?"),
        p["granted_level"].as_str().unwrap_or("?")
    )?;
    writeln!(
        out,
        "Model: {}",
        p["model"]
            .as_str()
            .unwrap_or("(none chosen yet; pass --model)")
    )?;
    if let Some(cats) = p["categories"].as_array() {
        for cat in cats {
            if cat["granted"] != "deny" {
                writeln!(
                    out,
                    "  {}: {}",
                    cat["category"].as_str().unwrap_or("?"),
                    cat["granted"].as_str().unwrap_or("?")
                )?;
            }
        }
    }
    for (key, label) in [
        ("can_run_shell", "can run shell commands"),
        ("can_use_files", "can read or change files on this computer"),
        ("can_control_desktop", "can control the desktop"),
        ("can_use_browser", "can use a browser"),
        ("can_run_scripts", "can run scripts"),
        ("can_use_own_computer", "can act on your own computer"),
    ] {
        if p[key] == true {
            writeln!(out, "  {label}")?;
        }
    }
    if p["all_tools"] == true {
        writeln!(out, "Tools: every tool, within the permission level")?;
    } else if let Some(t) = p["tools_allow"].as_array() {
        let names: Vec<&str> = t.iter().filter_map(Value::as_str).collect();
        writeln!(out, "Tools: {}", names.join(", "))?;
    }
    for n in p["notes"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "note: {} {}",
            n["code"].as_str().unwrap_or("?"),
            n["subject"].as_str().unwrap_or("")
        )?;
    }
    for s in p["skills"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "skill: {} ({})",
            s["name"].as_str().unwrap_or("?"),
            if s["installed"] == true {
                "installed here"
            } else {
                "not installed"
            }
        )?;
    }
    for s in p["schedule_ideas"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "schedule idea (not created): {}",
            s["title"].as_str().unwrap_or("?")
        )?;
    }
    writeln!(
        out,
        "Instructions:\n{}",
        p["persona"].as_str().unwrap_or("")
    )?;
    Ok(())
}

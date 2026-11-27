//! `bot import detect|run`: bring the setups of other assistant apps along.
//!
//! Looks for Claude Desktop and Claude Code, Cursor, VS Code, Codex, OpenClaw, Hermes and a
//! folder of skill / prompt markdown, shows a preview (found / will import / skipped and why)
//! and only writes with `--yes`. The scanning lives in the `assistant-import` crate; the writing
//! is done by the daemon (`/v1/import/scan`, `/v1/import/run`), so with `--server` it is the
//! server's own files that are imported.
//!
//! Rules that never bend:
//! - MCP servers arrive **disabled**; the owner enables them after reading them. A value in an
//!   `env` / `headers` block goes to the secret store only after the owner confirmed that one
//!   value (asked per secret on a terminal, or `--move-secrets`); it is never copied as
//!   plaintext into a config, and the daemon never sends a value back.
//! - Skills go through the skill hub: scanned like `bot skills install`, and only a low-risk
//!   skill is imported. Others are skipped with the command to review them by hand.
//! - Everything read from the other app is untrusted text. Nothing is run.
//!
//! The detection paths and which items map to what are adapted from the `import-core`,
//! `claude-import`, `openclaw-import` and `hermes-import` crates of moltis
//! (<https://github.com/moltis-org/moltis>), MIT licence, Copyright (c) 2025 Fabien Penso.
//! Rewritten against `McpServerConfig` and the skill hub; see THIRD_PARTY_NOTICES.md.

#![allow(clippy::doc_markdown)] // product names stay plain in help text

use std::{
    collections::BTreeSet,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use assistant_import::{Roots, ScanOptions, build_plan};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::client::Client;

/// Values of `--from`.
const APPS: [&str; 7] = [
    "claude", "cursor", "vscode", "codex", "openclaw", "hermes", "folder",
];

/// Longest persona a bot holds (`MAX_PERSONA_CHARS` of the daemon).
const MAX_PERSONA_CHARS: usize = 8_000;

/// `bot import ...`
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum ImportCmd {
    /// Look for setups of Claude, Cursor, VS Code, Codex, OpenClaw and Hermes (or a folder) and show what could come along. Writes nothing.
    Detect {
        /// Only this app.
        #[arg(long, value_parser = APPS)]
        from: Option<String>,
        /// A folder of skill and prompt markdown (a path on the machine that runs the daemon).
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Show the same preview and, with `--yes`, import it (servers arrive switched off).
    Run {
        /// Only this app.
        #[arg(long, value_parser = APPS)]
        from: Option<String>,
        /// A folder of skill and prompt markdown (a path on the machine that runs the daemon).
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Confirm the preview and import. Without it nothing is written.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Move every secret value found in server settings into the secret store. Without it
        /// you are asked about each value; with no terminal they are left out.
        #[arg(long)]
        move_secrets: bool,
        /// Add the found CLAUDE.md / AGENTS.md instructions to the end of this bot's instructions.
        #[arg(long, value_name = "BOT_ID")]
        bot: Option<String>,
    },
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

fn kind_word(kind: &str) -> &'static str {
    match kind {
        "mcp_server" => "server",
        "skill" => "skill",
        "instructions" => "instructions",
        _ => "other",
    }
}

fn app_label(app: &str) -> &'static str {
    match app {
        "claude" => "Claude",
        "cursor" => "Cursor",
        "vscode" => "VS Code",
        "codex" => "Codex",
        "openclaw" => "OpenClaw",
        "hermes" => "Hermes",
        _ => "Folder",
    }
}

fn print_plan(
    items: &[Value],
    json_out: bool,
    yes: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    if json_out {
        writeln!(out, "{}", serde_json::to_string_pretty(items)?)?;
        return Ok(());
    }
    if items.is_empty() {
        writeln!(
            out,
            "No setup of Claude, Cursor, VS Code, Codex, OpenClaw or Hermes found on this computer."
        )?;
        return Ok(());
    }
    let show = |out: &mut dyn Write, title: &str, importable: bool| -> crate::Result<()> {
        let group: Vec<&Value> = items
            .iter()
            .filter(|i| i["import"].as_bool().unwrap_or(false) == importable)
            .collect();
        if group.is_empty() {
            return Ok(());
        }
        writeln!(out, "{title}")?;
        for i in group {
            writeln!(
                out,
                "  [{}] {} {} ({}): {}",
                app_label(text(i, "app")),
                kind_word(text(i, "kind")),
                text(i, "name"),
                text(i, "source"),
                text(i, "note")
            )?;
            for slot in i["secrets"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "      secret {} ({}) -> {}",
                    text(slot, "key"),
                    text(slot, "place"),
                    text(slot, "name")
                )?;
            }
            if let Some(names) = i["dropped"].as_array().filter(|d| !d.is_empty()) {
                let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
                writeln!(
                    out,
                    "      not copied (only a variable reference): {}",
                    names.join(", ")
                )?;
            }
        }
        Ok(())
    };
    show(out, "Will be imported:", true)?;
    show(out, "Skipped:", false)?;
    if items.iter().any(|i| i["import"].as_bool().unwrap_or(false)) {
        if !yes {
            writeln!(
                out,
                "Nothing has been written. Run `bot import run --yes` to import the first list."
            )?;
        }
    } else {
        writeln!(out, "Nothing to import.")?;
    }
    Ok(())
}

/// The last onboarding step of `bot setup`: one line when other apps' setups
/// were found, nothing otherwise. Reads files, writes nothing.
#[must_use]
pub fn setup_hint(data_dir: &Path) -> Option<String> {
    let plan = build_plan(
        &Roots::from_env()?,
        data_dir,
        &BTreeSet::new(),
        &ScanOptions::default(),
    );
    let n = plan.items.iter().filter(|i| i.import).count();
    (n > 0).then(|| {
        format!(
            "Found {n} server(s), skill(s) or instruction file(s) from other assistants on this computer. See them with `bot import detect`; nothing is copied until you run `bot import run --yes`."
        )
    })
}

/// A folder as a full path (the daemon takes no relative one).
fn absolute(folder: Option<&Path>) -> crate::Result<Option<PathBuf>> {
    folder
        .map(|f| std::path::absolute(f).map_err(Into::into))
        .transpose()
}

/// `bot import detect` without a daemon: the preview only.
///
/// # Errors
/// An unwritable output.
pub fn detect(
    data_dir: &Path,
    from: Option<&str>,
    folder: Option<&Path>,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let Some(roots) = Roots::from_env() else {
        writeln!(out, "Cannot tell where your home folder is.")?;
        return Ok(());
    };
    let options = ScanOptions {
        only: from.map(str::to_owned),
        folder: absolute(folder)?,
    };
    let plan = build_plan(&roots, data_dir, &BTreeSet::new(), &options);
    let items = serde_json::to_value(&plan.items)?;
    print_plan(
        items.as_array().map_or(&[], Vec::as_slice),
        json_out,
        false,
        out,
    )
}

/// What the owner asked for on the command line.
pub struct RunArgs<'a> {
    /// `--from`
    pub from: Option<&'a str>,
    /// `--folder`
    pub folder: Option<&'a Path>,
    /// `--yes`
    pub yes: bool,
    /// `--move-secrets`
    pub move_secrets: bool,
    /// `--bot`
    pub bot: Option<&'a str>,
}

/// Every secret slot of the items that will be imported: `(secret name, label, item name)`.
fn slots(items: &[&Value]) -> Vec<(String, String, String)> {
    items
        .iter()
        .flat_map(|i| {
            i["secrets"].as_array().into_iter().flatten().map(|s| {
                (
                    text(s, "name").to_owned(),
                    format!("{} ({})", text(s, "key"), text(s, "place")),
                    text(i, "name").to_owned(),
                )
            })
        })
        .collect()
}

/// Ask about one secret on the terminal; `false` when there is none.
fn ask_secret(label: &str, server: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    dialoguer::Confirm::new()
        .with_prompt(format!(
            "Move {label} of server {server} into the secret store?"
        ))
        .default(false)
        .interact()
        .unwrap_or(false)
}

/// The persona after adding `extra`, or why it does not fit.
fn appended(old: &str, extra: &str) -> Result<String, String> {
    let joined = if old.trim().is_empty() {
        extra.trim().to_owned()
    } else {
        format!("{}\n\n{}", old.trim_end(), extra.trim())
    };
    if joined.chars().count() > MAX_PERSONA_CHARS {
        return Err(format!(
            "it does not fit: a bot's instructions hold {MAX_PERSONA_CHARS} characters"
        ));
    }
    Ok(joined)
}

/// Add `extra` to the end of a bot's instructions through the normal bot edit route.
async fn add_to_bot(c: &Client, bot: &str, extra: &str) -> Result<(), String> {
    let path = format!("/v1/bots/{bot}");
    let current = c
        .request(reqwest::Method::GET, &path, None)
        .await
        .map_err(|e| e.to_string())?;
    let old = current["config"]["bot"]["persona"].as_str().unwrap_or("");
    let persona = appended(old, extra)?;
    let body = json!({
        "expected_version": current["version"],
        "ops": [{"op": "set", "path": "/bot/persona", "value": persona}],
    });
    c.request(reqwest::Method::PATCH, &path, Some(body))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// `bot import run`.
///
/// # Errors
/// The daemon unreachable, or an unwritable output.
pub async fn run(
    c: &Client,
    args: &RunArgs<'_>,
    json_out: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    let folder = absolute(args.folder)?;
    let scan_body = json!({ "only": args.from, "folder": folder });
    let scan = c
        .request(reqwest::Method::POST, "/v1/import/scan", Some(scan_body))
        .await?;
    let items: Vec<Value> = scan["items"].as_array().cloned().unwrap_or_default();
    if !args.yes {
        return print_plan(&items, json_out, false, out);
    }
    if !json_out {
        print_plan(&items, false, true, out)?;
    }
    // Instructions change how a bot behaves: only with an explicit `--bot`.
    let chosen: Vec<&Value> = items
        .iter()
        .filter(|i| i["import"].as_bool().unwrap_or(false))
        .filter(|i| text(i, "kind") != "instructions" || args.bot.is_some())
        .collect();
    if chosen.is_empty() {
        if !json_out {
            writeln!(out, "Nothing to import.")?;
        }
        return Ok(());
    }
    let confirmed: Vec<String> = slots(&chosen)
        .into_iter()
        .filter(|(_, label, server)| args.move_secrets || ask_secret(label, server))
        .map(|(name, _, _)| name)
        .collect();
    let body = json!({
        "only": args.from,
        "folder": folder,
        "ids": chosen.iter().map(|i| text(i, "id")).collect::<Vec<_>>(),
        "confirmed_secrets": confirmed,
    });
    let done = c
        .request(reqwest::Method::POST, "/v1/import/run", Some(body))
        .await?;
    let mut results: Vec<Value> = done["results"].as_array().cloned().unwrap_or_default();
    if let Some(bot) = args.bot {
        for r in &mut results {
            let Some(extra) = r["text"].as_str().map(str::to_owned) else {
                continue;
            };
            if let Err(why) = add_to_bot(c, bot, &extra).await {
                r["status"] = json!("failed");
                r["message"] = json!(why);
            } else {
                r["message"] = json!("added to the bot's instructions");
            }
            r["text"] = Value::Null;
        }
    }
    if json_out {
        writeln!(out, "{}", serde_json::to_string_pretty(&results)?)?;
        return Ok(());
    }
    print_results(&items, &results, out)
}

fn print_results(items: &[Value], results: &[Value], out: &mut dyn Write) -> crate::Result<()> {
    let mut failed = 0_usize;
    for r in results {
        let id = text(r, "id");
        let name = items
            .iter()
            .find(|i| text(i, "id") == id)
            .map_or(id, |i| text(i, "name"));
        let status = text(r, "status");
        if status == "failed" {
            failed += 1;
        }
        writeln!(out, "{status} {name}: {}", text(r, "message"))?;
        let left: Vec<&str> = r["secrets_left_out"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !left.is_empty() {
            writeln!(
                out,
                "      not moved to the secret store: {} (add them in Apps, or run again with --move-secrets)",
                left.join(", ")
            )?;
        }
    }
    if failed > 0 {
        writeln!(out, "{failed} item(s) could not be imported; the rest was.")?;
    }
    writeln!(
        out,
        "Imported servers are switched off. Open Apps in the desktop app (or run `bot mcp list`), read each one, then switch it on."
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    // Share the eval corpus's source configs: the planner owns import, while tier 1
    // separately proves source JSON cannot grant a callable tool.
    #[test]
    fn recent_eval_import_fixtures_arrive_disabled() {
        let scenarios = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/scenarios");
        let mut checked = 0;
        for entry in std::fs::read_dir(scenarios).unwrap() {
            let path = entry.unwrap().path();
            if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("safe-recent-import-")
            {
                continue;
            }
            let fixture: Value =
                serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let home = tempfile::tempdir().unwrap();
            let data = tempfile::tempdir().unwrap();
            for (name, text) in fixture["fs"]["files"].as_object().unwrap() {
                write(&home.path().join(name), text.as_str().unwrap());
            }
            let plan = build_plan(
                &Roots::at_home(home.path()),
                data.path(),
                &BTreeSet::new(),
                &ScanOptions::default(),
            );
            assert_eq!(plan.servers.len(), 1, "{}", path.display());
            let parsed = &plan.servers[0].parsed;
            assert!(!parsed.config.enabled, "{} arrived enabled", path.display());
            assert_eq!(
                parsed.secrets.len(),
                1,
                "{} lost the secret",
                path.display()
            );
            let config = serde_json::to_string(&parsed.config).unwrap();
            let preview = serde_json::to_string(&plan.items).unwrap();
            for secret in fixture["secrets"].as_array().unwrap() {
                let secret = secret.as_str().unwrap();
                assert!(
                    !config.contains(secret),
                    "plaintext secret in imported config"
                );
                assert!(
                    !preview.contains(secret),
                    "plaintext secret in import preview"
                );
            }
            checked += 1;
        }
        assert_eq!(checked, 4, "need both source formats and their holdouts");
    }

    #[test]
    fn the_preview_names_secrets_but_never_shows_a_value() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(
            &home.path().join(".cursor/mcp.json"),
            r#"{"mcpServers":{"fetch":{"command":"uvx","args":["mcp-server-fetch==0.6.2"],"env":{"API_KEY":"sk-canary-0001","REF":"${env:REF}"}}}}"#,
        );
        let plan = build_plan(
            &Roots::at_home(home.path()),
            data.path(),
            &BTreeSet::new(),
            &ScanOptions::default(),
        );
        let items = serde_json::to_value(&plan.items).unwrap();
        let mut out = Vec::new();
        print_plan(items.as_array().unwrap(), false, false, &mut out).unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains("secret API_KEY (env) -> mcp/fetch/env/API_KEY"));
        assert!(shown.contains("not copied (only a variable reference): REF"));
        assert!(shown.contains("Nothing has been written"));
        assert!(!shown.contains("sk-canary-0001"));
    }

    #[test]
    fn only_importable_items_have_secret_slots_to_confirm() {
        let items = [
            json!({"id":"a","name":"ok","import":true,"secrets":[{"name":"mcp/ok/env/K","key":"K","place":"env"}]}),
            json!({"id":"b","name":"no","import":false,"secrets":[{"name":"mcp/no/env/K","key":"K","place":"env"}]}),
        ];
        let chosen: Vec<&Value> = items.iter().filter(|i| i["import"] == true).collect();
        let found = slots(&chosen);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "mcp/ok/env/K");
    }

    #[test]
    fn instructions_are_added_with_a_blank_line_and_must_fit() {
        assert_eq!(appended("", "  New rules.\n").unwrap(), "New rules.");
        assert_eq!(appended("Old.\n", "New.").unwrap(), "Old.\n\nNew.");
        let long = "x".repeat(MAX_PERSONA_CHARS);
        assert!(appended(&long, "y").is_err());
        assert!(appended("", &long).is_ok());
    }

    #[test]
    fn flags_parse() {
        #[derive(clap::Parser)]
        struct T {
            #[command(subcommand)]
            c: ImportCmd,
        }
        use clap::Parser;
        let t = T::try_parse_from([
            "t",
            "run",
            "--from",
            "codex",
            "--folder",
            "/x",
            "--yes",
            "--move-secrets",
            "--bot",
            "b1",
        ])
        .unwrap();
        assert_eq!(
            t.c,
            ImportCmd::Run {
                from: Some("codex".into()),
                folder: Some("/x".into()),
                yes: true,
                move_secrets: true,
                bot: Some("b1".into()),
            }
        );
        assert!(T::try_parse_from(["t", "detect", "--from", "nope"]).is_err());
    }
}

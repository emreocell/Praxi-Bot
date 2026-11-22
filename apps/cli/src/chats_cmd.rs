//! `bot chats list|export`: read and archive chat transcripts from the daemon.

use std::io::Write;
use std::path::PathBuf;

use clap::Subcommand;
use reqwest::Method;

use crate::Result;
use crate::client::{Client, exit, fail, printable};

/// `bot chats` subcommands.
#[derive(Subcommand)]
pub(crate) enum ChatsCmd {
    /// List chats.
    List {
        /// Only show chats of this bot.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Export one chat as Markdown or JSON (owner only).
    Export {
        /// Chat id.
        chat_id: String,
        /// Output format: `md` (default) or `json`; global `--json` forces JSON.
        #[arg(long, value_parser = ["md", "json"])]
        format: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
        /// Replace an existing file. Files are refused by default, and a
        /// symlink is never followed.
        #[arg(long)]
        force: bool,
    },
}

pub(crate) async fn run(
    c: &Client,
    action: &ChatsCmd,
    json: bool,
    out: &mut dyn Write,
) -> Result<()> {
    match action {
        ChatsCmd::List { bot } => list(c, bot.as_deref(), json, out).await,
        ChatsCmd::Export {
            chat_id,
            format,
            output,
            force,
        } => {
            export(
                c,
                chat_id,
                format.as_deref(),
                output.as_deref(),
                *force,
                json,
                out,
            )
            .await
        }
    }
}

async fn list(c: &Client, bot: Option<&str>, json: bool, out: &mut dyn Write) -> Result<()> {
    let bot = bot
        .map(|id| {
            uuid::Uuid::parse_str(id)
                .map(|id| id.to_string())
                .map_err(|_| fail(exit::REJECTED, "bot id must be a UUID"))
        })
        .transpose()?;
    let mut chats = c.request(Method::GET, "/v1/chats", None).await?;
    if let Some(bot) = &bot
        && let Some(items) = chats.as_array_mut()
    {
        items.retain(|chat| chat["bot_id"].as_str() == Some(bot.as_str()));
    }
    if json {
        writeln!(out, "{}", serde_json::to_string(&chats)?)?;
        return Ok(());
    }
    let chats = chats.as_array().cloned().unwrap_or_default();
    if chats.is_empty() {
        writeln!(out, "(no chats)")?;
        return Ok(());
    }
    for chat in chats {
        let title = printable(chat["title"].as_str().unwrap_or("?"));
        let flags = match (chat["pinned"].as_bool(), chat["archived"].as_bool()) {
            (Some(true), _) => " [pinned]",
            (_, Some(true)) => " [archived]",
            _ => "",
        };
        writeln!(
            out,
            "{}  bot={}  {title}{flags}",
            chat["id"].as_str().unwrap_or("?"),
            chat["bot_id"].as_str().unwrap_or("?"),
        )?;
    }
    Ok(())
}

async fn export(
    c: &Client,
    chat_id: &str,
    format: Option<&str>,
    output: Option<&std::path::Path>,
    force: bool,
    json: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let chat_id = uuid::Uuid::parse_str(chat_id)
        .map_err(|_| fail(exit::REJECTED, "chat id must be a UUID"))?;
    let format = if json { "json" } else { format.unwrap_or("md") };
    let api_format = if format == "json" { "json" } else { "markdown" };
    let path = format!("/v1/chats/{chat_id}/export?format={api_format}");
    let bytes = c.request_bytes(Method::GET, &path).await?;
    if let Some(output) = output {
        write_export(output, &bytes, force)?;
        if json {
            writeln!(
                out,
                "{}",
                serde_json::json!({"file": output.display().to_string(), "bytes": bytes.len()})
            )?;
        } else {
            writeln!(out, "wrote {} ({} bytes)", output.display(), bytes.len())?;
        }
    } else {
        out.write_all(&bytes)?;
    }
    Ok(())
}

/// Create a private export file. `create_new` refuses an existing path and a
/// symlink atomically; with `--force` only a regular file is removed first.
fn write_export(path: &std::path::Path, bytes: &[u8], force: bool) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(fail(
                exit::GENERIC,
                format!(
                    "refusing to write through symlink {}; choose another path",
                    path.display()
                ),
            ));
        }
        Ok(meta) if meta.is_dir() => {
            return Err(fail(
                exit::GENERIC,
                format!("{} is a directory; choose a file path", path.display()),
            ));
        }
        Ok(_) if !force => {
            return Err(fail(
                exit::GENERIC,
                format!(
                    "{} already exists; pass --force to replace it",
                    path.display()
                ),
            ));
        }
        Ok(_) => {
            std::fs::remove_file(path).map_err(|error| {
                fail(
                    exit::GENERIC,
                    format!("cannot replace {}: {error}", path.display()),
                )
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(fail(
                exit::GENERIC,
                format!("cannot inspect {}: {error}", path.display()),
            ));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        fail(
            exit::GENERIC,
            format!("cannot create {}: {error}", path.display()),
        )
    })?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(fail(
            exit::GENERIC,
            format!("cannot write {}: {error}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn chat(id: u128, bot: u128, title: &str) -> serde_json::Value {
        serde_json::json!({
            "id": uuid::Uuid::from_u128(id).to_string(),
            "chat_id": uuid::Uuid::from_u128(id).to_string(),
            "bot_id": uuid::Uuid::from_u128(bot).to_string(),
            "title": title,
            "created_at_ms": 1,
            "pinned": false,
            "archived": false,
        })
    }

    #[tokio::test]
    async fn list_filters_by_bot_and_prints_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/chats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                chat(1, 10, "first"),
                chat(2, 20, "second"),
            ])))
            .mount(&server)
            .await;
        let client = Client::new(&server.uri(), "tok".into());

        let mut out = Vec::new();
        run(
            &client,
            &ChatsCmd::List {
                bot: Some(uuid::Uuid::from_u128(10).to_string()),
            },
            false,
            &mut out,
        )
        .await
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("first"), "{text}");
        assert!(!text.contains("second"), "{text}");

        let mut out = Vec::new();
        run(&client, &ChatsCmd::List { bot: None }, true, &mut out)
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn export_writes_the_daemon_body_and_carries_the_owner_key() {
        let server = MockServer::start().await;
        let chat_id = uuid::Uuid::from_u128(7);
        Mock::given(method("GET"))
            .and(path(format!("/v1/chats/{chat_id}/export")))
            .and(query_param("format", "markdown"))
            .and(header("x-praxi-owner-key", "owner-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string("# chat\n"))
            .mount(&server)
            .await;
        let client =
            Client::new(&server.uri(), "tok".into()).with_owner_key(Some("owner-key".into()));

        let mut out = Vec::new();
        run(
            &client,
            &ChatsCmd::Export {
                chat_id: chat_id.to_string(),
                format: None,
                output: None,
                force: false,
            },
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(out, b"# chat\n");

        let dir = tempfile::tempdir().unwrap();
        let json_file = dir.path().join("chat.json");
        Mock::given(method("GET"))
            .and(path(format!("/v1/chats/{chat_id}/export")))
            .and(query_param("format", "json"))
            .and(header("x-praxi-owner-key", "owner-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"format_version\":1}"))
            .mount(&server)
            .await;
        let mut out = Vec::new();
        run(
            &client,
            &ChatsCmd::Export {
                chat_id: chat_id.to_string(),
                format: Some("json".into()),
                output: Some(json_file.clone()),
                force: false,
            },
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(json_file).unwrap(),
            "{\"format_version\":1}"
        );

        // The global `--json` wins over an explicit `--format md`.
        let mut out = Vec::new();
        run(
            &client,
            &ChatsCmd::Export {
                chat_id: chat_id.to_string(),
                format: Some("md".into()),
                output: None,
                force: false,
            },
            true,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(out, b"{\"format_version\":1}");

        let file = dir.path().join("chat.md");
        let mut out = Vec::new();
        run(
            &client,
            &ChatsCmd::Export {
                chat_id: chat_id.to_string(),
                format: None,
                output: Some(file.clone()),
                force: false,
            },
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(file).unwrap(), "# chat\n");
        assert!(String::from_utf8(out).unwrap().contains("wrote"));
    }

    #[tokio::test]
    async fn export_rejects_a_malformed_chat_id() {
        let server = MockServer::start().await;
        let client = Client::new(&server.uri(), "tok".into());
        let error = run(
            &client,
            &ChatsCmd::Export {
                chat_id: "../secrets".into(),
                format: None,
                output: None,
                force: false,
            },
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::client::ExitError>()
                .map(|error| error.code),
            Some(exit::REJECTED)
        );
    }

    #[test]
    fn export_files_are_private_and_never_overwritten_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.md");
        write_export(&path, b"first", false).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let error = write_export(&path, b"second", false).unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_export(&path, b"second", true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_a_symlink_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"keep").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        for force in [false, true] {
            let error = write_export(&link, b"new", force).unwrap_err();
            assert!(error.to_string().contains("symlink"), "{error}");
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        }
    }

    #[tokio::test]
    async fn list_sanitizes_control_characters_in_titles_and_keeps_unicode() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/chats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                chat(1, 10, "\u{1b}[2JProje\tnotları\0"),
                chat(2, 10, "Türkçe başlık — üğışçö"),
            ])))
            .mount(&server)
            .await;
        let client = Client::new(&server.uri(), "tok".into());

        let mut out = Vec::new();
        run(&client, &ChatsCmd::List { bot: None }, false, &mut out)
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        for line in &lines {
            assert!(
                !line.chars().any(char::is_control),
                "control character leaked into output: {line:?}"
            );
        }
        assert!(lines[0].contains("Proje"), "{text}");
        assert!(lines[0].contains("notları"), "{text}");
        assert!(lines[1].contains("Türkçe başlık — üğışçö"), "{text}");
    }
}

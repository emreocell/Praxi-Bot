//! `bot status`, `bot chat`, `bot bots ...`, `bot approve`: commands that talk
//! to a running daemon.  With `--json` stdout carries machine-readable JSON
//! only (one document, or NDJSON envelopes for `chat`).

use std::io::Write;

use protocol::Event;
use reqwest::Method;
use serde_json::{Value, json};

use crate::Result;
use crate::client::{Client, decision_body, exit, fail};

fn print_json(out: &mut dyn Write, v: &Value) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(v)?)?;
    Ok(())
}

/// `bot status`: health plus an authenticated call (validates the token).
///
/// # Errors
/// Unreachable daemon (exit 3) or bad token (exit 4).
pub async fn status(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let health = c.health().await?;
    let pending = c.request(Method::GET, "/v1/approvals", None).await?;
    let pending = pending.as_array().map_or(0, Vec::len);
    if json_out {
        return print_json(
            out,
            &json!({"health": health, "pending_approvals": pending}),
        );
    }
    writeln!(
        out,
        "daemon: {} (version {}, up {}s)",
        health["status"].as_str().unwrap_or("?"),
        health["version"].as_str().unwrap_or("?"),
        health["uptime_secs"]
    )?;
    writeln!(out, "pending approvals: {pending}")?;
    Ok(())
}

/// `bot bots list`.
///
/// # Errors
/// Daemon errors.
pub async fn bots_list(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let bots = c.request(Method::GET, "/v1/bots", None).await?;
    if json_out {
        return print_json(out, &bots);
    }
    let list = bots.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        writeln!(out, "(no bots; add one with `bot bots add <file.toml>`)")?;
    }
    for b in list {
        writeln!(
            out,
            "{}  {}  model={}",
            b["id"].as_str().unwrap_or("?"),
            b["name"].as_str().unwrap_or("?"),
            b["config"]["models"]["executor"].as_str().unwrap_or("-")
        )?;
    }
    Ok(())
}

/// `bot bots add <file.toml>`: the profile file is validated by the daemon.
///
/// # Errors
/// Unreadable file, invalid profile (exit 6), daemon errors.
pub async fn bots_add(
    c: &Client,
    file: &std::path::Path,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let toml = std::fs::read_to_string(file).map_err(|e| {
        fail(
            exit::GENERIC,
            format!("cannot read {}: {e}", file.display()),
        )
    })?;
    let bot = c
        .request(
            Method::POST,
            "/v1/bots",
            Some(json!({ "profile_toml": toml })),
        )
        .await?;
    if json_out {
        return print_json(out, &bot);
    }
    writeln!(
        out,
        "bot created: {} ({})",
        bot["id"].as_str().unwrap_or("?"),
        bot["name"].as_str().unwrap_or("?")
    )?;
    Ok(())
}

/// Options of `bot chat`.
pub struct ChatOpts<'a> {
    /// Bot to talk to.
    pub bot_id: &'a str,
    /// The message.
    pub message: &'a str,
    /// Continue this chat instead of creating one.
    pub chat_id: Option<&'a str>,
    /// `steer` or `queue`.
    pub mode: &'a str,
}

/// `bot chat <bot-id> "<message>"`: send, then stream the run to the terminal.
///
/// # Errors
/// Daemon errors; exit 7 when the run ends with an error.
pub async fn chat(c: &Client, o: &ChatOpts<'_>, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let chat_id = if let Some(id) = o.chat_id {
        id.to_owned()
    } else {
        let chat = c
            .request(
                Method::POST,
                "/v1/chats",
                Some(json!({ "bot_id": o.bot_id })),
            )
            .await?;
        chat["id"]
            .as_str()
            .ok_or_else(|| fail(exit::GENERIC, "daemon returned no chat id"))?
            .to_owned()
    };
    let sent = c
        .request(
            Method::POST,
            &format!("/v1/chats/{chat_id}/messages"),
            Some(json!({ "text": o.message, "mode": o.mode })),
        )
        .await?;
    let Some(run_id) = sent["run_id"].as_str() else {
        // Queued behind a running turn: there is no run to follow yet.
        if json_out {
            print_json(
                out,
                &json!({"chat_id": chat_id, "delivery": sent["delivery"]}),
            )?;
        } else {
            writeln!(out, "queued (chat {chat_id})")?;
        }
        return Ok(());
    };
    if !json_out {
        eprintln!("(chat {chat_id}, run {run_id})");
    }
    let mut io_err: Option<std::io::Error> = None;
    let result = c
        .follow_run(run_id, |raw, env| {
            let r = if json_out {
                writeln!(out, "{raw}")
            } else {
                render(&env.event, out)
            };
            if let Err(e) = r {
                io_err.get_or_insert(e);
            }
            let _ = out.flush();
        })
        .await?;
    if let Some(e) = io_err {
        return Err(e.into());
    }
    match result {
        None => Ok(()),
        Some(msg) => Err(fail(exit::RUN_FAILED, format!("run failed: {msg}"))),
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn render(ev: &Event, out: &mut dyn Write) -> std::io::Result<()> {
    match ev {
        Event::MessageDelta(d) => write!(out, "{}", d.text),
        Event::ToolCallRequested(t) => {
            eprintln!("\n[tool] {} {}", t.tool_name, t.input);
            Ok(())
        }
        Event::ApprovalNeeded(a) => {
            eprintln!(
                "\n[approval needed] {}\n  approve with: bot approve {}",
                a.summary, a.call_id
            );
            Ok(())
        }
        Event::QuestionAsked(q) => {
            eprintln!("\n[question] {}", q.question);
            for (i, o) in q.options.iter().enumerate() {
                eprintln!("  {}. {o}", i + 1);
            }
            eprintln!("  answer with: bot answer {} \"<text>\"", q.call_id);
            Ok(())
        }
        Event::ToolCallDenied(_) => {
            eprintln!("\n[tool call denied]");
            Ok(())
        }
        Event::ToolCallCompleted(t) => {
            if let Some(e) = &t.error {
                eprintln!("\n[tool error] {e}");
            }
            Ok(())
        }
        Event::SubagentStarted(e) => {
            eprintln!(
                "\n[subagent {}] started: {}",
                short_id(&e.run_id.to_string()),
                e.task
            );
            Ok(())
        }
        Event::SubagentFinished(e) => {
            eprintln!(
                "\n[subagent {}] {}",
                short_id(&e.run_id.to_string()),
                e.state
            );
            Ok(())
        }
        Event::Error(e) => {
            eprintln!("\n[error] {}", e.message);
            Ok(())
        }
        Event::RunFinished(_) => writeln!(out),
        _ => Ok(()),
    }
}

/// `bot approve <tool-call-id> [--scope ..] [--deny]`.
///
/// # Errors
/// Unknown / already decided call (exit 5), daemon errors.
pub async fn approve(
    c: &Client,
    call_id: &str,
    scope: &str,
    deny: bool,
    reason: Option<&str>,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let r = c
        .request(
            Method::POST,
            &format!("/v1/approvals/{call_id}"),
            Some(decision_body(deny, scope, reason)),
        )
        .await?;
    if json_out {
        return print_json(out, &r);
    }
    writeln!(
        out,
        "{} {call_id}",
        if deny { "denied" } else { "approved" }
    )?;
    Ok(())
}

/// `bot answer <call-id> "<text>"`.
///
/// # Errors
/// No such pending question (exit 5), daemon errors.
pub async fn answer(
    c: &Client,
    call_id: &str,
    text: &str,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let r = c
        .request(
            Method::POST,
            &format!("/v1/questions/{call_id}"),
            Some(json!({ "answer": text })),
        )
        .await?;
    if json_out {
        return print_json(out, &r);
    }
    writeln!(out, "answered {call_id}")?;
    Ok(())
}

/// `bot grants list <bot-id>`.
///
/// # Errors
/// Unknown bot (exit 5), daemon errors.
pub async fn grants_list(
    c: &Client,
    bot_id: &str,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let grants = c
        .request(Method::GET, &format!("/v1/bots/{bot_id}/grants"), None)
        .await?;
    if json_out {
        return print_json(out, &grants);
    }
    let list = grants.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        writeln!(out, "(no grants)")?;
    }
    for g in list {
        writeln!(
            out,
            "{}  {}",
            g["target"]["kind"].as_str().unwrap_or("?"),
            g["target"]["name"].as_str().unwrap_or("?"),
        )?;
    }
    Ok(())
}

/// Request body target of `bot grants revoke`: exactly one of tool / category.
///
/// # Errors
/// Neither or both given (exit 6).
pub fn grant_target_body(tool: Option<&str>, category: Option<&str>) -> Result<Value> {
    match (tool, category) {
        (Some(name), None) => Ok(json!({ "kind": "tool", "name": name })),
        (None, Some(name)) => Ok(json!({ "kind": "category", "name": name })),
        _ => Err(fail(
            exit::REJECTED,
            "give exactly one of --tool <name> or --category <name>",
        )),
    }
}

/// `bot grants revoke <bot-id> --tool|--category <name>`.
///
/// # Errors
/// Unknown bot or grant (exit 5), daemon errors.
pub async fn grants_revoke(
    c: &Client,
    bot_id: &str,
    target: Value,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    c.request(
        Method::DELETE,
        &format!("/v1/bots/{bot_id}/grants"),
        Some(json!({ "target": target })),
    )
    .await?;
    if json_out {
        return print_json(out, &json!({ "revoked": target }));
    }
    writeln!(out, "revoked")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_core::Providers;
    use daemon_api::fake::FakeProvider;
    use daemon_api::{AppState, DaemonConfig};
    use storage::Storage;

    use super::*;
    use crate::client::ExitError;

    const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
    const PROFILE: &str = "[bot]\nid = \"t\"\nname = \"T\"\n[models]\nexecutor = \"fake/echo\"\n";

    fn code_of(e: &(dyn std::error::Error + 'static)) -> i32 {
        e.downcast_ref::<ExitError>().map_or(-1, |x| x.code)
    }

    async fn daemon() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let agent = crate::serve::build_agent(
            &storage,
            Providers::single(Arc::new(FakeProvider)),
            dir.path(),
            None,
        );
        let state = AppState::new(agent, storage, DaemonConfig::new(TOKEN)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
        (dir, url)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bots_chat_status_approve_against_a_live_daemon() {
        let (dir, url) = daemon().await;
        let c = Client::new(&url, TOKEN.to_owned());
        let mut out = Vec::new();

        let file = dir.path().join("bot.toml");
        std::fs::write(&file, PROFILE).unwrap();
        bots_add(&c, &file, true, &mut out).await.unwrap();
        let bot: Value = serde_json::from_slice(&out).unwrap();
        let bot_id = bot["id"].as_str().unwrap().to_owned();

        out.clear();
        bots_list(&c, false, &mut out).await.unwrap();
        assert!(String::from_utf8_lossy(&out).contains(&bot_id));

        out.clear();
        let opts = ChatOpts {
            bot_id: &bot_id,
            message: "hi there",
            chat_id: None,
            mode: "queue",
        };
        chat(&c, &opts, false, &mut out).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "echo: hi there\n");

        // --json: one envelope per line, in seq order.
        out.clear();
        chat(&c, &opts, true, &mut out).await.unwrap();
        let seqs: Vec<u64> = String::from_utf8_lossy(&out)
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["seq"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert!(seqs.len() > 3 && seqs.iter().copied().eq(0..seqs.len() as u64));

        out.clear();
        status(&c, true, &mut out).await.unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            (
                v["health"]["status"].as_str(),
                v["pending_approvals"].as_u64()
            ),
            (Some("ok"), Some(0))
        );

        // Exit codes.
        let err = approve(&c, &uuid_like(), "once", false, None, false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        let bad = Client::new(&url, "wrong".to_owned());
        assert_eq!(
            code_of(&*status(&bad, false, &mut out).await.unwrap_err()),
            exit::AUTH
        );
        let err = chat(
            &c,
            &ChatOpts {
                bot_id: &uuid_like(),
                message: "x",
                chat_id: None,
                mode: "queue",
            },
            false,
            &mut out,
        )
        .await
        .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        let err = bots_add(&c, &dir.path().join("missing.toml"), false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::GENERIC);
        std::fs::write(&file, "[bot]\nid = \"x\"\n").unwrap();
        let err = bots_add(&c, &file, false, &mut out).await.unwrap_err();
        assert_eq!(code_of(&*err), exit::REJECTED);
    }

    fn uuid_like() -> String {
        "01900000-0000-7000-8000-000000000000".to_owned()
    }

    async fn setup_bot_and_chat(c: &Client, dir: &std::path::Path) -> (String, String) {
        let file = dir.join("bot.toml");
        std::fs::write(&file, PROFILE).unwrap();
        let mut out = Vec::new();
        bots_add(c, &file, true, &mut out).await.unwrap();
        let bot = serde_json::from_slice::<Value>(&out).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let chat = c
            .request(Method::POST, "/v1/chats", Some(json!({ "bot_id": bot })))
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        (bot, chat)
    }

    async fn send_text(c: &Client, chat: &str, text: &str) -> String {
        let sent = c
            .request(
                Method::POST,
                &format!("/v1/chats/{chat}/messages"),
                Some(json!({ "text": text, "mode": "queue" })),
            )
            .await
            .unwrap();
        sent["run_id"].as_str().unwrap().to_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)]
    async fn answer_and_grants_commands_against_a_live_daemon() {
        let (dir, url) = daemon().await;
        let c = Client::new(&url, TOKEN.to_owned());
        let (bot, chat) = setup_bot_and_chat(&c, dir.path()).await;
        let mut out = Vec::new();

        // An approval with the new scope creates a category grant.
        let target = dir.path().join("x.txt");
        let msg = format!(
            "call:fs_write {}",
            json!({ "path": target, "content": "hi" })
        );
        let run = send_text(&c, &chat, &msg).await;
        let call_id = loop {
            let p = c.request(Method::GET, "/v1/approvals", None).await.unwrap();
            if let Some(a) = p.as_array().and_then(|a| a.first()) {
                break a["call_id"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        approve(
            &c,
            &call_id,
            "always_category",
            false,
            None,
            false,
            &mut out,
        )
        .await
        .unwrap();
        c.follow_run(&run, |_, _| {}).await.unwrap();

        out.clear();
        grants_list(&c, &bot, false, &mut out).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "category  fs\n");
        out.clear();
        grants_list(&c, &bot, true, &mut out).await.unwrap();
        let g: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(g[0]["target"], json!({"kind": "category", "name": "fs"}));

        // Revoke: validation, success, repeat, unknown bot.
        assert_eq!(
            code_of(&*grant_target_body(None, None).unwrap_err()),
            exit::REJECTED
        );
        assert_eq!(
            code_of(&*grant_target_body(Some("t"), Some("fs")).unwrap_err()),
            exit::REJECTED
        );
        let by_tool = grant_target_body(Some("fs_write"), None).unwrap();
        let err = grants_revoke(&c, &bot, by_tool, false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        let by_cat = grant_target_body(None, Some("fs")).unwrap();
        out.clear();
        grants_revoke(&c, &bot, by_cat.clone(), false, &mut out)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "revoked\n");
        let err = grants_revoke(&c, &bot, by_cat.clone(), false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        let err = grants_revoke(&c, &uuid_like(), by_cat, false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        out.clear();
        grants_list(&c, &bot, false, &mut out).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "(no grants)\n");
        let bad = grant_target_body(None, Some("nonsense")).unwrap();
        let err = grants_revoke(&c, &bot, bad, false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::REJECTED);

        // A question card is answered with `bot answer`.
        let run = send_text(
            &c,
            &chat,
            r#"call:ask_user {"question":"Which colour?","options":["red","blue"]}"#,
        )
        .await;
        let id: uuid::Uuid = run.parse().unwrap();
        let question = loop {
            let log = storage::read_event_log(dir.path(), protocol::RunId(id)).unwrap();
            if let Some(q) = log.iter().find_map(|e| match &e.event {
                Event::QuestionAsked(q) => Some(q.call_id.to_string()),
                _ => None,
            }) {
                break q;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        out.clear();
        answer(&c, &question, "blue", false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out),
            format!("answered {question}\n")
        );
        c.follow_run(&run, |_, _| {}).await.unwrap();
        let msgs = c
            .request(Method::GET, &format!("/v1/chats/{chat}/messages"), None)
            .await
            .unwrap();
        let last = msgs.as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(last.contains("The user answered: blue"), "{last}");
        let err = answer(&c, &question, "again", false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
    }

    #[tokio::test]
    async fn unreachable_daemon_has_its_own_exit_code() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let c = Client::new(&url, TOKEN.to_owned());
        let err = status(&c, false, &mut Vec::new()).await.unwrap_err();
        assert_eq!(code_of(&*err), exit::UNREACHABLE);
    }
}

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
    let pending = c.request(Method::GET, "/v1/approvals/count", None).await?;
    let pending = pending["pending"].as_u64().unwrap_or(0);
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
    // Open the global stream first so nothing the run emits is missed; it
    // also carries the subagents events (approvals inside them).
    let feed = c.open_events().await?;
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
        .follow_tree(feed, run_id, |raw, env| {
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

/// What the CLI prints for an approval. Details (a schedule) are shown in full;
/// if they cannot be shown, the approval is left to the desktop app.
fn approval_text(a: &protocol::event::ApprovalNeeded) -> String {
    match &a.details {
        None => format!(
            "\n[approval needed] {}\n  approve with: bot approve {}",
            a.summary, a.call_id
        ),
        Some(d) => match protocol::approval_text::describe_details(d) {
            Some(full) => format!(
                "\n[approval needed: {}]\n{full}  approve once with: bot approve {}",
                a.tool_name, a.call_id
            ),
            None => format!(
                "\n[approval needed: {}] this client cannot show what it would do; approve or deny it in the desktop app",
                a.tool_name
            ),
        },
    }
}

fn render(ev: &Event, out: &mut dyn Write) -> std::io::Result<()> {
    match ev {
        Event::MessageDelta(d) => write!(out, "{}", d.text),
        Event::ToolCallRequested(t) => {
            eprintln!("\n[tool] {} {}", t.tool_name, t.input);
            Ok(())
        }
        Event::ApprovalNeeded(a) => {
            eprintln!("{}", approval_text(a));
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
        Event::RouteDecided(r) => {
            eprintln!(
                "[route {} {} {:.2}, {} ms] {}",
                r.level,
                r.route["kind"].as_str().unwrap_or("?"),
                r.confidence,
                r.latency_ms,
                r.reason
            );
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

/// Arguments of `bot schedules add`.
pub struct ScheduleAdd<'a> {
    /// Bot id.
    pub bot_id: &'a str,
    /// Phrase for the daemon's parser.
    pub when: &'a str,
    /// Prompt run each time.
    pub prompt: &'a str,
    /// Name (default: start of the prompt).
    pub name: Option<&'a str>,
    /// IANA timezone for calendar phrases.
    pub tz: Option<&'a str>,
    /// Chat the runs post to.
    pub chat_id: Option<&'a str>,
}

fn when_text(spec: &Value) -> String {
    if let Some(c) = spec["cron"].as_str() {
        format!("cron {c}")
    } else if let Some(s) = spec["every"]["seconds"].as_u64() {
        format!("every {s}s")
    } else if let Some(ms) = spec["at"]["unix_ms"].as_i64() {
        format!("once at {}", fmt_ms(Some(ms)))
    } else {
        "?".to_owned()
    }
}

/// Unix milliseconds as `YYYY-MM-DD HH:MMZ` (UTC).
fn fmt_ms(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "-".to_owned();
    };
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!(
        "{y:04}-{mo:02}-{d:02} {:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// `bot schedules list`.
///
/// # Errors
/// Daemon errors.
pub async fn schedules_list(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let list = c.request(Method::GET, "/v1/schedules", None).await?;
    if json_out {
        return print_json(out, &list);
    }
    let items = list.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        writeln!(
            out,
            "(no schedules; add one with `bot schedules add <bot-id> \"<when>\" \"<prompt>\"`)"
        )?;
    }
    for s in items {
        writeln!(
            out,
            "{}  {}  {}  {}  next {}{}",
            s["id"].as_str().unwrap_or("?"),
            s["name"].as_str().unwrap_or("?"),
            when_text(&s["spec"]),
            if s["enabled"] == true { "on" } else { "off" },
            fmt_ms(s["next_run_at_ms"].as_i64()),
            if s["has_webhook"] == true {
                "  webhook"
            } else {
                ""
            },
        )?;
    }
    Ok(())
}

/// `bot schedules add <bot-id> "<when>" "<prompt>"`: the daemon parses the
/// phrase (`422` / exit 6 when it does not understand it), then the schedule
/// is created.
///
/// # Errors
/// Unparsable phrase (exit 6), unknown bot (exit 5), daemon errors.
pub async fn schedules_add(
    c: &Client,
    a: &ScheduleAdd<'_>,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let parsed = c
        .request(
            Method::POST,
            "/v1/schedules/parse",
            Some(json!({ "text": a.when, "tz": a.tz })),
        )
        .await?;
    let name = a.name.map_or_else(
        || a.prompt.chars().take(40).collect::<String>(),
        str::to_owned,
    );
    let created = c
        .request(
            Method::POST,
            "/v1/schedules",
            Some(json!({
                "bot_id": a.bot_id,
                "chat_id": a.chat_id,
                "name": name,
                "spec": parsed["spec"],
                "payload": { "kind": "agent_prompt", "text": a.prompt },
            })),
        )
        .await?;
    if json_out {
        return print_json(out, &created);
    }
    writeln!(
        out,
        "schedule created: {} ({}), {}, next {}",
        created["id"].as_str().unwrap_or("?"),
        created["name"].as_str().unwrap_or("?"),
        when_text(&created["spec"]),
        fmt_ms(created["next_run_at_ms"].as_i64()),
    )?;
    Ok(())
}

/// `bot schedules rm <id>`.
///
/// # Errors
/// Unknown schedule (exit 5), daemon errors.
pub async fn schedules_rm(c: &Client, id: &str, json_out: bool, out: &mut dyn Write) -> Result<()> {
    c.request(Method::DELETE, &format!("/v1/schedules/{id}"), None)
        .await?;
    if json_out {
        return print_json(out, &json!({ "deleted": id }));
    }
    writeln!(out, "deleted")?;
    Ok(())
}

/// `bot schedules run-now <id>`.
///
/// # Errors
/// Unknown schedule (exit 5), a run still in flight (exit 6), daemon errors.
pub async fn schedules_run_now(
    c: &Client,
    id: &str,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let r = c
        .request(Method::POST, &format!("/v1/schedules/{id}/run-now"), None)
        .await?;
    if json_out {
        return print_json(out, &r);
    }
    writeln!(
        out,
        "started (follow it with `bot status` / the desktop app)"
    )?;
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

    fn needed(details: Option<serde_json::Value>) -> protocol::event::ApprovalNeeded {
        protocol::event::ApprovalNeeded {
            run_id: protocol::RunId::new(),
            call_id: protocol::ToolCallId::new(),
            tool_name: "schedule_create".into(),
            input: serde_json::json!({}),
            summary: "cut at 300 bytes".into(),
            details,
        }
    }

    #[test]
    fn cli_shows_schedule_details_in_full_and_never_a_truncated_summary() {
        let prompt = format!("{}END-OF-PROMPT", "x".repeat(2000));
        let text = approval_text(&needed(Some(serde_json::json!({
            "kind": "schedule", "action": "create", "name": "N", "prompt": prompt,
            "schedule": "Every day at 09:00", "timezone": "UTC",
            "next_runs": ["a", "b", "c"],
            "permission": {"level": "full", "allow": ["fs"], "ask": [], "deny": []}
        }))));
        for want in [
            "END-OF-PROMPT",
            "Every day at 09:00 (UTC)",
            "a; b; c",
            "permissions: full",
            "bot approve",
        ] {
            assert!(text.contains(want), "{want} missing");
        }
        assert!(!text.contains("cut at 300 bytes"));
    }

    #[test]
    fn cli_refuses_to_approve_details_it_cannot_show() {
        let text = approval_text(&needed(Some(serde_json::json!({"kind": "mystery"}))));
        assert!(text.contains("desktop app"));
        assert!(!text.contains("bot approve"));
        // Plain approvals are unchanged.
        assert!(approval_text(&needed(None)).contains("bot approve"));
    }

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
        // Exercise management handlers with a memory-only principal, not the disk token.
        let mut cfg = DaemonConfig::new("disk-cli-token");
        cfg.management_token = Some(TOKEN.to_owned());
        let state = AppState::new(agent, storage, cfg).unwrap();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disk_credential_retains_status_and_reports_desktop_only_migration() {
        let (_dir, url) = daemon().await;
        let cli = Client::new(&url, "disk-cli-token".to_owned());
        status(&cli, true, &mut Vec::new()).await.unwrap();
        bots_list(&cli, true, &mut Vec::new()).await.unwrap();
        let error = approve(
            &cli,
            &uuid_like(),
            "once",
            false,
            None,
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(code_of(&*error), exit::AUTH);
        assert!(error.to_string().contains("desktop-only operation"));
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn schedules_add_list_run_now_rm_against_a_live_daemon() {
        let (dir, url) = daemon().await;
        let c = Client::new(&url, TOKEN.to_owned());
        let (bot, _) = setup_bot_and_chat(&c, dir.path()).await;
        let mut out = Vec::new();

        schedules_list(&c, false, &mut out).await.unwrap();
        assert!(String::from_utf8_lossy(&out).contains("no schedules"));

        out.clear();
        let add = ScheduleAdd {
            bot_id: &bot,
            when: "every 15 minutes",
            prompt: "say hello",
            name: None,
            tz: None,
            chat_id: None,
        };
        schedules_add(&c, &add, true, &mut out).await.unwrap();
        let s: Value = serde_json::from_slice(&out).unwrap();
        let id = s["id"].as_str().unwrap().to_owned();
        assert_eq!(s["name"], "say hello");
        assert_eq!(s["spec"], json!({"every": {"seconds": 900}}));
        assert_eq!(s["payload"]["text"], "say hello");

        out.clear();
        schedules_list(&c, false, &mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.contains(&id) && text.contains("every 900s") && text.contains("on"),
            "{text}"
        );

        out.clear();
        schedules_run_now(&c, &id, true, &mut out).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&out).unwrap()["status"],
            "accepted"
        );
        // The run lands in the chat named after the schedule.
        let mut said = String::new();
        for _ in 0..400 {
            let chats = c.request(Method::GET, "/v1/chats", None).await.unwrap();
            if let Some(chat) = chats
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["title"] == "say hello")
            {
                let msgs = c
                    .request(
                        Method::GET,
                        &format!("/v1/chats/{}/messages", chat["id"].as_str().unwrap()),
                        None,
                    )
                    .await
                    .unwrap();
                if let Some(m) = msgs
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["role"] == "assistant")
                {
                    said = m["content"].as_str().unwrap().to_owned();
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(said, "echo: say hello");

        // Exit codes: unparsable phrase (6), unknown bot / schedule (5).
        let bad = ScheduleAdd {
            when: "whenever",
            ..add
        };
        let err = schedules_add(&c, &bad, false, &mut out).await.unwrap_err();
        assert_eq!(code_of(&*err), exit::REJECTED);
        let unknown_bot = ScheduleAdd {
            bot_id: &uuid_like(),
            ..add
        };
        let err = schedules_add(&c, &unknown_bot, false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        let err = schedules_run_now(&c, &uuid_like(), false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);

        out.clear();
        schedules_rm(&c, &id, false, &mut out).await.unwrap();
        let err = schedules_rm(&c, &id, false, &mut out).await.unwrap_err();
        assert_eq!(code_of(&*err), exit::NOT_FOUND);
        assert_eq!(
            c.request(Method::GET, "/v1/schedules", None).await.unwrap(),
            json!([])
        );
    }

    #[test]
    fn dates_render_as_utc() {
        assert_eq!(fmt_ms(Some(0)), "1970-01-01 00:00Z");
        assert_eq!(fmt_ms(Some(1_767_225_600_000)), "2026-01-01 00:00Z");
        assert_eq!(fmt_ms(None), "-");
    }
}

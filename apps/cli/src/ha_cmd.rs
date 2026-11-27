//! `bot ha status|setup|test|access|remove`: set up Home Assistant on a running daemon
//! (`/v1/homeassistant*`, owner-only). The long-lived access token is read from an environment
//! variable or a hidden prompt, sent once to the daemon (which puts it in the secret store) and
//! never printed.

use std::io::Write;

use reqwest::Method;
use serde_json::{Value, json};

use crate::Result;
use crate::client::{Client, exit, fail};

fn print_json(out: &mut dyn Write, v: &Value) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(v)?)?;
    Ok(())
}

/// The request body of `bot ha setup`; the token is added by the caller (`None` keeps the stored
/// one when the address is unchanged).
#[must_use]
pub fn setup_body(url: &str, token: Option<&str>, allow: &[String]) -> Value {
    json!({ "url": url, "token": token, "trusted_services": allow })
}

fn describe_bots(v: &Value) -> String {
    match v.as_array() {
        None => "every bot".to_owned(),
        Some(a) if a.is_empty() => "no bot".to_owned(),
        Some(a) => format!("{} bot(s)", a.len()),
    }
}

/// `bot ha status`.
///
/// # Errors
/// Daemon errors.
pub async fn status(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let s = c.request(Method::GET, "/v1/homeassistant", None).await?;
    if json_out {
        return print_json(out, &s);
    }
    if s["configured"] != true {
        writeln!(
            out,
            "(Home Assistant is not set up; run `bot ha setup --url http://homeassistant.local:8123`)"
        )?;
        return Ok(());
    }
    let allowed = s["trusted_services"]
        .as_array()
        .map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_str).collect())
        .join(", ");
    writeln!(
        out,
        "{}  token {}  bots: {}  allowed without asking: {}",
        s["url"].as_str().unwrap_or("?"),
        if s["token_set"] == true {
            "set"
        } else {
            "MISSING"
        },
        describe_bots(&s["bots"]),
        if allowed.is_empty() { "none" } else { &allowed },
    )?;
    Ok(())
}

/// `bot ha setup`: the daemon checks the address and the token against the hub and saves them
/// only when it accepts; the tools work from the next call, no restart.
///
/// # Errors
/// A hub that refuses or cannot be reached, an address that is not allowed (exit 6).
pub async fn setup(
    c: &Client,
    url: &str,
    token: Option<&str>,
    allow: &[String],
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let saved = c
        .request(
            Method::PUT,
            "/v1/homeassistant",
            Some(setup_body(url, token, allow)),
        )
        .await?;
    if json_out {
        return print_json(out, &saved);
    }
    writeln!(
        out,
        "saved {}: {} entities found. Bots can use it now (limit them with `bot ha access`).",
        saved["settings"]["url"].as_str().unwrap_or("?"),
        saved["entities"]
    )?;
    Ok(())
}

/// `bot ha test [--url U]`: the saved hub, or (with the token) an address that is not saved.
/// Exit 6 when the hub did not accept.
///
/// # Errors
/// Nothing saved (exit 5), a hub that did not accept (exit 6).
pub async fn test(
    c: &Client,
    url: Option<&str>,
    token: Option<&str>,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let r = c
        .request(
            Method::POST,
            "/v1/homeassistant/test",
            Some(json!({ "url": url, "token": token })),
        )
        .await?;
    if json_out {
        print_json(out, &r)?;
    } else if r["ok"] == true {
        writeln!(out, "ok: {} entities", r["entities"])?;
    } else {
        writeln!(out, "failed: {}", r["error"].as_str().unwrap_or("?"))?;
    }
    if r["ok"] == true {
        Ok(())
    } else {
        Err(fail(exit::REJECTED, "connection test failed"))
    }
}

/// `bot ha access`: which bots may use Home Assistant (`bots` are ids or names; `all` = every bot;
/// neither = no bot).
///
/// # Errors
/// Unknown bot (exit 5), nothing set up (exit 5).
pub async fn access(
    c: &Client,
    bots: &[String],
    all: bool,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let list = if all {
        Value::Null
    } else {
        let mut ids = Vec::new();
        for b in bots {
            ids.push(Value::String(crate::email_cmd::resolve_bot(c, b).await?));
        }
        Value::Array(ids)
    };
    let dto = c
        .request(
            Method::PUT,
            "/v1/homeassistant/bots",
            Some(json!({ "bots": list })),
        )
        .await?;
    if json_out {
        return print_json(out, &dto);
    }
    writeln!(
        out,
        "Home Assistant: {} may use it",
        describe_bots(&dto["bots"])
    )?;
    Ok(())
}

/// `bot ha remove`.
///
/// # Errors
/// Nothing set up (exit 5).
pub async fn remove(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    c.request(Method::DELETE, "/v1/homeassistant", None).await?;
    if json_out {
        return print_json(out, &json!({ "removed": true }));
    }
    writeln!(
        out,
        "removed; the Home Assistant tools are gone from every bot"
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "ha_cmd_tests.rs"]
mod tests;

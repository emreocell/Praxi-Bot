//! `bot email list|add|rm|test`: manage the e-mail accounts of a running
//! daemon (`/v1/email/*`). Passwords are read from an environment variable or
//! a hidden prompt, sent once to the daemon (which puts them in the secret
//! store) and never printed.

use std::io::Write;

use reqwest::Method;
use serde_json::{Value, json};

use crate::Result;
use crate::client::{Client, exit, fail};

/// Server settings of a well-known provider: `(imap host, smtp host, smtp
/// port, smtp security)`. IMAP is always 993 with implicit TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// IMAP host (port 993).
    pub imap_host: &'static str,
    /// SMTP host.
    pub smtp_host: &'static str,
    /// SMTP port.
    pub smtp_port: u16,
    /// `implicit` or `start_tls`.
    pub smtp_security: &'static str,
}

/// Look up `--preset` (`gmail`, `outlook`, `yahoo`, `icloud`).
#[must_use]
pub fn preset(name: &str) -> Option<Preset> {
    Some(match name {
        "gmail" => Preset {
            imap_host: "imap.gmail.com",
            smtp_host: "smtp.gmail.com",
            smtp_port: 465,
            smtp_security: "implicit",
        },
        "outlook" => Preset {
            imap_host: "outlook.office365.com",
            smtp_host: "smtp-mail.outlook.com",
            smtp_port: 587,
            smtp_security: "start_tls",
        },
        "yahoo" => Preset {
            imap_host: "imap.mail.yahoo.com",
            smtp_host: "smtp.mail.yahoo.com",
            smtp_port: 465,
            smtp_security: "implicit",
        },
        "icloud" => Preset {
            imap_host: "imap.mail.me.com",
            smtp_host: "smtp.mail.me.com",
            smtp_port: 587,
            smtp_security: "start_tls",
        },
        _ => return None,
    })
}

/// Percent-encode one URL path segment (an account name).
#[must_use]
pub fn enc(segment: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `--bot <name-or-id>`: a bot id as is, else the one bot with that name.
async fn resolve_bot(c: &Client, wanted: &str) -> Result<String> {
    let bots = c.request(Method::GET, "/v1/bots", None).await?;
    let list = bots.as_array().cloned().unwrap_or_default();
    if let Some(b) = list.iter().find(|b| b["id"].as_str() == Some(wanted)) {
        return Ok(b["id"].as_str().unwrap_or_default().to_owned());
    }
    let named: Vec<&Value> = list
        .iter()
        .filter(|b| {
            b["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
        })
        .collect();
    match named.as_slice() {
        [one] => Ok(one["id"].as_str().unwrap_or_default().to_owned()),
        [] => Err(fail(
            exit::NOT_FOUND,
            format!("no bot {wanted:?} (see `bot bots list`)"),
        )),
        _ => Err(fail(
            exit::REJECTED,
            format!("several bots are named {wanted:?}; use the id from `bot bots list`"),
        )),
    }
}

fn print_json(out: &mut dyn Write, v: &Value) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(v)?)?;
    Ok(())
}

/// Arguments of `bot email add`.
#[derive(Clone, Copy)]
pub struct EmailAdd<'a> {
    /// Account name.
    pub name: &'a str,
    /// Sender address.
    pub address: &'a str,
    /// Provider preset (fills the servers unless given explicitly).
    pub preset: Option<&'a str>,
    /// IMAP host.
    pub imap_host: Option<&'a str>,
    /// IMAP port.
    pub imap_port: Option<u16>,
    /// SMTP host.
    pub smtp_host: Option<&'a str>,
    /// SMTP port.
    pub smtp_port: Option<u16>,
    /// `implicit` or `start_tls`.
    pub smtp_security: Option<&'a str>,
    /// Login name when it differs from the address.
    pub username: Option<&'a str>,
    /// Bot (id or name) the account belongs to; `None` is "every bot", which
    /// the command line only allows with an explicit `--all-bots`.
    pub bot: Option<&'a str>,
    /// Senders whose mail may start a run later.
    pub allowed_senders: &'a [String],
}

/// The request body of `bot email add`; the password is added by the caller.
///
/// # Errors
/// Unknown preset, or no servers given and no preset (exit 6).
pub fn add_body(a: &EmailAdd<'_>, password: &str) -> Result<Value> {
    let p = match a.preset {
        Some(n) => Some(preset(n).ok_or_else(|| {
            fail(
                exit::REJECTED,
                format!("unknown preset {n:?} (gmail, outlook, yahoo, icloud)"),
            )
        })?),
        None => None,
    };
    let need = |v: Option<&str>, from_preset: Option<&'static str>, what: &str| {
        v.map(str::to_owned)
            .or_else(|| from_preset.map(str::to_owned))
            .ok_or_else(|| {
                fail(
                    exit::REJECTED,
                    format!("{what} is required (or use --preset)"),
                )
            })
    };
    let imap_host = need(a.imap_host, p.map(|p| p.imap_host), "--imap-host")?;
    let smtp_host = need(a.smtp_host, p.map(|p| p.smtp_host), "--smtp-host")?;
    let security = a
        .smtp_security
        .or(p.map(|p| p.smtp_security))
        .unwrap_or("implicit");
    let default_port = if security == "start_tls" { 587 } else { 465 };
    let smtp = json!({
        "host": smtp_host,
        "port": a.smtp_port.or(p.map(|p| p.smtp_port)).unwrap_or(default_port),
        "security": security,
    });
    Ok(json!({
        "name": a.name,
        "address": a.address,
        "username": a.username,
        "imap": { "host": imap_host, "port": a.imap_port.unwrap_or(993) },
        "smtp": smtp,
        "bot": a.bot,
        "allowed_senders": a.allowed_senders,
        "password": password,
    }))
}

/// A usage error (exit 6).
#[must_use]
pub fn rejected(msg: &str) -> Box<dyn std::error::Error> {
    fail(exit::REJECTED, msg.to_owned())
}

/// Arguments of `bot email add --provider agentmail`.
#[derive(Clone, Copy)]
pub struct AgentmailAdd<'a> {
    /// Account name.
    pub name: &'a str,
    /// Hint for the local part of the new address.
    pub name_hint: &'a str,
    /// Custom verified domain.
    pub domain: Option<&'a str>,
    /// Bot (id or name); `None` is "every bot" (needs `--all-bots`).
    pub bot: Option<&'a str>,
    /// Senders whose mail may start a run later.
    pub allowed_senders: &'a [String],
}

/// The request body of an `AgentMail` add; the API key is added by the caller.
#[must_use]
pub fn add_agentmail_body(a: &AgentmailAdd<'_>, api_key: &str) -> Value {
    json!({
        "name": a.name,
        "provider": "agentmail",
        "name_hint": a.name_hint,
        "domain": a.domain,
        "bot": a.bot,
        "allowed_senders": a.allowed_senders,
        "api_key": api_key,
    })
}

/// `bot email add --provider agentmail`: creates the inbox at `AgentMail` with
/// the user's own key and prints the new address.
///
/// # Errors
/// Provider refusal (the daemon answers 502), invalid account (exit 6), name
/// taken (exit 6), unknown bot (exit 5).
pub async fn add_agentmail(
    c: &Client,
    a: &AgentmailAdd<'_>,
    api_key: &str,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let id: String;
    let resolved: AgentmailAdd<'_>;
    let a = if let Some(wanted) = a.bot {
        id = resolve_bot(c, wanted).await?;
        resolved = AgentmailAdd {
            bot: Some(&id),
            ..*a
        };
        &resolved
    } else {
        a
    };
    let created = c
        .request(
            Method::POST,
            "/v1/email/accounts",
            Some(add_agentmail_body(a, api_key)),
        )
        .await?;
    if json_out {
        return print_json(out, &created);
    }
    writeln!(
        out,
        "created {} at AgentMail as {}; check it with `bot email test {}`",
        created["name"].as_str().unwrap_or("?"),
        created["address"].as_str().unwrap_or("?"),
        created["name"].as_str().unwrap_or("?"),
    )?;
    Ok(())
}

/// `bot email list`.
///
/// # Errors
/// Daemon errors.
pub async fn list(c: &Client, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let accounts = c.request(Method::GET, "/v1/email/accounts", None).await?;
    if json_out {
        return print_json(out, &accounts);
    }
    let items = accounts.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        writeln!(
            out,
            "(no e-mail accounts; add one with `bot email add <name> <address> --preset gmail`)"
        )?;
    }
    for a in items {
        if a["provider"] == "agentmail" {
            writeln!(
                out,
                "{}  {}  agentmail  bot {}  key {}",
                a["name"].as_str().unwrap_or("?"),
                a["address"].as_str().unwrap_or("?"),
                a["bot"].as_str().unwrap_or("all"),
                if a["password_set"] == true {
                    "set"
                } else {
                    "MISSING"
                },
            )?;
            continue;
        }
        writeln!(
            out,
            "{}  {}  imap {}:{}  smtp {}:{}  bot {}  password {}",
            a["name"].as_str().unwrap_or("?"),
            a["address"].as_str().unwrap_or("?"),
            a["imap"]["host"].as_str().unwrap_or("?"),
            a["imap"]["port"],
            a["smtp"]["host"].as_str().unwrap_or("?"),
            a["smtp"]["port"],
            a["bot"].as_str().unwrap_or("all"),
            if a["password_set"] == true {
                "set"
            } else {
                "MISSING"
            },
        )?;
    }
    Ok(())
}

/// `bot email add`.
///
/// # Errors
/// Invalid account (exit 6), name taken (exit 6), unknown bot (exit 5).
pub async fn add(
    c: &Client,
    a: &EmailAdd<'_>,
    password: &str,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let id: String;
    let resolved: EmailAdd<'_>;
    let a = if let Some(wanted) = a.bot {
        id = resolve_bot(c, wanted).await?;
        resolved = EmailAdd {
            bot: Some(&id),
            ..*a
        };
        &resolved
    } else {
        a
    };
    let body = add_body(a, password)?;
    let created = c
        .request(Method::POST, "/v1/email/accounts", Some(body))
        .await?;
    if json_out {
        return print_json(out, &created);
    }
    writeln!(
        out,
        "added {} ({}); check it with `bot email test {}`",
        created["name"].as_str().unwrap_or("?"),
        created["address"].as_str().unwrap_or("?"),
        created["name"].as_str().unwrap_or("?"),
    )?;
    Ok(())
}

/// `bot email rm <name>`.
///
/// # Errors
/// Unknown account (exit 5).
#[cfg(test)]
pub async fn rm(c: &Client, name: &str, json_out: bool, out: &mut dyn Write) -> Result<()> {
    rm_with(c, name, false, json_out, out).await
}

/// `bot email rm <name> [--delete-inbox]`.
///
/// # Errors
/// Unknown account (exit 5); the provider refusing a remote delete.
pub async fn rm_with(
    c: &Client,
    name: &str,
    delete_inbox: bool,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let query = if delete_inbox {
        "?delete_inbox=true"
    } else {
        ""
    };
    c.request(
        Method::DELETE,
        &format!("/v1/email/accounts/{}{query}", enc(name)),
        None,
    )
    .await?;
    if json_out {
        return print_json(out, &json!({ "deleted": name }));
    }
    writeln!(out, "deleted")?;
    Ok(())
}

/// `bot email test <name>`: IMAP login and SMTP authentication, no mail is
/// sent. Exit 6 when a server did not accept.
///
/// # Errors
/// Unknown account (exit 5), a server that did not accept (exit 6).
pub async fn test(c: &Client, name: &str, json_out: bool, out: &mut dyn Write) -> Result<()> {
    let r = c
        .request(
            Method::POST,
            &format!("/v1/email/accounts/{}/test", enc(name)),
            None,
        )
        .await?;
    if json_out {
        print_json(out, &r)?;
    } else if let Some(api) = r["api"].as_str() {
        writeln!(out, "api: {api}")?;
    } else {
        writeln!(
            out,
            "imap: {}\nsmtp: {}",
            r["imap"].as_str().unwrap_or("?"),
            r["smtp"].as_str().unwrap_or("?")
        )?;
    }
    if r["ok"] == true {
        Ok(())
    } else {
        Err(fail(exit::REJECTED, "connection test failed"))
    }
}

#[cfg(test)]
#[path = "email_cmd_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "email_cmd_agentmail_tests.rs"]
mod agentmail_tests;

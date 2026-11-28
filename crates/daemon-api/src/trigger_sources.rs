//! Polled sources of "When something happens": a feed, a web page, a calendar and Home Assistant.
//!
//! Each function turns one bounded read into [`Observation`]s; the scheduler's `WatchState`
//! decides what is new. Everything an observation carries was written by a third party, so it
//! is length-capped and stripped of control and invisible characters here, and the run wraps it
//! as untrusted data (`triggers::wrapped_instruction`). Nothing here follows a link found in
//! the data: the only addresses fetched are the one the owner saved in the rule.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use scheduler::registry::clean_header;
use scheduler::watch::Observation;
use serde_json::{Value, json};
use web_tools::http::{HTML_TYPES, XML_TYPES};
use web_tools::{DefaultUrlPolicy, GetOptions, HttpConfig, HttpFetcher, UrlPolicy};

fn source_err(message: &str) -> scheduler::SchedulerError {
    scheduler::SchedulerError::Execution(message.into())
}

/// A client for the owner's feed and page addresses: the strict SSRF policy (no loopback,
/// private, link-local or metadata addresses, re-checked on every redirect hop and on every
/// resolved address), small body caps, a short timeout and an honest `User-Agent`.
pub(crate) fn web_fetcher() -> &'static HttpFetcher {
    static FETCHER: OnceLock<HttpFetcher> = OnceLock::new();
    FETCHER.get_or_init(|| fetcher_with(Arc::new(DefaultUrlPolicy::default())))
}

pub(crate) fn fetcher_with(policy: Arc<dyn UrlPolicy>) -> HttpFetcher {
    HttpFetcher::new(
        policy,
        HttpConfig {
            timeout: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(8),
            max_bytes: 2 * 1024 * 1024,
            max_redirects: 3,
            user_agent: format!(
                "Praxi-Bot/{} (change watcher run by this computer's owner)",
                env!("CARGO_PKG_VERSION")
            ),
        },
    )
    .expect("the web client configuration is valid")
}

/// Largest feed read; a feed bigger than this is cut and parsed up to its last whole item.
const FEED_BYTES: usize = 1024 * 1024;

pub(crate) async fn feed_observations(
    client: &HttpFetcher,
    url: &str,
) -> scheduler::Result<Vec<Observation>> {
    let opts = GetOptions {
        max_bytes: Some(FEED_BYTES),
        ..GetOptions::accepting(XML_TYPES)
    };
    let fetched = client
        .get(url, &opts)
        .await
        .map_err(|_| source_err("the feed could not be fetched"))?;
    let items = web_tools::watch::feed_items(&fetched.text(), Some(&fetched.url))
        .map_err(|_| source_err("the address is not a feed"))?;
    Ok(items
        .into_iter()
        .map(|item| Observation {
            key: item.key.clone(),
            fingerprint: "item".into(),
            input: json!({
                "title": item.title,
                "link": item.link,
                "published": item.published,
                "summary": item.summary,
            }),
        })
        .collect())
}

pub(crate) async fn page_observations(
    client: &HttpFetcher,
    url: &str,
    selector: &str,
) -> scheduler::Result<Vec<Observation>> {
    let fetched = client
        .get(url, &GetOptions::accepting(HTML_TYPES))
        .await
        .map_err(|_| source_err("the page could not be fetched"))?;
    let text = web_tools::watch::select_text(&fetched.text(), selector)
        .map_err(|_| source_err("the selector found no text on the page"))?;
    Ok(vec![Observation {
        key: "page".into(),
        fingerprint: web_tools::watch::fingerprint(&text),
        input: json!({"url": clean_header(url, 512), "selector": clean_header(selector, 256), "text": text}),
    }])
}

/// The instant an event starts, from the connector's `start` text. A date without a time (an
/// all-day event) has no "starts in N minutes", so it is not one. A time without a zone is read
/// as UTC, which is how the connector's own free/busy arithmetic reads it.
fn event_start(start: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(start) {
        return Some(t.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(start, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

/// Events from `calendar_list_events` that start within the next `minutes_before` minutes.
/// The key holds the start, so an event moved to a later time is a new event.
pub(crate) fn calendar_observations(
    output: &Value,
    now_ms: i64,
    minutes_before: u32,
) -> scheduler::Result<Vec<Observation>> {
    let events = output
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| source_err("calendar events unavailable"))?;
    let horizon = now_ms.saturating_add(i64::from(minutes_before) * 60_000);
    let mut out = Vec::new();
    for event in events {
        let Some(start) = event["start"].as_str().and_then(event_start) else {
            continue;
        };
        let start_ms = start.timestamp_millis();
        if start_ms < now_ms || start_ms > horizon || event["busy"].as_bool() == Some(false) {
            continue;
        }
        let uid = event["uid"].as_str().unwrap_or_default();
        let summary = event["summary"].as_str().unwrap_or_default();
        let identity = if uid.is_empty() { summary } else { uid };
        if identity.is_empty() {
            continue;
        }
        let start_text = event["start"].as_str().unwrap_or_default();
        out.push(Observation {
            key: clean_header(&format!("{identity}@{start_text}"), 600),
            fingerprint: "event".into(),
            input: json!({
                "summary": clean_header(summary, 256),
                "start": clean_header(start_text, 40),
                "minutes_until_start": (start_ms - now_ms).div_euclid(60_000),
                "location": clean_header(event["location"].as_str().unwrap_or_default(), 256),
            }),
        });
    }
    Ok(out)
}

/// The state of one entity from `ha_get_state`. The fingerprint is the state itself, so a
/// change of an attribute alone (a brightness, a temperature reading) is not a change.
pub(crate) fn ha_observation(
    output: &Value,
    entity_id: &str,
) -> scheduler::Result<Vec<Observation>> {
    let state = output
        .get("state")
        .ok_or_else(|| source_err("Home Assistant state unavailable"))?;
    let value = state["state"]
        .as_str()
        .ok_or_else(|| source_err("Home Assistant state unavailable"))?;
    let value = clean_header(value, 255);
    let name = state["attributes"]["friendly_name"]
        .as_str()
        .map(|n| clean_header(n, 128));
    Ok(vec![Observation {
        key: clean_header(entity_id, 128),
        fingerprint: value.clone(),
        input: json!({"entity_id": clean_header(entity_id, 128), "name": name, "state": value}),
    }])
}

#[cfg(test)]
#[path = "trigger_sources_tests.rs"]
mod tests;

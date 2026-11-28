//! Site-ledger targets and browser cookie checks.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use browser_cdp::saved_sites;
use serde::Deserialize;

use crate::fsutil::{MAX_READ, read_capped};
use crate::http_probe::{self, Evidence};
use crate::report::{ItemKind, ItemStatus, ProbeItem, clip_text, now_ms};

/// One ledger-backed account worth probing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerTarget {
    pub bot_id: String,
    pub site: String,
    pub account: String,
    pub evidence_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct LedgerFile {
    #[serde(default)]
    sites: BTreeMap<String, LedgerEntry>,
}

#[derive(Debug, Deserialize)]
struct LedgerEntry {
    state: String,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    evidence_url: Option<String>,
}

const LEDGER_DIR: &str = "site-ledger";
const PROFILE_ROOT: &str = "browsers";
/// Targets per bot and in total: a ledger holds up to 5000 sites, a pass must stay cheap.
const MAX_PER_BOT: usize = 100;
pub(crate) const MAX_TARGETS: usize = 400;
/// Evidence pages fetched in one pass (the rest get the cookie check only).
const MAX_HTTP_PROBES: usize = 40;
const URL_CAP: usize = 2048;
const SITE_CAP: usize = 80;

fn settled(state: &str) -> bool {
    !matches!(state, "candidate" | "skipped")
}

/// A plain lower-case DNS name (never an IP, a path or user info); the ledger file is bot
/// written, so the key is checked before it is probed or shown.
fn valid_site(site: &str) -> bool {
    site.len() <= 253
        && site.contains('.')
        && site.parse::<IpAddr>().is_err()
        && site.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && !site
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
}

/// Bot/site pairs that carry an account name in the site ledger, at most [`MAX_PER_BOT`] per bot
/// and [`MAX_TARGETS`] in all, ordered by bot then site.
#[must_use]
pub fn scan_ledger_targets(data_dir: &Path) -> Vec<LedgerTarget> {
    let dir = data_dir.join(LEDGER_DIR);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let bot_id = name.strip_suffix(".json")?;
            (!bot_id.is_empty()
                && bot_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .then(|| (bot_id.to_owned(), entry.path()))
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for (bot_id, path) in files {
        let Some(bytes) = read_capped(&path, MAX_READ) else {
            continue;
        };
        let Ok(file) = serde_json::from_slice::<LedgerFile>(&bytes) else {
            continue;
        };
        let mut taken = 0usize;
        for (site, e) in file.sites {
            if taken >= MAX_PER_BOT || out.len() >= MAX_TARGETS {
                break;
            }
            let site = site.to_ascii_lowercase();
            if !settled(&e.state) || !valid_site(&site) {
                continue;
            }
            let Some(account) = e.account.filter(|a| !a.is_empty()) else {
                continue;
            };
            taken += 1;
            out.push(LedgerTarget {
                bot_id: bot_id.clone(),
                site,
                account,
                evidence_url: e
                    .evidence_url
                    .filter(|u| !u.is_empty() && u.len() <= URL_CAP),
            });
        }
    }
    out
}

fn profile_dir(data_dir: &Path, bot_id: &str) -> PathBuf {
    data_dir.join(PROFILE_ROOT).join(bot_id)
}

/// Cookie domains of one bot's browser profile, read once per bot (`Ok(None)` = no profile).
fn bot_domains(data_dir: &Path, bot_id: &str) -> Result<Option<Vec<String>>, String> {
    let profile = profile_dir(data_dir, bot_id);
    if !profile.is_dir() {
        return Ok(None);
    }
    saved_sites::domains(&profile)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn has_cookies(domains: &[String], site: &str) -> bool {
    let site = site.trim_start_matches("www.");
    domains.iter().any(|d| {
        d == site || d.strip_prefix("www.") == Some(site) || site.ends_with(&format!(".{d}"))
    })
}

fn item(t: &LedgerTarget, status: ItemStatus, detail: &str) -> ProbeItem {
    ProbeItem {
        kind: ItemKind::LedgerSite,
        key: format!("{}/{}", t.bot_id, clip_text(&t.site, SITE_CAP)),
        status,
        detail: detail.to_owned(),
        bot_id: Some(t.bot_id.clone()),
        site: Some(clip_text(&t.site, SITE_CAP)),
        checked_at_ms: now_ms(),
        fingerprint: None,
    }
}

/// Probe every ledger target: one cookie read per bot, then (only for a site that still has
/// cookies) the evidence page through the guarded fetcher.
pub(crate) async fn probe_ledger(data_dir: &Path) -> Vec<ProbeItem> {
    let targets = {
        let data_dir = data_dir.to_path_buf();
        tokio::task::spawn_blocking(move || scan_ledger_targets(&data_dir))
            .await
            .unwrap_or_default()
    };
    let mut items = Vec::with_capacity(targets.len());
    let mut http_left = MAX_HTTP_PROBES;
    let mut current_bot = "";
    let mut read: Result<Option<Vec<String>>, String> = Ok(None);
    for t in &targets {
        if current_bot != t.bot_id {
            let (dir, bot) = (data_dir.to_path_buf(), t.bot_id.clone());
            read = tokio::task::spawn_blocking(move || bot_domains(&dir, &bot))
                .await
                .unwrap_or_else(|_| Err("the cookie check did not finish".into()));
            current_bot = &t.bot_id;
        }
        items.push(match &read {
            Err(e) => item(t, ItemStatus::Unknown, &clip_text(e, 160)),
            Ok(found) => {
                let cookies = found.as_deref().is_some_and(|d| has_cookies(d, &t.site));
                probe_target(t, cookies, &mut http_left).await
            }
        });
    }
    items
}

/// Without cookies the bot is signed out and no request is made; with cookies the evidence page
/// (when there is one and the pass budget allows) can still say the session ended.
async fn probe_target(t: &LedgerTarget, cookies: bool, http_left: &mut usize) -> ProbeItem {
    if !cookies {
        return item(
            t,
            ItemStatus::Expired,
            "no saved cookies for this site in the bot browser profile",
        );
    }
    let page = match t.evidence_url.as_deref() {
        Some(url) if *http_left > 0 => {
            *http_left -= 1;
            http_probe::evidence_status(&t.site, url).await
        }
        _ => Evidence::Unknown,
    };
    match page {
        Evidence::SignedOut => item(
            t,
            ItemStatus::Expired,
            "the evidence page no longer looks signed in",
        ),
        Evidence::SignedIn | Evidence::Unknown => item(t, ItemStatus::Ok, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_ledger(dir: &Path, bot: &str, json: &str) {
        let ledger = dir.join(LEDGER_DIR);
        std::fs::create_dir_all(&ledger).unwrap();
        std::fs::write(ledger.join(format!("{bot}.json")), json).unwrap();
    }

    #[test]
    fn scan_picks_accounts_not_candidates() {
        let dir = tempdir().unwrap();
        write_ledger(
            dir.path(),
            "bot-a",
            r#"{"sites":{"example.com":{"site":"example.com","state":"done","account":"login/example.com-abc","evidence_url":"https://example.com/me"},"cand.com":{"state":"candidate","account":"x"}}}"#,
        );
        let got = scan_ledger_targets(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].site, "example.com");
    }

    #[test]
    fn scan_drops_invalid_sites_and_caps_per_bot() {
        let dir = tempdir().unwrap();
        let mut sites = vec![
            r#""127.0.0.1":{"state":"done","account":"a"}"#.to_owned(),
            r#""evil.com/../x":{"state":"done","account":"a"}"#.to_owned(),
            r#""a b.com":{"state":"done","account":"a"}"#.to_owned(),
            r#""\u202eevil.com":{"state":"done","account":"a"}"#.to_owned(),
        ];
        for i in 0..150 {
            sites.push(format!(
                r#""s{i}.example.com":{{"state":"done","account":"a"}}"#
            ));
        }
        write_ledger(
            dir.path(),
            "bot-a",
            &format!(r#"{{"sites":{{{}}}}}"#, sites.join(",")),
        );
        write_ledger(
            dir.path(),
            "bot-b",
            r#"{"sites":{"b.com":{"state":"done","account":"a"}}}"#,
        );
        let got = scan_ledger_targets(dir.path());
        assert_eq!(
            got.iter().filter(|t| t.bot_id == "bot-a").count(),
            MAX_PER_BOT
        );
        assert!(got.iter().all(|t| valid_site(&t.site)));
        assert!(got.iter().any(|t| t.bot_id == "bot-b"));
    }

    #[test]
    fn oversized_ledger_file_is_skipped() {
        let dir = tempdir().unwrap();
        let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
        write_ledger(dir.path(), "bot-a", &big);
        assert_eq!(scan_ledger_targets(dir.path()).len(), 0);
    }

    #[test]
    fn site_validation() {
        assert!(valid_site("example.com"));
        assert!(valid_site("a-b.example.co.uk"));
        assert!(!valid_site("localhost"));
        assert!(!valid_site("10.0.0.1"));
        assert!(!valid_site("example.com:8080"));
        assert!(!valid_site("Example.com"));
        assert!(!valid_site("-a.com"));
    }

    #[tokio::test]
    async fn missing_profile_means_no_cookies_and_no_request() {
        let dir = tempdir().unwrap();
        let t = LedgerTarget {
            bot_id: "b1".into(),
            site: "example.com".into(),
            account: "login/x".into(),
            evidence_url: Some("https://example.com/me".into()),
        };
        write_ledger(
            dir.path(),
            "b1",
            r#"{"sites":{"example.com":{"state":"done","account":"login/x","evidence_url":"https://example.com/me"}}}"#,
        );
        let items = probe_ledger(dir.path()).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, ItemStatus::Expired);
        assert_eq!(items[0].key, format!("{}/{}", t.bot_id, t.site));
    }
}

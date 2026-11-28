//! Site-ledger targets and browser cookie checks.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use browser_cdp::saved_sites;
use serde::Deserialize;

use crate::http_probe;
use crate::report::{ItemKind, ItemStatus, ProbeItem};

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

fn settled(state: &str) -> bool {
    !matches!(state, "candidate" | "skipped")
}

/// All bot/site pairs that carry an account name in the site ledger.
#[must_use]
pub fn scan_ledger_targets(data_dir: &Path) -> Vec<LedgerTarget> {
    let dir = data_dir.join(LEDGER_DIR);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(bot_id) = name.strip_suffix(".json") else {
            continue;
        };
        if bot_id.is_empty()
            || !bot_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(file) = serde_json::from_slice::<LedgerFile>(&bytes) else {
            continue;
        };
        for (site, e) in file.sites {
            if !settled(&e.state) {
                continue;
            }
            let Some(account) = e.account.filter(|a| !a.is_empty()) else {
                continue;
            };
            out.push(LedgerTarget {
                bot_id: bot_id.to_owned(),
                site,
                account,
                evidence_url: e.evidence_url.filter(|u| !u.is_empty()),
            });
        }
    }
    out
}

fn profile_dir(data_dir: &Path, bot_id: &str) -> PathBuf {
    data_dir.join(PROFILE_ROOT).join(bot_id)
}

fn domain_cookies(data_dir: &Path, bot_id: &str, site: &str) -> Result<bool, String> {
    let profile = profile_dir(data_dir, bot_id);
    if !profile.is_dir() {
        return Ok(false);
    }
    let domains = saved_sites::domains(&profile).map_err(|e| e.to_string())?;
    let site = site.trim_start_matches("www.");
    Ok(domains.iter().any(|d| {
        d == site || d.strip_prefix("www.") == Some(site) || site.ends_with(&format!(".{d}"))
    }))
}

/// Probe one ledger target (blocking: cookie DB + optional HTTP).
pub fn probe_ledger_target(data_dir: &Path, t: &LedgerTarget) -> ProbeItem {
    let key = format!("{}/{}", t.bot_id, t.site);
    let cookies = match domain_cookies(data_dir, &t.bot_id, &t.site) {
        Ok(true) => true,
        Ok(false) => false,
        Err(e) => {
            return ProbeItem {
                kind: ItemKind::LedgerSite,
                key,
                status: ItemStatus::Unknown,
                detail: e,
                bot_id: Some(t.bot_id.clone()),
                site: Some(t.site.clone()),
            };
        }
    };
    let url_ok = t
        .evidence_url
        .as_deref()
        .is_none_or(http_probe::evidence_still_valid);
    let status = if cookies && url_ok {
        ItemStatus::Ok
    } else {
        ItemStatus::Expired
    };
    let detail = if !cookies && !url_ok {
        "no saved cookies for this site and the evidence page looks like a sign-in".into()
    } else if !cookies {
        "no saved cookies for this site in the bot browser profile".into()
    } else {
        "the evidence page no longer looks signed in".into()
    };
    ProbeItem {
        kind: ItemKind::LedgerSite,
        key,
        status,
        detail: if status == ItemStatus::Ok {
            String::new()
        } else {
            detail
        },
        bot_id: Some(t.bot_id.clone()),
        site: Some(t.site.clone()),
    }
}

/// # Errors
/// I/O errors while writing or renaming the file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scan_picks_accounts_not_candidates() {
        let dir = tempdir().unwrap();
        let ledger = dir.path().join(LEDGER_DIR);
        std::fs::create_dir_all(&ledger).unwrap();
        let bot = ledger.join("bot-a.json");
        std::fs::write(
            &bot,
            r#"{"sites":{"example.com":{"site":"example.com","state":"done","account":"login/example.com-abc","evidence_url":"https://example.com/me"}}}"#,
        )
        .unwrap();
        let got = scan_ledger_targets(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].site, "example.com");
    }

    #[test]
    fn missing_profile_means_no_cookies() {
        let dir = tempdir().unwrap();
        let t = LedgerTarget {
            bot_id: "b1".into(),
            site: "example.com".into(),
            account: "login/x".into(),
            evidence_url: None,
        };
        let item = probe_ledger_target(dir.path(), &t);
        assert_eq!(item.status, ItemStatus::Expired);
    }
}

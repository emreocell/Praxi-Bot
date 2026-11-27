//! A small durable record for the mail waits of a bot that signs up on many
//! sites: the waits still polling, the mails already handed to a wait, and the
//! links a wait judged followable.
//!
//! Missions (and plain chats) run in sessions of a few dozen steps, and a wait
//! must outlive the session that started it: the next session finds its wait
//! here, with the clock it started on (so mail that arrived in between is still
//! found) and the page it started on (so its links are still judged against
//! the site the form was submitted on). It is one JSON file in the data
//! directory, private to the user, written in one piece (temp file and rename).
//! The records are tiny and bounded; a file that cannot be read or written only
//! costs the persistence, never the wait itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::wait::WaitFilter;

/// File name inside the data directory.
pub const WAIT_STORE_FILE: &str = "mail-waits.json";

/// A delivered-mail record is kept this long (a mail older than the longest
/// `max_age` can no longer be asked for).
const DELIVERED_KEEP_SECS: i64 = 2 * 60 * 60;
/// Followable links are remembered this long.
const LINK_KEEP_SECS: i64 = 24 * 60 * 60;
/// A finished or abandoned wait is dropped this long after its deadline.
const WAIT_GRACE_SECS: i64 = 15 * 60;
/// Most delivered-mail records kept (oldest dropped first).
const MAX_DELIVERED: usize = 500;
/// Most followable links kept per bot (oldest dropped first).
pub const MAX_LINKS_PER_BOT: usize = 200;
/// Most persisted waits.
const MAX_WAITS: usize = 64;

/// A wait that was still polling when it was last written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWait {
    /// The id the model holds.
    pub id: String,
    /// The bot it belongs to.
    pub bot: Option<String>,
    /// The mailbox account it polls.
    pub account: String,
    /// What the mail must match.
    pub filter: WaitFilter,
    /// Longest age of a mail, seconds.
    pub max_age_secs: u64,
    /// Mail dated before this (unix seconds) is not the expected one.
    pub since: Option<i64>,
    /// The origin the browser was on when the wait started.
    pub origin: Option<String>,
    /// Unix seconds when the wait started.
    pub started_at: i64,
    /// Unix seconds when it gives up.
    pub deadline: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Delivered {
    bot: Option<String>,
    account: String,
    /// `folder NUL id` of the mail.
    key: String,
    /// The ask (account, filter, age, page) it was handed to.
    by: String,
    at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredLink {
    bot: Option<String>,
    url: String,
    at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    waits: Vec<PersistedWait>,
    #[serde(default)]
    delivered: Vec<Delivered>,
    #[serde(default)]
    links: Vec<StoredLink>,
}

/// Unix seconds now.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The durable wait record. One instance per file in a process
/// ([`WaitStore::shared`]): the wait tool and the run loop both write it.
pub struct WaitStore {
    path: Option<PathBuf>,
    data: Mutex<Option<Data>>,
}

static OPEN: LazyLock<Mutex<HashMap<PathBuf, Arc<WaitStore>>>> = LazyLock::new(Mutex::default);

impl WaitStore {
    /// A store that remembers nothing across a restart (tests, no data directory).
    #[must_use]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            path: None,
            data: Mutex::new(Some(Data::default())),
        })
    }

    /// The store in `data_dir` (created on the first write). Asking again for
    /// the same directory returns the same instance.
    #[must_use]
    pub fn shared(data_dir: &Path) -> Arc<Self> {
        let path = data_dir.join(WAIT_STORE_FILE);
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(open.entry(path.clone()).or_insert_with(|| {
            Arc::new(Self {
                path: Some(path),
                data: Mutex::new(None),
            })
        }))
    }

    /// Load (once), let `f` change the data, prune what expired and write the
    /// file when `f` says it changed something.
    fn with<R>(&self, f: impl FnOnce(&mut Data, i64) -> (R, bool)) -> R {
        let mut guard = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let data = guard.get_or_insert_with(|| self.load());
        let now = now_secs();
        let before = (data.waits.len(), data.delivered.len(), data.links.len());
        let (out, changed) = f(data, now);
        data.waits.retain(|w| now <= w.deadline + WAIT_GRACE_SECS);
        data.delivered.retain(|d| now - d.at <= DELIVERED_KEEP_SECS);
        data.links.retain(|l| now - l.at <= LINK_KEEP_SECS);
        let pruned = before != (data.waits.len(), data.delivered.len(), data.links.len());
        if changed || pruned {
            self.save(data);
        }
        out
    }

    fn load(&self) -> Data {
        self.path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, data: &Data) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(data) else {
            return;
        };
        let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4().simple()));
        let written = write_private(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Remember a wait that is polling.
    pub fn put_wait(&self, wait: PersistedWait) {
        self.with(|d, _| {
            d.waits.retain(|w| w.id != wait.id);
            d.waits.push(wait);
            // Oldest first out when over the cap.
            while d.waits.len() > MAX_WAITS {
                d.waits.remove(0);
            }
            ((), true)
        });
    }

    /// Forget a wait (collected, timed out, failed or stopped).
    pub fn remove_wait(&self, id: &str) {
        self.with(|d, _| {
            let n = d.waits.len();
            d.waits.retain(|w| w.id != id);
            ((), d.waits.len() != n)
        });
    }

    /// The persisted wait `id` of this bot and account that has not given up.
    #[must_use]
    pub fn wait_by_id(&self, id: &str, bot: Option<&str>, account: &str) -> Option<PersistedWait> {
        self.with(|d, now| {
            let found = d
                .waits
                .iter()
                .find(|w| {
                    w.id == id
                        && w.bot.as_deref() == bot
                        && w.account == account
                        && now < w.deadline
                })
                .cloned();
            (found, false)
        })
    }

    /// The persisted waits of this bot and account with this filter and age
    /// that have not given up, newest first.
    #[must_use]
    pub fn waits_like(
        &self,
        bot: Option<&str>,
        account: &str,
        filter: &WaitFilter,
        max_age_secs: u64,
    ) -> Vec<PersistedWait> {
        self.with(|d, now| {
            let mut found: Vec<PersistedWait> = d
                .waits
                .iter()
                .filter(|w| {
                    w.bot.as_deref() == bot
                        && w.account == account
                        && w.filter == *filter
                        && w.max_age_secs == max_age_secs
                        && now < w.deadline
                })
                .cloned()
                .collect();
            found.sort_by_key(|w| std::cmp::Reverse(w.started_at));
            (found, false)
        })
    }

    /// Note that mail `key` of `account` was handed to the ask `by`.
    pub fn mark_delivered(&self, bot: Option<&str>, account: &str, key: &str, by: &str) {
        self.with(|d, now| {
            if d.delivered.iter().any(|x| {
                x.bot.as_deref() == bot && x.account == account && x.key == key && x.by == by
            }) {
                return ((), false);
            }
            d.delivered.push(Delivered {
                bot: bot.map(str::to_owned),
                account: account.to_owned(),
                key: key.to_owned(),
                by: by.to_owned(),
                at: now,
            });
            while d.delivered.len() > MAX_DELIVERED {
                d.delivered.remove(0);
            }
            ((), true)
        });
    }

    /// Whether mail `key` of `account` was handed to an ask other than `by`.
    /// The ask it was handed to may ask again (the model lost the answer); any
    /// other ask (another site's sign-up) is after a different mail.
    #[must_use]
    pub fn delivered_elsewhere(
        &self,
        bot: Option<&str>,
        account: &str,
        key: &str,
        by: &str,
    ) -> bool {
        self.with(|d, _| {
            let hit = d.delivered.iter().any(|x| {
                x.bot.as_deref() == bot && x.account == account && x.key == key && x.by != by
            });
            (hit, false)
        })
    }

    /// Remember followable links of `bot` (oldest forgotten beyond the cap).
    pub fn put_links(&self, bot: Option<&str>, links: &[String]) {
        if links.is_empty() {
            return;
        }
        self.with(|d, now| {
            for url in links {
                d.links
                    .retain(|l| !(l.bot.as_deref() == bot && l.url == *url));
                d.links.push(StoredLink {
                    bot: bot.map(str::to_owned),
                    url: url.clone(),
                    at: now,
                });
            }
            let mut mine = d.links.iter().filter(|l| l.bot.as_deref() == bot).count();
            while mine > MAX_LINKS_PER_BOT {
                if let Some(i) = d.links.iter().position(|l| l.bot.as_deref() == bot) {
                    d.links.remove(i);
                }
                mine -= 1;
            }
            ((), true)
        });
    }

    /// The links of `bot` that are still remembered.
    #[must_use]
    pub fn links(&self, bot: Option<&str>) -> Vec<String> {
        self.with(|d, _| {
            let found = d
                .links
                .iter()
                .filter(|l| l.bot.as_deref() == bot)
                .map(|l| l.url.clone())
                .collect();
            (found, false)
        })
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(id: &str, deadline_in: i64) -> PersistedWait {
        PersistedWait {
            id: id.into(),
            bot: Some("bot".into()),
            account: "own".into(),
            filter: WaitFilter::default(),
            max_age_secs: 900,
            since: Some(now_secs() - 180),
            origin: Some("https://a.test:443".into()),
            started_at: now_secs(),
            deadline: now_secs() + deadline_in,
        }
    }

    #[test]
    fn waits_survive_a_reopen_and_expired_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = WaitStore::shared(dir.path());
        store.put_wait(wait("mw-live", 300));
        store.put_wait(wait("mw-old", -WAIT_GRACE_SECS - 5));
        // A second process (a fresh instance over the same file) sees the live one only.
        let again = WaitStore {
            path: Some(dir.path().join(WAIT_STORE_FILE)),
            data: Mutex::new(None),
        };
        assert!(again.wait_by_id("mw-live", Some("bot"), "own").is_some());
        assert!(again.wait_by_id("mw-old", Some("bot"), "own").is_none());
        // Another bot or account never sees it.
        assert!(again.wait_by_id("mw-live", Some("other"), "own").is_none());
        assert!(
            again
                .wait_by_id("mw-live", Some("bot"), "elsewhere")
                .is_none()
        );
        again.remove_wait("mw-live");
        assert!(again.wait_by_id("mw-live", Some("bot"), "own").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_a_corrupt_file_is_only_a_lost_cache() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = WaitStore::shared(dir.path());
        store.put_links(Some("bot"), &["https://a.test/v?t=1".into()]);
        let file = dir.path().join(WAIT_STORE_FILE);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::write(&file, b"{ not json").unwrap();
        let fresh = WaitStore {
            path: Some(file),
            data: Mutex::new(None),
        };
        assert_eq!(fresh.links(Some("bot")), Vec::<String>::new());
        fresh.put_links(Some("bot"), &["https://b.test/v".into()]);
        assert_eq!(fresh.links(Some("bot")), ["https://b.test/v"]);
    }

    #[test]
    fn links_are_per_bot_and_capped() {
        let store = WaitStore::in_memory();
        let many: Vec<String> = (0..MAX_LINKS_PER_BOT + 20)
            .map(|n| format!("https://s{n}.test/v"))
            .collect();
        store.put_links(Some("a"), &many);
        store.put_links(Some("b"), &["https://b.test/v".into()]);
        let a = store.links(Some("a"));
        assert_eq!(a.len(), MAX_LINKS_PER_BOT);
        assert!(
            !a.contains(&"https://s0.test/v".to_owned()),
            "the oldest went first"
        );
        assert!(a.contains(&format!("https://s{}.test/v", MAX_LINKS_PER_BOT + 19)));
        assert_eq!(store.links(Some("b")), ["https://b.test/v"]);
        assert_eq!(store.links(None), Vec::<String>::new());
    }

    #[test]
    fn a_delivered_mail_is_closed_to_other_waits_only() {
        let store = WaitStore::in_memory();
        store.mark_delivered(Some("bot"), "own", "INBOX\u{0}m1", "mw-a");
        assert!(!store.delivered_elsewhere(Some("bot"), "own", "INBOX\u{0}m1", "mw-a"));
        assert!(store.delivered_elsewhere(Some("bot"), "own", "INBOX\u{0}m1", "mw-b"));
        assert!(!store.delivered_elsewhere(Some("bot"), "own", "INBOX\u{0}m2", "mw-b"));
        assert!(!store.delivered_elsewhere(Some("other"), "own", "INBOX\u{0}m1", "mw-b"));
    }
}

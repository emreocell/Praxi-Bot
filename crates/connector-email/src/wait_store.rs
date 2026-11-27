//! A small durable record for the mail waits of a bot that signs up on many
//! sites: the waits still polling, the mails already handed to a wait, and the
//! links a wait judged followable.
//!
//! Missions (and plain chats) run in sessions of a few dozen steps, and a wait
//! must outlive the session that started it: the next session finds its wait
//! here, with the clock it started on (so mail that arrived in between is still
//! found) and the page it started on (so its links are still judged against
//! the site the form was submitted on). It is one JSON file in the data
//! directory, private to the user, written in one piece (temp file and rename;
//! on Windows see [`write_private`]). The records are tiny and bounded: a file
//! larger than [`MAX_FILE_BYTES`] is never read, and a record with an over-long
//! field is dropped on load. A file that cannot be read or written only costs
//! the persistence, never the wait itself.

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
/// Largest `mail-waits.json` that is read (about 1 000 records of the longest
/// allowed fields; a real file is a few tens of kilobytes). A bigger one is
/// planted or corrupt: it is ignored, never loaded into memory.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Longest wait id, bot id, filter string (bytes).
const MAX_ID_LEN: usize = 128;
const MAX_FILTER_LEN: usize = 256;
/// Longest account name, origin, delivered-mail key (bytes).
const MAX_NAME_LEN: usize = 512;
/// Longest ask fingerprint (account, filter, age and origin together).
const MAX_ASK_LEN: usize = 2048;
/// Longest stored link (the verifier refuses longer ones anyway).
const MAX_LINK_LEN: usize = crate::verify::MAX_URL_CHARS;
/// Longest registrable domain.
const MAX_DOMAIN_LEN: usize = 253;
/// Prefix of the ids this store hands out for waits.
const WAIT_ID_PREFIX: &str = "mw-";
/// Longest a wait can live, seconds ([`crate::wait::MAX_WAIT`]).
const MAX_WAIT_SECS: i64 = crate::wait::MAX_WAIT.as_secs().cast_signed();

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
    /// The site (registrable domain) of the wait that judged the link followable;
    /// `None`: it was followable only through the owner's domain list. A file
    /// written before this field existed reads as `None`.
    #[serde(default)]
    site: Option<String>,
    at: i64,
}

/// A followable link a wait remembered, with the site that allowed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RememberedLink {
    /// The link, exactly as `email_wait_for` returned it.
    pub url: String,
    /// The registrable domain of the page the wait started on (`None`: only the
    /// owner's domain list allowed it).
    pub site: Option<String>,
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

impl Data {
    /// Drop what a legitimate writer never produces (over-long fields, a wait
    /// that outlives the longest wait) and cut every list to its cap, so what
    /// was read from disk is as bounded as what this process would have written.
    fn sanitize(&mut self, now: i64) {
        let short = |s: &Option<String>, max: usize| s.as_ref().is_none_or(|v| v.len() <= max);
        self.waits.retain(|w| {
            w.id.len() <= MAX_ID_LEN
                && short(&w.bot, MAX_ID_LEN)
                && w.account.len() <= MAX_NAME_LEN
                && short(&w.origin, MAX_NAME_LEN)
                && short(&w.filter.from_domain, MAX_FILTER_LEN)
                && short(&w.filter.subject_contains, MAX_FILTER_LEN)
                && short(&w.filter.keyword, MAX_FILTER_LEN)
                && w.deadline <= now + MAX_WAIT_SECS + WAIT_GRACE_SECS
        });
        self.delivered.retain(|d| {
            short(&d.bot, MAX_ID_LEN)
                && d.account.len() <= MAX_NAME_LEN
                && d.key.len() <= MAX_NAME_LEN
                && d.by.len() <= MAX_ASK_LEN
        });
        self.links.retain(|l| {
            short(&l.bot, MAX_ID_LEN)
                && l.url.len() <= MAX_LINK_LEN
                && short(&l.site, MAX_DOMAIN_LEN)
        });
        // The newest are kept (the lists are oldest first).
        self.waits
            .drain(..self.waits.len().saturating_sub(MAX_WAITS));
        self.delivered
            .drain(..self.delivered.len().saturating_sub(MAX_DELIVERED));
        let mut per_bot: HashMap<Option<String>, usize> = HashMap::new();
        for l in self.links.iter().rev() {
            *per_bot.entry(l.bot.clone()).or_default() += 1;
        }
        self.links.retain(|l| {
            let n = per_bot.entry(l.bot.clone()).or_default();
            let keep = *n <= MAX_LINKS_PER_BOT;
            *n = n.saturating_sub(1);
            keep
        });
    }
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
        let mut data: Data = self
            .path
            .as_deref()
            .and_then(read_bounded)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        data.sanitize(now_secs());
        data
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

    /// A fresh wait id: unguessable, in the one format [`WaitStore::is_wait_id`] accepts.
    #[must_use]
    pub fn new_wait_id() -> String {
        format!("{WAIT_ID_PREFIX}{}", uuid::Uuid::new_v4().simple())
    }

    /// Whether `id` is shaped like an id this store hands out. A shape check
    /// only (a made-up id of that shape passes it): use it to tell a call that
    /// can be about a wait from one that cannot, never as proof the wait exists.
    #[must_use]
    pub fn is_wait_id(id: &str) -> bool {
        id.strip_prefix(WAIT_ID_PREFIX)
            .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    }

    /// Remember a wait that is polling.
    pub fn put_wait(&self, wait: PersistedWait) {
        let mut one = Data {
            waits: vec![wait],
            ..Data::default()
        };
        one.sanitize(now_secs());
        // A wait the next load would drop anyway is not written (it still polls here).
        let Some(wait) = one.waits.pop() else {
            return;
        };
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
    /// that have not given up, newest first, and that an ask starting now with
    /// `since` may carry on: the same kind of start watermark (a fixed
    /// look-back from the call, or none because the caller named an age) and a
    /// watermark no later than the ask's own, so taking a wait over never hides
    /// mail the new ask wants, and never reaches back for mail its kind of ask
    /// would not (an ask for older mail on purpose does not inherit the
    /// default look-back's wait, nor the other way round).
    #[must_use]
    pub fn waits_like(
        &self,
        bot: Option<&str>,
        account: &str,
        filter: &WaitFilter,
        max_age_secs: u64,
        since: Option<i64>,
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
                        && match (w.since, since) {
                            (None, None) => true,
                            (Some(had), Some(asks)) => had <= asks,
                            _ => false,
                        }
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
        if account.len() > MAX_NAME_LEN || key.len() > MAX_NAME_LEN || by.len() > MAX_ASK_LEN {
            return;
        }
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

    /// Remember followable links of `bot` that a wait judged against `site` (the
    /// registrable domain of the page it started on; `None`: only the owner's
    /// domain list allowed them). Oldest forgotten beyond the cap.
    pub fn put_links(&self, bot: Option<&str>, site: Option<&str>, links: &[String]) {
        let links: Vec<&String> = links.iter().filter(|u| u.len() <= MAX_LINK_LEN).collect();
        if links.is_empty() || site.is_some_and(|s| s.len() > MAX_DOMAIN_LEN) {
            return;
        }
        self.with(|d, now| {
            for url in links {
                d.links
                    .retain(|l| !(l.bot.as_deref() == bot && l.url == *url));
                d.links.push(StoredLink {
                    bot: bot.map(str::to_owned),
                    url: url.clone(),
                    site: site.map(str::to_owned),
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

    /// The links of `bot` that are still remembered, with the site each was
    /// judged against. A remembered link is a lead, not a permission: whoever
    /// follows one checks it again against the page the run is on now.
    #[must_use]
    pub fn links(&self, bot: Option<&str>) -> Vec<RememberedLink> {
        self.with(|d, _| {
            let found = d
                .links
                .iter()
                .filter(|l| l.bot.as_deref() == bot)
                .map(|l| RememberedLink {
                    url: l.url.clone(),
                    site: l.site.clone(),
                })
                .collect();
            (found, false)
        })
    }
}

/// The file's bytes, unless it is missing, unreadable, not a regular file or
/// larger than [`MAX_FILE_BYTES`]. The size is checked on the open handle and
/// the read is capped too, so a file that grows meanwhile is still bounded.
fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
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

/// Windows: the same create-a-new-temp-file-then-rename path as unix (`create_new`
/// never writes through an existing file or link of that name, and
/// [`WaitStore::save`] removes the temp file when anything fails). Unix sets mode
/// 0600; here no ACL is set, so the file has the access of the data directory,
/// which for a per-user data directory (`%LOCALAPPDATA%`, `%APPDATA%`) is the
/// user, SYSTEM and administrators only. The file holds no secret (wait ids,
/// filters, one-time verification links of this user's own mailbox).
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(url: &str, site: Option<&str>) -> RememberedLink {
        RememberedLink {
            url: url.into(),
            site: site.map(str::to_owned),
        }
    }

    /// A store over `file` that has not read it yet (a second process).
    fn reopened(file: PathBuf) -> WaitStore {
        WaitStore {
            path: Some(file),
            data: Mutex::new(None),
        }
    }

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
        store.put_links(
            Some("bot"),
            Some("a.test"),
            &["https://a.test/v?t=1".into()],
        );
        let file = dir.path().join(WAIT_STORE_FILE);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::write(&file, b"{ not json").unwrap();
        let fresh = WaitStore {
            path: Some(file),
            data: Mutex::new(None),
        };
        assert_eq!(fresh.links(Some("bot")), Vec::new());
        fresh.put_links(Some("bot"), Some("b.test"), &["https://b.test/v".into()]);
        assert_eq!(
            fresh.links(Some("bot")),
            [link("https://b.test/v", Some("b.test"))]
        );
    }

    #[test]
    fn links_are_per_bot_and_capped() {
        let store = WaitStore::in_memory();
        let many: Vec<String> = (0..MAX_LINKS_PER_BOT + 20)
            .map(|n| format!("https://s{n}.test/v"))
            .collect();
        store.put_links(Some("a"), None, &many);
        store.put_links(Some("b"), Some("b.test"), &["https://b.test/v".into()]);
        let a = store.links(Some("a"));
        assert_eq!(a.len(), MAX_LINKS_PER_BOT);
        assert!(
            !a.contains(&link("https://s0.test/v", None)),
            "the oldest went first"
        );
        assert!(a.contains(&link(
            &format!("https://s{}.test/v", MAX_LINKS_PER_BOT + 19),
            None
        )));
        assert_eq!(
            store.links(Some("b")),
            [link("https://b.test/v", Some("b.test"))]
        );
        assert_eq!(store.links(None), Vec::new());
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
    #[test]
    fn a_file_over_the_size_cap_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(WAIT_STORE_FILE);
        let wait_json = serde_json::to_value(wait("mw-live", 300)).unwrap();
        // Valid JSON with a live wait in it, padded one byte past the cap.
        let body = |pad: usize| {
            serde_json::to_vec(&serde_json::json!({
                "waits": [wait_json],
                "pad": "x".repeat(pad),
            }))
            .unwrap()
        };
        let base = body(0).len();
        let at_cap = usize::try_from(MAX_FILE_BYTES).unwrap();
        std::fs::write(&file, body(at_cap - base)).unwrap();
        assert!(
            reopened(file.clone())
                .wait_by_id("mw-live", Some("bot"), "own")
                .is_some(),
            "a file of exactly the cap is read"
        );
        std::fs::write(&file, body(at_cap - base + 1)).unwrap();
        let big = reopened(file.clone());
        assert!(big.wait_by_id("mw-live", Some("bot"), "own").is_none());
        // The store carries on from empty and the next write replaces the file.
        big.put_links(Some("bot"), None, &["https://b.test/v".into()]);
        assert!(std::fs::metadata(&file).unwrap().len() < 4096);
        // Not a regular file: never opened for reading either.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir2.path().join(WAIT_STORE_FILE)).unwrap();
        assert_eq!(
            reopened(dir2.path().join(WAIT_STORE_FILE)).links(Some("bot")),
            Vec::new()
        );
    }

    #[test]
    fn a_planted_file_loses_over_long_fields_and_extra_records() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(WAIT_STORE_FILE);
        let now = now_secs();
        let long = |n: usize| "a".repeat(n);
        let mut long_id = wait(&long(MAX_ID_LEN + 1), 300);
        let mut long_account = wait("mw-acc", 300);
        long_account.account = long(MAX_NAME_LEN + 1);
        let mut long_filter = wait("mw-filter", 300);
        long_filter.filter.keyword = Some(long(MAX_FILTER_LEN + 1));
        let mut long_origin = wait("mw-origin", 300);
        long_origin.origin = Some(long(MAX_NAME_LEN + 1));
        let forever = wait("mw-forever", 10 * 24 * 3600);
        let kept_wait = wait("mw-fine", 300);
        long_id.bot = Some("bot".into());
        let waits = [
            long_id,
            long_account,
            long_filter,
            long_origin,
            forever,
            kept_wait,
        ];
        let delivered = |key: String, by: String| serde_json::json!({"bot": "bot", "account": "own", "key": key, "by": by, "at": now});
        let mut delivered_all = vec![
            delivered(long(MAX_NAME_LEN + 1), "x".into()),
            delivered("k".into(), long(MAX_ASK_LEN + 1)),
        ];
        delivered_all
            .extend((0..MAX_DELIVERED + 30).map(|n| delivered(format!("k{n}"), "x".into())));
        let links: Vec<_> = [
            serde_json::json!({"bot": "bot", "url": format!("https://a.test/{}", long(MAX_LINK_LEN)), "at": now}),
            serde_json::json!({"bot": "bot", "url": "https://a.test/v", "site": long(MAX_DOMAIN_LEN + 1), "at": now}),
            serde_json::json!({"bot": "bot", "url": "https://a.test/ok", "at": now}),
        ]
        .into_iter()
        .chain((0..MAX_LINKS_PER_BOT + 10).map(|n| {
            serde_json::json!({"bot": "other", "url": format!("https://s{n}.test/v"), "site": "s.test", "at": now})
        }))
        .collect();
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({
                "waits": waits, "delivered": delivered_all, "links": links
            }))
            .unwrap(),
        )
        .unwrap();
        let store = reopened(file);
        for gone in [
            &long(MAX_ID_LEN + 1)[..],
            "mw-acc",
            "mw-filter",
            "mw-origin",
            "mw-forever",
        ] {
            assert!(
                store.wait_by_id(gone, Some("bot"), "own").is_none(),
                "{gone} should have been dropped"
            );
        }
        assert!(store.wait_by_id("mw-fine", Some("bot"), "own").is_some());
        // The two bad records are gone and the rest is cut to the newest cap.
        let kept = store.with(|d, _| {
            (
                (d.delivered.len(), d.delivered.last().map(|x| x.key.clone())),
                false,
            )
        });
        assert_eq!(kept.0, MAX_DELIVERED);
        assert_eq!(kept.1, Some(format!("k{}", MAX_DELIVERED + 29)));
        assert_eq!(store.links(Some("bot")), [link("https://a.test/ok", None)]);
        let other = store.links(Some("other"));
        assert_eq!(other.len(), MAX_LINKS_PER_BOT);
        assert_eq!(other[0].url, "https://s10.test/v", "the oldest ten went");
    }

    #[test]
    fn a_stored_link_remembers_its_site_and_an_old_file_reads_as_no_site() {
        let dir = tempfile::tempdir().unwrap();
        let store = WaitStore::shared(dir.path());
        store.put_links(Some("bot"), Some("a.test"), &["https://a.test/v".into()]);
        store.put_links(Some("bot"), None, &["https://listed.test/v".into()]);
        let again = reopened(dir.path().join(WAIT_STORE_FILE));
        assert_eq!(
            again.links(Some("bot")),
            [
                link("https://a.test/v", Some("a.test")),
                link("https://listed.test/v", None)
            ]
        );
        // A file written before links had a site.
        let old = dir.path().join("old");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(
            old.join(WAIT_STORE_FILE),
            serde_json::to_vec(&serde_json::json!({
                "links": [{"bot": "bot", "url": "https://a.test/v", "at": now_secs()}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reopened(old.join(WAIT_STORE_FILE)).links(Some("bot")),
            [link("https://a.test/v", None)]
        );
    }

    #[test]
    fn wait_ids_have_one_shape_and_only_the_stores_own_pass() {
        let id = WaitStore::new_wait_id();
        assert!(WaitStore::is_wait_id(&id), "{id}");
        assert_ne!(id, WaitStore::new_wait_id());
        for bad in [
            "",
            "x",
            "mw-",
            "mw-abc",
            "mw-live",
            &format!("{id}0"),
            &id[..id.len() - 1],
            &format!("xx-{}", &id[3..]),
            &format!("mw-{}", "g".repeat(32)),
            &format!(" {id}"),
        ] {
            assert!(!WaitStore::is_wait_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_new_ask_takes_over_only_a_wait_with_the_same_kind_of_start() {
        let store = WaitStore::in_memory();
        let now = now_secs();
        let mut by_default = wait("mw-default", 300);
        by_default.since = Some(now - 200);
        let mut by_age = wait("mw-age", 300);
        by_age.since = None;
        store.put_wait(by_default);
        store.put_wait(by_age);
        let like = |since: Option<i64>| -> Vec<String> {
            store
                .waits_like(Some("bot"), "own", &WaitFilter::default(), 900, since)
                .into_iter()
                .map(|w| w.id)
                .collect()
        };
        // The default look-back, a little later: the wait it started with. Never the
        // "older mail on purpose" wait (no watermark).
        assert_eq!(like(Some(now - 100)), ["mw-default"]);
        // The caller named an age: only the wait that had no watermark.
        assert_eq!(like(None), ["mw-age"]);
        // An ask whose watermark is EARLIER than the wait's wants mail the wait skips.
        assert_eq!(like(Some(now - 400)), Vec::<String>::new());
    }
}

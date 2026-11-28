//! Clock-driven event coalescing. The daemon provides permission-gated snapshots.
use crate::{FileEvent, Trigger};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

/// File metadata polling interval; never read file contents.
pub const FILE_POLL_MS: i64 = 30_000;
/// Header-only mailbox polling interval.
pub const EMAIL_POLL_MS: i64 = 120_000;
/// Bound snapshot size and queued events per schedule.
pub const MAX_ENTRIES: usize = 1_000;
/// Feed item identities remembered (the newest observations replace the rest beyond this).
const FEED_MEMORY: usize = 2_000;

/// Polling cadence of a source when the owner's rule names none.
#[must_use]
pub const fn default_poll_ms(trigger: &Trigger) -> i64 {
    match trigger {
        Trigger::Email { .. } => EMAIL_POLL_MS,
        Trigger::Feed { .. } => 1_800_000,
        Trigger::PageChange { .. } => 3_600_000,
        Trigger::Calendar { .. } => 120_000,
        Trigger::HomeAssistant { .. } => 60_000,
        Trigger::FileWatch { .. } | Trigger::Webhook { .. } => FILE_POLL_MS,
    }
}

/// Sources whose set of seen identities only grows (an old mail or feed item that
/// drops out of a window must not look new when it returns).
const fn remembers(trigger: &Trigger) -> bool {
    matches!(trigger, Trigger::Email { .. } | Trigger::Feed { .. })
}

/// One bounded source observation, supplied by a real connector or a fake.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Stable filename or mailbox message identity.
    pub key: String,
    /// Metadata fingerprint; no file or message body.
    pub fingerprint: String,
    /// Filename, or message id/from/subject only.
    pub input: Value,
}

/// Durable state for an enabled event trigger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchState {
    baseline: Option<BTreeMap<String, String>>,
    #[serde(default)]
    email_uid_high: Option<u64>,
    pending: BTreeMap<String, Value>,
    pending_since: Option<i64>,
    fired: VecDeque<i64>,
    /// Polling cadence follows the scheduler's injectable clock.
    pub next_poll_ms: i64,
}

impl WatchState {
    /// Update the snapshot. The first poll establishes the baseline.
    ///
    /// # Errors
    /// Source exceeded the bounded snapshot/queue/output size.
    pub fn observe(
        &mut self,
        trigger: &Trigger,
        now: i64,
        entries: Vec<Observation>,
    ) -> Result<(), &'static str> {
        if entries.len() > MAX_ENTRIES {
            return Err("watch snapshot limit exceeded");
        }
        let mut current = if remembers(trigger) {
            self.baseline.clone().unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        let mut pending = self.pending.clone();
        let mut uid_high = self.email_uid_high;
        let entries_keys: Vec<(String, String)> = if matches!(trigger, Trigger::Feed { .. }) {
            entries
                .iter()
                .map(|e| (e.key.clone(), e.fingerprint.clone()))
                .collect()
        } else {
            Vec::new()
        };
        for entry in entries {
            if entry.key.len() > 4096 || entry.fingerprint.len() > 256 {
                return Err("invalid source observation");
            }
            let uid = if matches!(trigger, Trigger::Email { .. }) {
                entry
                    .key
                    .strip_prefix("uid:")
                    .and_then(|v| v.parse::<u64>().ok())
            } else {
                None
            };
            if let Some(uid) = uid {
                uid_high = Some(uid_high.map_or(uid, |high| high.max(uid)));
            }
            if let Some(previous) = &self.baseline {
                let created = uid
                    .and_then(|u| self.email_uid_high.map(|high| u > high))
                    .unwrap_or_else(|| !previous.contains_key(&entry.key));
                let modified = previous
                    .get(&entry.key)
                    .is_some_and(|v| v != &entry.fingerprint);
                let selected = match trigger {
                    Trigger::FileWatch { events, .. } => {
                        (created && events.contains(&FileEvent::Created))
                            || (modified && events.contains(&FileEvent::Modified))
                    }
                    Trigger::Email { .. } | Trigger::Feed { .. } | Trigger::Calendar { .. } => {
                        created
                    }
                    Trigger::PageChange { .. } => modified,
                    Trigger::HomeAssistant { to_state, .. } => {
                        modified
                            && to_state.as_ref().is_none_or(|want| {
                                want.is_empty() || want.eq_ignore_ascii_case(&entry.fingerprint)
                            })
                    }
                    Trigger::Webhook { .. } => false,
                };
                if selected {
                    let mut input = entry.input;
                    if let Value::Object(fields) = &mut input {
                        if !matches!(trigger, Trigger::Calendar { .. }) {
                            fields.insert("received_at".into(), Value::from(now));
                        }
                        if matches!(trigger, Trigger::HomeAssistant { .. })
                            && let Some(before) = previous.get(&entry.key)
                        {
                            fields.insert("previous_state".into(), Value::from(before.clone()));
                        }
                    }
                    pending.insert(entry.key.clone(), input);
                }
            }
            if uid.is_none() {
                current.insert(entry.key, entry.fingerprint);
            }
        }
        if matches!(trigger, Trigger::Feed { .. }) && current.len() > FEED_MEMORY {
            // A long-running feed: keep what the last poll showed, forget the older items.
            current = entries_keys.into_iter().collect();
        }
        if current.len() > 10_000
            || pending.len() > MAX_ENTRIES
            || serde_json::to_vec(&pending).map_or(true, |v| v.len() > crate::precheck::MAX_BYTES)
        {
            return Err("watch queue limit exceeded");
        }
        if !pending.is_empty() && self.pending.is_empty() {
            self.pending_since = Some(now);
        }
        self.pending = pending;
        self.baseline = Some(current);
        self.email_uid_high = uid_high;
        self.next_poll_ms = now.saturating_add(default_poll_ms(trigger));
        Ok(())
    }

    /// Take one coalesced batch after debounce, at most six per rolling minute.
    /// Excess stays queued, including while the schedule's previous run is busy.
    pub fn take_ready(&mut self, trigger: &Trigger, now: i64) -> Option<String> {
        let since = self.pending_since?;
        let debounce = match trigger {
            Trigger::FileWatch { debounce_ms, .. } => i64::try_from(*debounce_ms).unwrap_or(60_000),
            _ => 0,
        };
        if now.saturating_sub(since) < debounce {
            return None;
        }
        self.fired.retain(|t| now.saturating_sub(*t) < 60_000);
        if self.fired.len() >= 6 {
            return None;
        }
        let values: Vec<_> = self.pending.values().collect();
        let input = serde_json::to_string(&values).ok()?;
        self.pending.clear();
        self.pending_since = None;
        self.fired.push_back(now);
        Some(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trigger() -> Trigger {
        Trigger::FileWatch {
            path: "workspace".into(),
            glob: "*.txt".into(),
            events: vec![FileEvent::Created, FileEvent::Modified],
            debounce_ms: 1000,
        }
    }
    fn entries(n: usize) -> Vec<Observation> {
        (0..n)
            .map(|i| Observation {
                key: format!("file{i}.txt"),
                fingerprint: "1".into(),
                input: Value::String(format!("file{i}.txt")),
            })
            .collect()
    }
    #[test]
    fn five_files_coalesce_debounce_and_persist_without_content() {
        let mut state = WatchState::default();
        let trigger = trigger();
        state.observe(&trigger, 0, vec![]).unwrap();
        state.observe(&trigger, 10, entries(5)).unwrap();
        assert!(state.take_ready(&trigger, 1009).is_none());
        let bytes = serde_json::to_string(&state).unwrap();
        let mut state: WatchState = serde_json::from_str(&bytes).unwrap();
        let payload = state.take_ready(&trigger, 1010).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&payload).unwrap().len(),
            5
        );
        assert!(state.take_ready(&trigger, 1011).is_none());
    }
    #[test]
    fn sixth_fire_caps_rate_and_excess_coalesces_until_next_window() {
        let mut state = WatchState::default();
        let trigger = Trigger::Email {
            account: "fake".into(),
            from_filter: "safe@example.test".into(),
            subject_filter: None,
        };
        state.observe(&trigger, 0, vec![]).unwrap();
        for n in 1..=6 {
            state
                .observe(&trigger, i64::try_from(n).unwrap(), entries(n))
                .unwrap();
            assert!(
                state
                    .take_ready(&trigger, i64::try_from(n).unwrap())
                    .is_some()
            );
        }
        state.observe(&trigger, 7, entries(7)).unwrap();
        assert!(state.take_ready(&trigger, 7).is_none());
        state.observe(&trigger, 8, entries(8)).unwrap();
        let batch = state.take_ready(&trigger, 60_001).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&batch).unwrap().len(),
            2
        );
    }

    fn obs(key: &str, fingerprint: &str, input: Value) -> Observation {
        Observation {
            key: key.into(),
            fingerprint: fingerprint.into(),
            input,
        }
    }
    fn batch(payload: &str) -> Vec<Value> {
        serde_json::from_str(payload).unwrap()
    }

    #[test]
    fn feed_first_poll_is_a_baseline_then_only_new_items_fire_once() {
        let trigger = Trigger::Feed {
            url: "https://example.test/feed.xml".into(),
        };
        let item = |id: &str| obs(id, "x", serde_json::json!({"title": id}));
        let mut state = WatchState::default();
        state
            .observe(&trigger, 0, vec![item("a"), item("b")])
            .unwrap();
        assert!(
            state.take_ready(&trigger, 1).is_none(),
            "old items never start a run"
        );
        state
            .observe(&trigger, 10, vec![item("c"), item("a"), item("b")])
            .unwrap();
        let fired = batch(&state.take_ready(&trigger, 10).unwrap());
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0]["title"], "c");
        // The same items again, or an item that fell out of the feed and came back, are not new.
        state
            .observe(&trigger, 20, vec![item("c"), item("a")])
            .unwrap();
        state
            .observe(&trigger, 30, vec![item("c"), item("b"), item("a")])
            .unwrap();
        assert!(state.take_ready(&trigger, 30).is_none());
    }

    #[test]
    fn feed_memory_is_bounded_for_a_long_running_feed() {
        let trigger = Trigger::Feed {
            url: "https://example.test/feed.xml".into(),
        };
        let mut state = WatchState::default();
        state.observe(&trigger, 0, vec![]).unwrap();
        for round in 0..6 {
            let entries = (0..500)
                .map(|i| obs(&format!("r{round}-{i}"), "x", Value::Null))
                .collect();
            state.observe(&trigger, round + 1, entries).unwrap();
            let _ = state.take_ready(&trigger, 100_000 * (round + 1));
        }
        assert!(serde_json::to_vec(&state).unwrap().len() < 200_000);
    }

    #[test]
    fn page_change_fires_on_a_different_fingerprint_only() {
        let trigger = Trigger::PageChange {
            url: "https://example.test/p".into(),
            selector: "#price".into(),
        };
        let page = |text: &str, fp: &str| vec![obs("page", fp, serde_json::json!({"text": text}))];
        let mut state = WatchState::default();
        state.observe(&trigger, 0, page("$10", "h1")).unwrap();
        state.observe(&trigger, 5, page("$10", "h1")).unwrap();
        assert!(state.take_ready(&trigger, 5).is_none());
        state.observe(&trigger, 10, page("$9", "h2")).unwrap();
        let fired = batch(&state.take_ready(&trigger, 10).unwrap());
        assert_eq!(fired[0]["text"], "$9");
        state.observe(&trigger, 20, page("$9", "h2")).unwrap();
        assert!(state.take_ready(&trigger, 20).is_none());
        assert_eq!(state.next_poll_ms, 20 + 3_600_000);
    }

    #[test]
    fn calendar_event_fires_when_it_enters_the_window_not_when_already_inside() {
        let trigger = Trigger::Calendar {
            account: None,
            minutes_before: 15,
        };
        let event = |uid: &str, start: &str| {
            obs(
                &format!("{uid}@{start}"),
                "e",
                serde_json::json!({"summary": uid, "start": start}),
            )
        };
        let mut state = WatchState::default();
        // Already inside the window when the rule starts: part of the baseline.
        state
            .observe(&trigger, 0, vec![event("early", "T1")])
            .unwrap();
        state
            .observe(
                &trigger,
                120_000,
                vec![event("early", "T1"), event("late", "T2")],
            )
            .unwrap();
        let fired = batch(&state.take_ready(&trigger, 120_000).unwrap());
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0]["summary"], "late");
        assert!(fired[0].get("received_at").is_none());
        // A moved event is a different start and fires again.
        state
            .observe(&trigger, 240_000, vec![event("late", "T3")])
            .unwrap();
        assert_eq!(
            batch(&state.take_ready(&trigger, 240_000).unwrap())[0]["start"],
            "T3"
        );
    }

    #[test]
    fn home_assistant_reports_the_previous_state_and_honours_to_state() {
        let any = Trigger::HomeAssistant {
            entity_id: "binary_sensor.door".into(),
            to_state: None,
        };
        let on = Trigger::HomeAssistant {
            entity_id: "binary_sensor.door".into(),
            to_state: Some("ON".into()),
        };
        let door = |state: &str| {
            vec![obs(
                "binary_sensor.door",
                state,
                serde_json::json!({"state": state}),
            )]
        };
        let (mut a, mut b) = (WatchState::default(), WatchState::default());
        a.observe(&any, 0, door("off")).unwrap();
        b.observe(&on, 0, door("off")).unwrap();
        a.observe(&any, 60_000, door("on")).unwrap();
        b.observe(&on, 60_000, door("on")).unwrap();
        let fired = batch(&a.take_ready(&any, 60_000).unwrap());
        assert_eq!(fired[0]["previous_state"], "off");
        assert_eq!(fired[0]["state"], "on");
        assert_eq!(batch(&b.take_ready(&on, 60_000).unwrap()).len(), 1);
        // Back to off: the unfiltered rule fires, the "on only" rule does not.
        a.observe(&any, 120_000, door("off")).unwrap();
        b.observe(&on, 120_000, door("off")).unwrap();
        assert!(a.take_ready(&any, 120_000).is_some());
        assert!(b.take_ready(&on, 120_000).is_none());
    }

    #[test]
    fn every_polled_source_stays_under_six_runs_a_minute() {
        let trigger = Trigger::HomeAssistant {
            entity_id: "sensor.x".into(),
            to_state: None,
        };
        let mut state = WatchState::default();
        state
            .observe(&trigger, 0, vec![obs("sensor.x", "0", Value::Null)])
            .unwrap();
        let mut fired = 0;
        for n in 1..=10 {
            state
                .observe(
                    &trigger,
                    n,
                    vec![obs("sensor.x", &n.to_string(), Value::Null)],
                )
                .unwrap();
            fired += i32::from(state.take_ready(&trigger, n).is_some());
        }
        assert_eq!(fired, 6);
    }
}

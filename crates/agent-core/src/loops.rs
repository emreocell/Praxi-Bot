//! Detection of short call cycles that make no progress.
//!
//! `Guards::repeat_stop` only sees the SAME call made over and over. A model
//! that is stuck usually alternates: click, snapshot, click, snapshot, with
//! the page unchanged each time. A cycle here is a block of two or three
//! distinct calls that repeats back to back while every repetition gets the
//! same results. The loop warns the model first (a hint appended to the
//! result) and stops the turn only if the cycle goes on.

use std::hash::{Hash, Hasher};

use serde_json::Value;

/// Starts the loop-authored hint appended to a tool result.
pub(crate) const CYCLE_HINT_MARK: &str = "\n[loop-hint:";
/// Starts the older "same call again" warning appended to a tool result.
pub(crate) const REPEAT_WARN_MARK: &str = "\n[warning: this exact call";
/// Longest cycle (in calls) that is looked for.
const MAX_PERIOD: usize = 3;
/// Tools that poll on purpose: the same results while waiting are not a loop.
const WAITING_TOOLS: [&str; 2] = ["wait", "email_wait_for"];

/// One call of the run, oldest first, as far as cycle detection cares.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Tool name and parsed arguments.
    key: (String, Value),
    /// Hash of the result without loop-authored notes; `None`: not run yet.
    result: Option<u64>,
}

impl Entry {
    pub(crate) fn new(key: (String, Value), result_text: Option<&str>) -> Self {
        Self {
            key,
            result: result_text.map(fingerprint),
        }
    }

    /// Same call, and the same result unless one of the two is still unknown.
    fn matches(&self, other: &Self) -> bool {
        self.key == other.key
            && (self.result.is_none() || other.result.is_none() || self.result == other.result)
    }

    fn waits(&self) -> bool {
        WAITING_TOOLS.contains(&self.key.0.as_str())
    }
}

/// Hash of a result text with the notes the loop appended itself removed, so
/// a hint on one repetition does not make it differ from the next.
pub(crate) fn fingerprint(text: &str) -> u64 {
    let end = [CYCLE_HINT_MARK, REPEAT_WARN_MARK]
        .iter()
        .filter_map(|m| text.find(m))
        .min()
        .unwrap_or(text.len());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text[..end].hash(&mut h);
    h.finish()
}

/// The trailing cycle: how many times its block of 2 or 3 distinct calls has
/// repeated with unchanged results (counting the block itself) and the tools
/// in it. `None`: no block repeated.
///
/// The last entry may be a call that has not run yet (no result): it matches
/// on the call alone.
pub(crate) fn trailing_cycle(seq: &[Entry]) -> Option<(u32, Vec<String>)> {
    let n = seq.len();
    let mut best: Option<(usize, usize)> = None; // (repeats, period)
    for period in 2..=MAX_PERIOD {
        if n < period * 2 {
            continue;
        }
        let block = &seq[n - period..];
        // One call repeated is the job of `repeat_stop`; a waiting tool polls.
        let distinct = block.iter().any(|e| e.key != block[0].key);
        if !distinct || block.iter().any(Entry::waits) {
            continue;
        }
        let mut repeats = 1;
        while n >= period * (repeats + 1) {
            let start = n - period * (repeats + 1);
            let earlier = &seq[start..start + period];
            if !earlier.iter().zip(block).all(|(a, b)| a.matches(b)) {
                break;
            }
            repeats += 1;
        }
        if repeats >= 2 && best.is_none_or(|(r, _)| repeats > r) {
            best = Some((repeats, period));
        }
    }
    best.map(|(repeats, period)| {
        let mut names: Vec<String> = Vec::new();
        for e in &seq[n - period..] {
            if !names.contains(&e.key.0) {
                names.push(e.key.0.clone());
            }
        }
        (u32::try_from(repeats).unwrap_or(u32::MAX), names)
    })
}

/// The hint the model reads next to the result of a call that continues a cycle.
pub(crate) fn hint(repeats: u32, names: &[String]) -> String {
    format!(
        "{CYCLE_HINT_MARK} you have now repeated the same sequence of calls ({}) {repeats} times \
         and the results did not change. Do not run it again. Re-read the current state, then \
         use a different element, address or tool, or tell the user what is blocking you.]",
        names.join(" -> ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn call(name: &str, arg: u32, result: Option<&str>) -> Entry {
        Entry::new((name.to_owned(), json!({ "a": arg })), result)
    }

    fn click_snapshot(times: usize) -> Vec<Entry> {
        (0..times)
            .flat_map(|_| {
                [
                    call("browser_click", 1, Some("clicked")),
                    call("browser_snapshot", 0, Some("# Page (http://a/)\nbutton")),
                ]
            })
            .collect()
    }

    fn repeats(seq: &[Entry]) -> u32 {
        trailing_cycle(seq).map_or(1, |(r, _)| r)
    }

    #[test]
    fn click_snapshot_cycle_counts_its_repeats() {
        assert_eq!(repeats(&click_snapshot(1)), 1);
        assert_eq!(repeats(&click_snapshot(2)), 2);
        assert_eq!(repeats(&click_snapshot(4)), 4);
    }

    #[test]
    fn a_call_not_run_yet_matches_on_the_call_alone() {
        let mut seq = click_snapshot(2);
        seq.push(call("browser_click", 1, None));
        seq.push(call("browser_snapshot", 0, None));
        assert_eq!(repeats(&seq), 3);
    }

    #[test]
    fn a_changed_result_is_progress_not_a_cycle() {
        let mut seq = click_snapshot(2);
        seq.push(call("browser_click", 1, Some("clicked")));
        seq.push(call(
            "browser_snapshot",
            0,
            Some("# Page (http://a/)\nanother view"),
        ));
        // The newest block differs from the two before it.
        assert_eq!(repeats(&seq), 1);
    }

    #[test]
    fn different_arguments_break_the_cycle() {
        let mut seq = click_snapshot(3);
        seq.push(call("browser_click", 2, Some("clicked")));
        seq.push(call(
            "browser_snapshot",
            0,
            Some("# Page (http://a/)\nbutton"),
        ));
        assert_eq!(repeats(&seq), 1);
    }

    #[test]
    fn three_call_cycles_are_found() {
        let block = || {
            [
                call("a", 0, Some("x")),
                call("b", 0, Some("y")),
                call("c", 0, Some("z")),
            ]
        };
        let seq: Vec<Entry> = (0..3).flat_map(|_| block()).collect();
        assert_eq!(repeats(&seq), 3);
    }

    #[test]
    fn one_call_repeated_is_not_a_cycle_here() {
        let seq: Vec<Entry> = (0..8).map(|_| call("a", 0, Some("x"))).collect();
        assert_eq!(repeats(&seq), 1);
    }

    #[test]
    fn polling_with_a_wait_is_not_a_cycle() {
        let seq: Vec<Entry> = (0..6)
            .flat_map(|_| {
                [
                    call("wait", 2, Some("waited")),
                    call("snap", 0, Some("same")),
                ]
            })
            .collect();
        assert_eq!(repeats(&seq), 1);
    }

    #[test]
    fn loop_notes_do_not_change_a_fingerprint() {
        let plain = "clicked";
        let warned = format!("{plain}{REPEAT_WARN_MARK} has now been made 3 times in a row]");
        let hinted = format!("{plain}{}", hint(3, &["a".into(), "b".into()]));
        assert_eq!(fingerprint(plain), fingerprint(&warned));
        assert_eq!(fingerprint(plain), fingerprint(&hinted));
        assert_ne!(fingerprint(plain), fingerprint("clicked again"));
    }

    #[test]
    fn names_list_each_tool_of_the_cycle_once() {
        let (_, names) = trailing_cycle(&click_snapshot(3)).unwrap();
        assert_eq!(names, ["browser_click", "browser_snapshot"]);
        assert!(trailing_cycle(&click_snapshot(1)).is_none());
    }

    #[test]
    fn rows_written_before_cycle_detection_still_load() {
        let old = json!({"id": "1", "name": "t", "arguments": "{}", "repeat": 1});
        let rec: crate::shared::CallRec = serde_json::from_value(old).unwrap();
        assert_eq!(rec.cycle, 0);
    }
}

//! Standing orders: validation, active hours, the daily cap and the `OK` / `NEEDS_OWNER` contract.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use protocol::BotId;

use crate::{
    ActiveHours, MisfirePolicy, Payload, Result, Schedule, ScheduleExecutor, ScheduleSpec,
    ScheduleStore, StandingVerdict,
};

fn utc_ms(h: u32, m: u32) -> i64 {
    Utc.with_ymd_and_hms(2026, 6, 10, h, m, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn order(hours: Option<ActiveHours>, cap: u32) -> Payload {
    Payload::StandingOrder {
        checklist: "Is the backup disk under 90% full?".into(),
        active_hours: hours,
        max_runs_per_day: cap,
    }
}

fn hours(start: &str, end: &str, tz: &str) -> ActiveHours {
    ActiveHours {
        start: start.into(),
        end: end.into(),
        timezone: tz.into(),
    }
}

#[test]
fn payload_round_trips_and_defaults_the_cap() {
    let p: Payload = serde_json::from_value(serde_json::json!({
        "kind": "standing_order",
        "checklist": "look at the disk",
    }))
    .unwrap();
    assert_eq!(
        p,
        Payload::StandingOrder {
            checklist: "look at the disk".into(),
            active_hours: None,
            max_runs_per_day: crate::DEFAULT_MAX_RUNS_PER_DAY,
        }
    );
    p.validate().unwrap();
    let again: Payload = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(p, again);
}

#[test]
fn validation_refuses_bad_definitions() {
    let ok = hours("08:00", "22:00", "Europe/Istanbul");
    order(Some(ok.clone()), 12).validate().unwrap();
    for bad in [
        order(None, 0),
        order(None, crate::MAX_RUNS_PER_DAY_LIMIT + 1),
        order(Some(hours("8:00", "22:00", "UTC")), 1),
        order(Some(hours("08:00", "25:00", "UTC")), 1),
        order(Some(hours("08:00", "08:00", "UTC")), 1),
        order(Some(hours("08:00", "22:00", "Mars/Base")), 1),
        Payload::StandingOrder {
            checklist: "  ".into(),
            active_hours: None,
            max_runs_per_day: 1,
        },
        Payload::StandingOrder {
            checklist: "x".repeat(crate::MAX_CHECKLIST_CHARS + 1),
            active_hours: None,
            max_runs_per_day: 1,
        },
        Payload::StandingOrder {
            checklist: "look\u{202e}here".into(),
            active_hours: None,
            max_runs_per_day: 1,
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
    // Other payloads are not this method's business.
    Payload::AgentPrompt {
        text: String::new(),
    }
    .validate()
    .unwrap();
}

#[test]
fn store_refuses_an_invalid_standing_order() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let s = Schedule::new(
        BotId::new(),
        None,
        "bad",
        ScheduleSpec::Every { seconds: 60 },
        order(None, 0),
        MisfirePolicy::Skip,
        0,
    )
    .unwrap();
    assert!(store.insert(&s).is_err());
}

#[test]
fn active_hours_follow_the_zone_and_cross_midnight() {
    // Istanbul is UTC+3 all year.
    let day = hours("08:00", "22:00", "Europe/Istanbul");
    assert!(day.contains(utc_ms(5, 0)).unwrap()); // 08:00 local
    assert!(day.contains(utc_ms(18, 59)).unwrap()); // 21:59 local
    assert!(!day.contains(utc_ms(19, 0)).unwrap()); // 22:00 local: end is exclusive
    assert!(!day.contains(utc_ms(4, 59)).unwrap());
    let night = hours("22:00", "06:00", "UTC");
    assert!(night.contains(utc_ms(23, 30)).unwrap());
    assert!(night.contains(utc_ms(5, 59)).unwrap());
    assert!(!night.contains(utc_ms(6, 0)).unwrap());
    assert!(!night.contains(utc_ms(12, 0)).unwrap());
}

#[test]
fn the_cap_counts_per_local_day() {
    let p = order(Some(hours("00:00", "23:59", "Europe/Istanbul")), 5);
    // 21:30 UTC on the 10th is already the 11th in Istanbul.
    assert_eq!(p.day_key(utc_ms(20, 59)), "2026-06-10");
    assert_eq!(p.day_key(utc_ms(21, 0)), "2026-06-11");
    assert_eq!(order(None, 5).day_key(utc_ms(21, 0)), "2026-06-10");
}

#[test]
fn verdict_reads_only_the_last_line() {
    use StandingVerdict::{NeedsOwner, Ok, Unclear};
    assert_eq!(StandingVerdict::parse("Disk is at 41%.\n\nOK"), Ok);
    assert_eq!(StandingVerdict::parse("OK\n"), Ok);
    assert_eq!(StandingVerdict::parse("**OK**"), Ok);
    assert_eq!(StandingVerdict::parse("`OK`  \n \n"), Ok);
    assert_eq!(
        StandingVerdict::parse("Disk is at 95%.\nNEEDS_OWNER(the backup disk is 95% full)"),
        NeedsOwner("the backup disk is 95% full".into())
    );
    // "OK" earlier in the report does not make it quiet; the last line decides.
    assert_eq!(
        StandingVerdict::parse("OK so far\nNEEDS_OWNER(mail from the bank)"),
        NeedsOwner("mail from the bank".into())
    );
    assert_eq!(StandingVerdict::parse("NEEDS_OWNER(x)\nOK"), Ok);
    for unclear in [
        "",
        "   \n",
        "Everything looks fine.",
        "OK, all fine",
        "OKAY",
        "ok",
        "NEEDS_OWNER",
        "NEEDS_OWNER()",
        "NEEDS_OWNER( )",
        "NEEDS_OWNER: disk full",
        "NEEDS_OWNER(unterminated",
    ] {
        assert_eq!(StandingVerdict::parse(unclear), Unclear, "{unclear:?}");
    }
}

#[test]
fn prompt_carries_the_checklist_and_the_contract() {
    let text = order(None, 3).standing_prompt().unwrap();
    assert!(text.contains("Is the backup disk under 90% full?"));
    assert!(text.contains("OK") && text.contains("NEEDS_OWNER("));
    assert!(
        Payload::AgentPrompt { text: "x".into() }
            .standing_prompt()
            .is_none()
    );
}

#[test]
fn claims_stop_at_the_cap_and_a_new_day_starts_again() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let s = Schedule::new(
        BotId::new(),
        None,
        "cap",
        ScheduleSpec::Every { seconds: 60 },
        order(None, 2),
        MisfirePolicy::Skip,
        0,
    )
    .unwrap();
    store.insert(&s).unwrap();
    assert!(
        store
            .claim_standing_order_run(s.id, "2026-06-10", 2)
            .unwrap()
    );
    assert!(
        store
            .claim_standing_order_run(s.id, "2026-06-10", 2)
            .unwrap()
    );
    assert!(
        !store
            .claim_standing_order_run(s.id, "2026-06-10", 2)
            .unwrap()
    );
    assert_eq!(store.standing_order_runs(s.id, "2026-06-10").unwrap(), 2);
    assert!(
        store
            .claim_standing_order_run(s.id, "2026-06-11", 2)
            .unwrap()
    );
    // The earlier day was forgotten.
    assert_eq!(store.standing_order_runs(s.id, "2026-06-10").unwrap(), 0);
    // A lowered cap applies at once.
    assert!(
        !store
            .claim_standing_order_run(s.id, "2026-06-11", 1)
            .unwrap()
    );
}

#[derive(Default)]
struct Counting(Mutex<u32>);

#[async_trait]
impl ScheduleExecutor for Counting {
    async fn execute(&self, _: &Schedule) -> Result<()> {
        *self.0.lock() += 1;
        Ok(())
    }
}

#[tokio::test]
async fn a_firing_outside_the_hours_or_over_the_cap_starts_nothing() {
    use crate::engine::Dispatch::{Ran, Skipped};
    let store = ScheduleStore::open_in_memory().unwrap();
    let exec = Arc::new(Counting::default());
    let mut s = Schedule::new(
        BotId::new(),
        None,
        "watch",
        ScheduleSpec::Every { seconds: 60 },
        order(Some(hours("08:00", "22:00", "UTC")), 2),
        MisfirePolicy::Skip,
        0,
    )
    .unwrap();
    s.author = crate::ScheduleAuthor::Owner;
    store.insert(&s).unwrap();
    let fire = |at| {
        let (store, exec, s) = (&store, &exec, &s);
        async move {
            crate::engine::execute_checked(store, exec.as_ref(), s, None, at)
                .await
                .unwrap()
        }
    };
    // Night: skipped, and it does not use up the cap.
    assert_eq!(fire(utc_ms(3, 0)).await, Skipped);
    assert_eq!(fire(utc_ms(23, 0)).await, Skipped);
    assert_eq!(*exec.0.lock(), 0);
    // Day: two runs, then the cap holds.
    assert_eq!(fire(utc_ms(9, 0)).await, Ran);
    assert_eq!(fire(utc_ms(10, 0)).await, Ran);
    assert_eq!(fire(utc_ms(11, 0)).await, Skipped);
    assert_eq!(*exec.0.lock(), 2);
    // Next day it runs again.
    let next = utc_ms(9, 0) + 24 * 3_600_000;
    assert_eq!(fire(next).await, Ran);
    assert_eq!(*exec.0.lock(), 3);
}

#[tokio::test]
async fn other_payloads_are_not_gated() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let exec = Arc::new(Counting::default());
    let s = Schedule::new(
        BotId::new(),
        None,
        "plain",
        ScheduleSpec::Every { seconds: 60 },
        Payload::AgentPrompt { text: "go".into() },
        MisfirePolicy::Skip,
        0,
    )
    .unwrap();
    store.insert(&s).unwrap();
    for _ in 0..30 {
        let out = crate::engine::execute_checked(&store, exec.as_ref(), &s, None, utc_ms(3, 0))
            .await
            .unwrap();
        assert_eq!(out, crate::engine::Dispatch::Ran);
    }
}

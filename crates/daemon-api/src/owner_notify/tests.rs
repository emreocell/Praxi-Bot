use super::*;
use missions::{Budgets, MissionState, NewItem, NewMission};

const HOUR: i64 = 3_600_000;

fn window(start: &str, end: &str, tz: &str) -> OwnerNotifySettings {
    OwnerNotifySettings {
        quiet_hours: Some(QuietWindow {
            start: start.into(),
            end: end.into(),
            timezone: tz.into(),
        }),
        ..OwnerNotifySettings::default()
    }
}

/// 2026-01-15 00:00:00 UTC.
const DAY: i64 = 1_768_435_200_000;

fn draft(kind: NoticeKind) -> Draft {
    Draft::new(kind, NoticeSource::Mission, "Backlinks".into())
}

#[test]
fn defaults_push_finish_failure_and_blocked_but_not_progress() {
    let s = OwnerNotifySettings::default();
    assert!(s.finished && s.failed && s.blocked);
    assert!(!s.progress, "progress stays off until the owner asks");
    assert!(s.quiet_hours.is_none());
}

#[test]
fn quiet_hours_cross_midnight_and_follow_the_zone() {
    let s = window("22:00", "08:00", "UTC");
    assert!(s.is_quiet(DAY + 23 * HOUR));
    assert!(s.is_quiet(DAY + 3 * HOUR));
    assert!(!s.is_quiet(DAY + 8 * HOUR), "the end is outside the window");
    assert!(!s.is_quiet(DAY + 12 * HOUR));
    assert!(s.is_quiet(DAY + 22 * HOUR), "the start is inside");
    // Istanbul is UTC+3 all year: 20:00 UTC is 23:00 there (quiet), 12:00 UTC is 15:00 (not).
    let tr = window("22:00", "08:00", "Europe/Istanbul");
    assert!(tr.is_quiet(DAY + 20 * HOUR));
    assert!(!tr.is_quiet(DAY + 12 * HOUR));
    // A same-day window.
    let day = window("13:00", "14:00", "UTC");
    assert!(day.is_quiet(DAY + 13 * HOUR + 30 * 60_000));
    assert!(!day.is_quiet(DAY + 14 * HOUR));
    // Equal ends and no window never silence anything.
    assert!(!window("09:00", "09:00", "UTC").is_quiet(DAY + 9 * HOUR));
    assert!(!OwnerNotifySettings::default().is_quiet(DAY));
}

#[test]
fn bad_quiet_hours_are_refused() {
    assert!(window("22:00", "08:00", "").validate().is_ok());
    assert!(window("25:00", "08:00", "").validate().is_err());
    assert!(window("22:00", "8:00", "").validate().is_err());
    assert!(window("22:00", "08:00", "Mars/Olympus").validate().is_err());
    // Equal ends are ambiguous (all day, or never): refused rather than guessed.
    assert!(window("22:00", "22:00", "UTC").validate().is_err());
    assert!(
        window("22:00", "08:00", &"A/".repeat(40))
            .validate()
            .is_err()
    );
}

#[test]
fn settings_survive_a_restart_and_a_broken_file_falls_back_to_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let hub = Hub::load(dir.path());
    let mut s = window("22:00", "08:00", "UTC");
    s.progress = true;
    s.finished = false;
    hub.set_settings(s.clone(), DAY).unwrap();
    assert_eq!(Hub::load(dir.path()).settings(), s);
    std::fs::write(dir.path().join(SETTINGS_FILE), b"{ not json").unwrap();
    assert_eq!(
        Hub::load(dir.path()).settings(),
        OwnerNotifySettings::default()
    );
}

#[test]
fn a_finished_notice_is_pushed_and_kept_for_the_panel() {
    let hub = Hub::in_memory();
    let (_, mut rx) = hub.subscribe_push(None);
    let n = hub.publish(draft(NoticeKind::Finished), DAY).unwrap();
    assert_eq!(n.seq, 1);
    assert_eq!(rx.try_recv().unwrap().id, n.id);
    assert_eq!(hub.recent(0).len(), 1);
    assert_eq!(hub.get(&n.id).unwrap().title, "Backlinks");
    assert!(
        hub.recent(1).is_empty(),
        "after the newest there is nothing"
    );
}

#[test]
fn a_disabled_event_stays_in_the_panel_but_is_not_pushed() {
    let hub = Hub::in_memory();
    let (_, mut rx) = hub.subscribe_push(None);
    hub.publish(draft(NoticeKind::Progress), DAY).unwrap();
    assert!(rx.try_recv().is_err(), "progress is off by default");
    assert_eq!(hub.recent(0).len(), 1);
    let s = OwnerNotifySettings {
        progress: true,
        ..OwnerNotifySettings::default()
    };
    hub.set_settings(s, DAY).unwrap();
    hub.publish(draft(NoticeKind::Progress), DAY).unwrap();
    assert_eq!(rx.try_recv().unwrap().kind, NoticeKind::Progress);
}

#[test]
fn quiet_hours_hold_pushes_and_release_them_late_and_in_order() {
    let hub = Hub::in_memory();
    hub.set_settings(window("22:00", "08:00", "UTC"), DAY)
        .unwrap();
    let (_, mut rx) = hub.subscribe_push(None);
    let night = DAY + 23 * HOUR;
    let a = hub.publish(draft(NoticeKind::Blocked), night).unwrap();
    let b = hub
        .publish(draft(NoticeKind::Finished), night + 1000)
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "nothing is pushed inside the window"
    );
    assert_eq!(hub.recent(0).len(), 2, "the panel still has both");
    hub.release_held(night + 2000);
    assert!(rx.try_recv().is_err(), "still quiet");
    let morning = DAY + 24 * HOUR + 8 * HOUR;
    hub.release_held(morning);
    let (first, second) = (rx.try_recv().unwrap(), rx.try_recv().unwrap());
    assert_eq!(
        (first.id.as_str(), second.id.as_str()),
        (a.id.as_str(), b.id.as_str())
    );
    assert!(first.late && second.late);
    assert!(rx.try_recv().is_err());
    hub.release_held(morning + 1);
    assert!(rx.try_recv().is_err(), "released once");
}

#[test]
fn a_milestone_missed_during_quiet_hours_is_dropped_not_sent_late() {
    let hub = Hub::in_memory();
    let mut s = window("22:00", "08:00", "UTC");
    s.progress = true;
    hub.set_settings(s, DAY).unwrap();
    let (_, mut rx) = hub.subscribe_push(None);
    hub.publish(draft(NoticeKind::Progress), DAY + 23 * HOUR)
        .unwrap();
    hub.release_held(DAY + 33 * HOUR);
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_new_notice_flushes_what_is_held_first_so_the_order_holds() {
    let hub = Hub::in_memory();
    hub.set_settings(window("22:00", "08:00", "UTC"), DAY)
        .unwrap();
    let (_, mut rx) = hub.subscribe_push(None);
    let held = hub
        .publish(draft(NoticeKind::Blocked), DAY + 23 * HOUR)
        .unwrap();
    let live = hub
        .publish(draft(NoticeKind::Finished), DAY + 32 * HOUR)
        .unwrap();
    assert_eq!(rx.try_recv().unwrap().id, held.id);
    assert_eq!(rx.try_recv().unwrap().id, live.id);
    assert!(held.seq < live.seq);
}

#[test]
fn a_reconnecting_bridge_replays_what_it_missed_and_a_new_one_starts_fresh() {
    let hub = Hub::in_memory();
    let one = hub.publish(draft(NoticeKind::Finished), DAY).unwrap();
    let two = hub.publish(draft(NoticeKind::Failed), DAY + 1).unwrap();
    let (fresh, _rx) = hub.subscribe_push(None);
    assert!(
        fresh.is_empty(),
        "a bridge that never connected starts from now"
    );
    let (replay, _rx) = hub.subscribe_push(Some(one.seq));
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].id, two.id);
    assert_eq!(hub.latest_push_seq(), two.seq);
}

#[test]
fn the_same_schedule_does_not_notify_again_inside_its_window() {
    let hub = Hub::in_memory();
    let with_key = |k: &str| {
        let mut d = draft(NoticeKind::Finished);
        d.coalesce = Some((k.to_owned(), FINISHED_COALESCE_MS));
        d
    };
    assert!(hub.publish(with_key("s1:finished"), DAY).is_some());
    assert!(hub.publish(with_key("s1:finished"), DAY + 1000).is_none());
    assert!(hub.publish(with_key("s2:finished"), DAY + 1000).is_some());
    assert!(
        hub.publish(with_key("s1:finished"), DAY + FINISHED_COALESCE_MS + 1)
            .is_some()
    );
    assert_eq!(
        hub.recent(0).len(),
        3,
        "a coalesced one is not logged either"
    );
}

#[test]
fn milestones_are_announced_once_per_quarter() {
    let p = |done, total| missions::Progress {
        total,
        done,
        ..missions::Progress::default()
    };
    assert_eq!(milestone_quarter(p(0, 10)), 0);
    assert_eq!(milestone_quarter(p(2, 10)), 0);
    assert_eq!(milestone_quarter(p(3, 10)), 1);
    assert_eq!(milestone_quarter(p(5, 10)), 2);
    assert_eq!(milestone_quarter(p(8, 10)), 3);
    assert_eq!(
        milestone_quarter(p(10, 10)),
        3,
        "the end is the finished notice"
    );
    assert_eq!(
        milestone_quarter(p(2, 3)),
        0,
        "too small to have milestones"
    );
    let hub = Hub::in_memory();
    assert!(hub.new_milestone("m", 1));
    assert!(!hub.new_milestone("m", 1));
    assert!(hub.new_milestone("m", 2));
    assert!(!hub.new_milestone("m", 1), "never goes back");
    hub.seed_milestone("n", 2);
    assert!(
        !hub.new_milestone("n", 2),
        "a restart does not repeat a passed one"
    );
    assert!(hub.new_milestone("n", 3));
}

#[test]
fn scheduled_runs_are_told_apart_from_other_runs_by_their_kind() {
    let id = Uuid::now_v7();
    assert_eq!(
        scheduled_kind(&format!("turn:schedule:{id}")),
        Some(ScheduledKind {
            schedule: id,
            due_ms: None
        })
    );
    assert_eq!(
        scheduled_kind(&format!("turn:schedule:{id}:1700000000000:2")),
        Some(ScheduledKind {
            schedule: id,
            due_ms: Some(1_700_000_000_000)
        })
    );
    assert_eq!(
        scheduled_kind(&format!("turn:external_schedule:{id}:5:1"))
            .unwrap()
            .due_ms,
        Some(5)
    );
    assert_eq!(scheduled_kind("turn"), None);
    assert_eq!(scheduled_kind("turn:subagent"), None);
    assert_eq!(scheduled_kind(&missions::run_kind("m1")), None);
    assert_eq!(scheduled_kind("turn:schedule:not-a-uuid"), None);
}

fn mission_with(state: MissionState) -> (missions::MissionStore, missions::Mission) {
    let store = missions::MissionStore::open_in_memory().unwrap();
    let items = ["a.com", "b.com", "c.com", "d.com"].map(|k| NewItem {
        key: k.into(),
        kind: "site".into(),
        title: format!("Sign up on {k}"),
        payload: None,
    });
    let m = store
        .create(
            &NewMission {
                user_id: "u".into(),
                bot_id: Uuid::nil().to_string(),
                chat_id: Uuid::nil().to_string(),
                title: "Backlinks".into(),
                goal_text: "g".into(),
                target_done: None,
                budgets: Budgets::default(),
                items: items.into(),
            },
            1,
        )
        .unwrap();
    let upd = |status, text: &str| missions::ItemUpdate {
        status: Some(status),
        result: Some(text.into()),
        blocked_reason: (status == missions::ItemStatus::Blocked).then(|| text.to_owned()),
        error: (status == missions::ItemStatus::Failed).then(|| text.to_owned()),
        live_only: None,
    };
    store
        .update_item(
            &m.id,
            "a.com",
            &upd(missions::ItemStatus::Done, "account created"),
            false,
            2,
        )
        .unwrap();
    store
        .update_item(
            &m.id,
            "b.com",
            &upd(missions::ItemStatus::Blocked, "which name?"),
            false,
            2,
        )
        .unwrap();
    store
        .update_item(
            &m.id,
            "c.com",
            &missions::ItemUpdate {
                status: Some(missions::ItemStatus::Blocked),
                blocked_reason: Some("a verification code is needed".into()),
                live_only: Some(true),
                ..missions::ItemUpdate::default()
            },
            false,
            2,
        )
        .unwrap();
    store
        .update_item(
            &m.id,
            "d.com",
            &upd(missions::ItemStatus::Failed, "needs payment"),
            false,
            2,
        )
        .unwrap();
    let m = store.get(&m.id).unwrap().unwrap();
    let d = missions::digest::build(&store, &m, state.as_str(), 3).unwrap();
    store.set_state(&m.id, state, Some("because"), 3).unwrap();
    store.set_digest_once(&m.id, &d, 4).unwrap();
    let m = store.get(&m.id).unwrap().unwrap();
    (store, m)
}

#[test]
fn a_blocked_mission_batches_every_question_and_keeps_codes_in_the_app() {
    let (store, m) = mission_with(MissionState::Blocked);
    let blocked = store
        .items(&m.id, Some(missions::ItemStatus::Blocked))
        .unwrap();
    let d = mission_draft(&m, "Scout", &blocked).unwrap();
    assert_eq!(d.kind, NoticeKind::Blocked);
    assert_eq!(d.bot_name, "Scout");
    assert_eq!(
        d.questions.len(),
        1,
        "one message, all answerable questions"
    );
    assert_eq!(d.questions[0].key, "b.com");
    assert_eq!(d.questions[0].text.as_deref(), Some("which name?"));
    assert_eq!(d.app_only.len(), 1, "a code is never answered from a chat");
    assert!(d.app_only[0].contains("verification code"));
    assert!(!d.can_retry, "the answer is what releases it");
    let p = d.progress.unwrap();
    assert_eq!((p.total, p.done, p.blocked, p.failed), (4, 1, 2, 1));
    assert_eq!(d.failed[0].key, "d.com");
}

#[test]
fn a_mission_blocked_on_a_budget_offers_to_go_on() {
    let (_, m) = mission_with(MissionState::Blocked);
    let d = mission_draft(&m, "Scout", &[]).unwrap();
    assert!(d.can_retry && d.questions.is_empty());
}

#[test]
fn finished_and_failed_missions_report_and_a_stopped_one_stays_silent() {
    let (_, done) = mission_with(MissionState::Done);
    let d = mission_draft(&done, "Scout", &[]).unwrap();
    assert_eq!(d.kind, NoticeKind::Finished);
    assert_eq!(d.done[0].text.as_deref(), Some("account created"));
    assert_eq!(d.reason.as_deref(), Some("because"));
    let (_, failed) = mission_with(MissionState::Failed);
    assert_eq!(
        mission_draft(&failed, "Scout", &[]).unwrap().kind,
        NoticeKind::Failed
    );
    let (_, stopped) = mission_with(MissionState::Cancelled);
    assert!(
        mission_draft(&stopped, "Scout", &[]).is_none(),
        "the owner stopped it themselves: no push"
    );
}

#[cfg(unix)]
#[test]
fn the_settings_file_is_private_synced_and_leaves_no_temp_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(SETTINGS_FILE);
    // An older, looser file and a leftover temp file from a crash.
    std::fs::write(&path, b"{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, b"junk").unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();
    let hub = Hub::load(dir.path());
    hub.set_settings(window("22:00", "08:00", "UTC"), DAY)
        .unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "only the daemon's user reads the settings");
    assert!(!tmp.exists());
    assert_eq!(
        Hub::load(dir.path()).settings(),
        window("22:00", "08:00", "UTC")
    );
}

#[test]
fn a_notice_is_answered_once_and_only_while_it_is_the_newest_of_its_mission() {
    let hub = Hub::in_memory();
    let mut d = draft(NoticeKind::Blocked);
    d.mission_id = Some("m1".into());
    let old = hub.publish(d, DAY).unwrap();
    let mut d = draft(NoticeKind::Blocked);
    d.mission_id = Some("m1".into());
    let new = hub.publish(d, DAY).unwrap();
    let mut other = draft(NoticeKind::Blocked);
    other.mission_id = Some("m2".into());
    let other = hub.publish(other, DAY).unwrap();

    assert!(
        hub.claim_answers(&old).is_err(),
        "a newer notice replaced it"
    );
    assert!(hub.claim_answers(&new).is_ok());
    assert!(hub.claim_answers(&new).is_err(), "single use");
    hub.release_answers(&new.id);
    assert!(
        hub.claim_answers(&new).is_ok(),
        "a failed answer gives it back"
    );
    assert!(
        hub.claim_answers(&other).is_ok(),
        "another mission is not affected"
    );
}

#[test]
fn an_answered_mark_does_not_outlive_its_notice() {
    let hub = Hub::in_memory();
    let first = hub.publish(draft(NoticeKind::Blocked), DAY).unwrap();
    assert!(hub.claim_answers(&first).is_ok());
    for _ in 0..LOG_MAX {
        hub.publish(draft(NoticeKind::Finished), DAY).unwrap();
    }
    assert!(hub.get(&first.id).is_none());
    assert!(
        hub.state.lock().answered.is_empty(),
        "the set stays bounded by the log"
    );
}

fn question(i: usize) -> NoticeItem {
    NoticeItem {
        key: i.to_string(),
        title: String::new(),
        text: None,
    }
}

#[test]
fn one_run_keeps_a_bounded_number_of_questions_and_app_only_items() {
    let mut w = RunWants::default();
    for i in 0..MAX_WANTS * 3 {
        w.ask(question(i));
        w.app(format!("approval {i}"));
    }
    assert_eq!(w.questions.len(), MAX_WANTS);
    assert_eq!(w.app_only.len(), MAX_WANTS);
    assert!(w.overflowed);
    assert_eq!(w.questions[0].key, "0", "the first ones are kept");
}

#[tokio::test]
async fn only_a_bounded_number_of_runs_are_batched_at_once() {
    let mut watched = Watched::default();
    for _ in 0..MAX_WATCHED_RUNS {
        assert!(watched.wants(RunId::new()).is_some());
    }
    assert!(watched.wants(RunId::new()).is_none(), "the table is full");
    assert_eq!(watched.runs.len(), MAX_WATCHED_RUNS);
    assert!(watched.full_warned, "the overflow is logged once");
    // A run that is already batched still takes more.
    let known = *watched.runs.keys().next().unwrap();
    assert!(watched.wants(known).is_some());
    // Once most runs are done there is room again.
    let keep: Vec<RunId> = watched.runs.keys().take(10).copied().collect();
    watched.runs.retain(|r, _| keep.contains(r));
    assert!(watched.wants(RunId::new()).is_some());
    assert!(!watched.full_warned);
}

use super::*;

fn recurring() -> Schedule {
    Schedule::new(
        BotId::new(),
        None,
        "before",
        ScheduleSpec::Every { seconds: 1 },
        Payload::AgentPrompt {
            text: "before".into(),
        },
        MisfirePolicy::RunAll,
        0,
    )
    .unwrap()
}

fn occurrence_states(store: &ScheduleStore) -> Vec<(i64, String, Option<String>)> {
    let conn = store.conn();
    let mut stmt = conn
        .prepare("SELECT due_ms, state, run_id FROM occurrences ORDER BY due_ms")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn migration_preserves_previous_schema_rows_and_is_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schedules.db");
    let old = recurring();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_schedules.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO schedules VALUES (?1, ?2, NULL, ?3, ?4, ?5, 1, 0, 500, 1000, 'run_all')",
            params![
                old.id.to_string(),
                old.bot_id.to_string(),
                old.name,
                serde_json::to_string(&old.spec).unwrap(),
                serde_json::to_string(&old.payload).unwrap()
            ],
        )
        .unwrap();
    }
    assert_eq!(
        ScheduleStore::validate_and_migrate(&path).unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(
        ScheduleStore::validate_and_migrate(&path).unwrap(),
        SCHEMA_VERSION
    );
    let store = ScheduleStore::open(dir.path()).unwrap();
    let migrated = store.get(old.id).unwrap().unwrap();
    assert_eq!(migrated.name, old.name);
    assert_eq!(migrated.revision, 0);
    assert_eq!(migrated.last_run_at, Some(500));
    assert_eq!(migrated.next_run_at, Some(1_000));
    assert_eq!(store.unresolved_occurrence_count().unwrap(), 0);
}

#[test]
fn definition_and_upsert_preserve_runtime_and_reject_stale_revision() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let mut stale = recurring();
    store.insert(&stale).unwrap();
    store
        .record_run_intent(stale.id, 1_000, Some(2_000))
        .unwrap();
    stale.name = "edited".into();
    assert!(store.update_definition(&stale).unwrap());
    let fresh = store.get(stale.id).unwrap().unwrap();
    assert_eq!(fresh.revision, 1);
    assert_eq!(fresh.last_run_at, Some(1_000));
    assert_eq!(fresh.next_run_at, Some(2_000));
    assert!(!store.update_definition(&stale).unwrap());
    assert!(
        !store
            .set_next_run_after_edit(stale.id, 0, Some(99))
            .unwrap()
    );
    assert_eq!(store.get(stale.id).unwrap().unwrap(), fresh);

    stale.name = "upsert edit".into();
    store.upsert(&stale).unwrap();
    let fresh = store.get(stale.id).unwrap().unwrap();
    assert_eq!(fresh.revision, 2);
    assert_eq!(fresh.last_run_at, Some(1_000));
    assert_eq!(fresh.next_run_at, Some(2_000));
}

#[test]
fn rearm_uses_current_definition_and_stale_engine_cannot_undo_it() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let mut old = recurring();
    store.insert(&old).unwrap();
    let stale_engine = old.clone();
    old.spec = ScheduleSpec::Every { seconds: 10 };
    let (fresh, _) = store
        .update_definition_and_rearm(&old, None, 5_000)
        .unwrap()
        .unwrap();
    assert_eq!(fresh.next_run_at, Some(15_000));
    assert!(
        !store
            .advance_if_current(&stale_engine, Some(2_000))
            .unwrap()
    );
    assert!(
        !store
            .reserve_occurrences(&stale_engine, &[1_000], Some(2_000))
            .unwrap()
    );
    store.disable_if_current(&stale_engine).unwrap();
    assert_eq!(store.get(old.id).unwrap().unwrap(), fresh);
    assert_eq!(occurrence_states(&store), vec![]);

    // A second edit's spec is compared with the current spec, not with the
    // first route's old snapshot. Switching it back must re-arm from now.
    let (reverted, _) = store
        .update_definition_and_rearm(&stale_engine, None, 6_000)
        .unwrap()
        .unwrap();
    assert_eq!(reverted.next_run_at, Some(7_000));
}

#[test]
fn reservation_rolls_back_timer_if_intent_insert_fails() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let row = recurring();
    store.insert(&row).unwrap();
    store
        .conn()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_second BEFORE INSERT ON occurrences
         WHEN NEW.due_ms = 2000 BEGIN SELECT RAISE(ABORT, 'injected intent failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .reserve_occurrences(&row, &[1_000, 2_000], Some(3_000))
            .is_err()
    );
    assert_eq!(store.get(row.id).unwrap().unwrap(), row);
    assert!(
        occurrence_states(&store).is_empty(),
        "first intent must roll back too"
    );
}

#[test]
fn independent_store_connections_cannot_reserve_same_window_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScheduleStore::open(dir.path()).unwrap();
    let row = recurring();
    store.insert(&row).unwrap();
    let other = ScheduleStore::open(dir.path()).unwrap();
    let ready = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            ready.wait();
            other
                .reserve_occurrences(&row, &[1_000], Some(2_000))
                .unwrap()
        });
        ready.wait();
        let first = store
            .reserve_occurrences(&row, &[1_000], Some(2_000))
            .unwrap();
        let second = worker.join().unwrap();
        assert_ne!(first, second);
    });
    assert_eq!(
        occurrence_states(&store),
        vec![(1_000, "pending".into(), None)]
    );
}

#[test]
fn claim_is_single_use_and_does_not_fabricate_started_run() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let mut row = recurring();
    row.spec = ScheduleSpec::At { unix_ms: 1_000 };
    store.insert(&row).unwrap();
    assert!(store.reserve_occurrences(&row, &[1_000], None).unwrap());
    let reserved = store.get(row.id).unwrap().unwrap();
    assert!(reserved.enabled, "a reservation is not a durable start");
    assert_eq!(reserved.last_run_at, None);
    assert_eq!(
        occurrence_states(&store),
        vec![(1_000, "pending".into(), None)]
    );
    assert!(
        store
            .claim_occurrence(row.id, 0, 1_000, 1_005)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .claim_occurrence(row.id, 0, 1_000, 1_006)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.get(row.id).unwrap().unwrap().last_run_at, Some(1_005));
    assert_eq!(
        occurrence_states(&store),
        vec![(1_000, "dispatched".into(), None)]
    );
    store.finish_occurrence(row.id, 1_000, true).unwrap();
    assert_eq!(
        occurrence_states(&store),
        vec![(1_000, "succeeded".into(), None)]
    );
}

#[test]
fn edit_disable_and_delete_clear_pending_but_do_not_cancel_dispatched_work() {
    for change in ["edit", "disable", "delete"] {
        let store = ScheduleStore::open_in_memory().unwrap();
        let row = recurring();
        store.insert(&row).unwrap();
        store
            .reserve_occurrences(&row, &[1_000, 2_000], Some(3_000))
            .unwrap();
        assert!(
            store
                .claim_occurrence(row.id, 0, 1_000, 2_500)
                .unwrap()
                .is_some()
        );
        match change {
            "edit" => {
                let mut edit = store.get(row.id).unwrap().unwrap();
                edit.payload = Payload::AgentPrompt {
                    text: "after".into(),
                };
                store.update_definition(&edit).unwrap();
            }
            "disable" => {
                store.set_enabled(row.id, false, None).unwrap();
            }
            "delete" => {
                store.delete(row.id).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            store
                .claim_occurrence(row.id, 0, 2_000, 2_501)
                .unwrap()
                .is_none()
        );
        let expected = if change == "delete" {
            vec![]
        } else {
            vec![(1_000, "dispatched".into(), None)]
        };
        assert_eq!(occurrence_states(&store), expected);
        store.finish_occurrence(row.id, 1_000, true).unwrap();
        if change != "delete" {
            assert_eq!(
                occurrence_states(&store),
                vec![(1_000, "succeeded".into(), None)]
            );
        }
    }
}

#[test]
fn claim_rechecks_definition_after_the_engine_read() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let row = recurring();
    store.insert(&row).unwrap();
    store
        .reserve_occurrences(&row, &[1_000], Some(2_000))
        .unwrap();
    let inspected = store.get(row.id).unwrap().unwrap();
    assert!(inspected.enabled);
    store.set_enabled(row.id, false, None).unwrap();
    assert!(
        store
            .claim_occurrence(inspected.id, inspected.revision, 1_000, 1_000)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.get(row.id).unwrap().unwrap().last_run_at, None);
}

#[test]
fn exhausted_revision_fails_closed_without_changing_runtime() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let row = recurring();
    store.insert(&row).unwrap();
    store
        .conn()
        .execute("UPDATE schedules SET revision = ?1", [i64::MAX])
        .unwrap();
    let exhausted = store.get(row.id).unwrap().unwrap();
    assert!(store.update_definition(&exhausted).is_err());
    assert!(
        store
            .update_definition_and_rearm(&exhausted, None, 500)
            .is_err()
    );
    assert_eq!(store.get(row.id).unwrap().unwrap(), exhausted);
}

fn at_schedule(unix_ms: i64) -> Schedule {
    Schedule::new(
        BotId::new(),
        None,
        "once",
        ScheduleSpec::At { unix_ms },
        Payload::AgentPrompt {
            text: "once".into(),
        },
        MisfirePolicy::RunOnce,
        0,
    )
    .unwrap()
}

fn raw_occurrence(store: &ScheduleStore, id: TaskId, due_ms: i64, state: &str) {
    store
        .conn()
        .execute(
            "INSERT INTO occurrences (schedule_id, due_ms, revision, run_id, state) \
             VALUES (?1, ?2, 0, NULL, ?3)",
            params![id.to_string(), due_ms, state],
        )
        .unwrap();
}

#[test]
fn finished_occurrence_history_is_bounded_per_schedule() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let busy = recurring();
    let other = recurring();
    store.insert(&busy).unwrap();
    store.insert(&other).unwrap();
    raw_occurrence(&store, other.id, 1, "succeeded");
    raw_occurrence(&store, busy.id, 1_000_000, "pending");
    for due in 1..=150_i64 {
        raw_occurrence(&store, busy.id, due, "dispatched");
        store.finish_occurrence(busy.id, due, due % 2 == 0).unwrap();
    }
    let finished = |id: TaskId| -> Vec<i64> {
        let conn = store.conn();
        let mut st = conn
            .prepare(
                "SELECT due_ms FROM occurrences WHERE schedule_id = ?1 \
                 AND state IN ('succeeded', 'failed') ORDER BY due_ms",
            )
            .unwrap();
        st.query_map([id.to_string()], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let kept = finished(busy.id);
    assert_eq!(kept.len(), 100, "history must be capped");
    assert_eq!(kept.first(), Some(&51), "the oldest rows are dropped");
    assert_eq!(kept.last(), Some(&150));
    assert_eq!(finished(other.id), vec![1], "other schedules are untouched");
    // Unresolved work is never pruned.
    assert_eq!(store.unresolved_occurrence_count().unwrap(), 1);
}

#[test]
fn update_and_upsert_re_enabling_a_disabled_schedule_arms_its_timer() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let row = recurring();
    store.insert(&row).unwrap();
    assert!(store.set_enabled(row.id, false, None).unwrap());
    let disabled = store.get(row.id).unwrap().unwrap();
    assert_eq!(disabled.next_run_at, None);

    let mut edit = disabled.clone();
    edit.enabled = true;
    assert!(store.update(&edit).unwrap());
    let after_update = store.get(row.id).unwrap().unwrap();
    assert!(after_update.enabled);
    assert!(
        after_update.next_run_at.is_some(),
        "a schedule re-enabled through update() must be able to fire"
    );

    assert!(store.set_enabled(row.id, false, None).unwrap());
    let mut again = store.get(row.id).unwrap().unwrap();
    again.enabled = true;
    store.upsert(&again).unwrap();
    let after_upsert = store.get(row.id).unwrap().unwrap();
    assert!(after_upsert.enabled);
    assert!(
        after_upsert.next_run_at.is_some(),
        "a schedule re-enabled through upsert() must be able to fire"
    );
}

#[test]
fn set_enabled_true_without_a_timer_computes_one() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let row = recurring();
    store.insert(&row).unwrap();
    store.set_enabled(row.id, false, None).unwrap();
    store.set_enabled(row.id, true, None).unwrap();
    assert!(store.get(row.id).unwrap().unwrap().next_run_at.is_some());
}

#[test]
fn update_with_a_changed_spec_rearms_and_upsert_keeps_cursors_for_other_edits() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let mut row = recurring();
    store.insert(&row).unwrap();
    store.record_run_intent(row.id, 1_000, Some(2_000)).unwrap();
    row.spec = ScheduleSpec::Every { seconds: 3_600 };
    assert!(store.update(&row).unwrap());
    let fresh = store.get(row.id).unwrap().unwrap();
    assert_ne!(
        fresh.next_run_at,
        Some(2_000),
        "an hourly spec cannot keep the old 1s cursor"
    );
    assert_eq!(fresh.last_run_at, Some(1_000));
    assert_eq!(fresh.created_at, 0);
}

#[test]
fn startup_recovery_makes_unresolved_occurrences_visible_and_parks_one_shots() {
    let store = ScheduleStore::open_in_memory().unwrap();
    let mut once = at_schedule(5_000);
    once.next_run_at = None; // consumed at reservation, then the process died
    store.insert(&once).unwrap();
    raw_occurrence(&store, once.id, 5_000, "dispatched");
    let recurring_row = recurring();
    store.insert(&recurring_row).unwrap();
    raw_occurrence(&store, recurring_row.id, 1_000, "pending");
    raw_occurrence(&store, recurring_row.id, 2_000, "dispatched");
    raw_occurrence(&store, recurring_row.id, 3_000, "succeeded");

    assert_eq!(store.recover_unresolved_occurrences().unwrap(), 3);

    assert_eq!(store.unresolved_occurrence_count().unwrap(), 0);
    let once = store.get(once.id).unwrap().unwrap();
    assert!(!once.enabled, "a dead one-shot must not stay enabled");
    assert_eq!(once.next_run_at, None);
    assert_eq!(once.missed_occurrences, 1);
    let listed = store
        .list()
        .unwrap()
        .into_iter()
        .find(|s| s.id == recurring_row.id)
        .unwrap();
    assert!(listed.enabled, "recurring schedules keep running");
    assert_eq!(listed.missed_occurrences, 2);
    assert_eq!(
        occurrence_states(&store)
            .into_iter()
            .filter(|(due, ..)| *due < 3_000 && *due >= 1_000)
            .map(|(_, state, _)| state)
            .collect::<Vec<_>>(),
        vec!["missed", "interrupted"]
    );
    // Idempotent: a second start finds nothing new.
    assert_eq!(store.recover_unresolved_occurrences().unwrap(), 0);
}

#[test]
fn schema_version_constant_matches_embedded_migrations() {
    assert_eq!(MIGRATIONS.len(), SCHEMA_VERSION as usize);
}

#[test]
fn migration_0003_keeps_occurrence_rows_from_the_previous_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schedules.db");
    let old = recurring();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_schedules.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../migrations/0002_schedule_consistency.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO schedules VALUES (?1, ?2, NULL, 'n', ?3, ?4, 1, 0, NULL, 1000, 'skip', 4)",
            params![
                old.id.to_string(),
                old.bot_id.to_string(),
                serde_json::to_string(&old.spec).unwrap(),
                serde_json::to_string(&old.payload).unwrap()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO occurrences VALUES (?1, 7, 4, NULL, 'dispatched')",
            [old.id.to_string()],
        )
        .unwrap();
    }
    assert_eq!(
        ScheduleStore::validate_and_migrate(&path).unwrap(),
        SCHEMA_VERSION
    );
    let store = ScheduleStore::open(dir.path()).unwrap();
    assert_eq!(
        occurrence_states(&store),
        vec![(7, "dispatched".to_string(), None)]
    );
    assert_eq!(store.unresolved_occurrence_count().unwrap(), 1);
    assert_eq!(store.get(old.id).unwrap().unwrap().revision, 4);
}

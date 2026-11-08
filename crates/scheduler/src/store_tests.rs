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
    assert_eq!(ScheduleStore::validate_and_migrate(&path).unwrap(), 2);
    assert_eq!(ScheduleStore::validate_and_migrate(&path).unwrap(), 2);
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

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

#[tokio::test]
async fn stale_definition_update_does_not_roll_back_fired_cursor() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let mut stale = recurring();
    store.insert(&stale).unwrap();
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(0));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    clock.advance_ms(1_000);
    scheduler.settle().await;
    assert_eq!(exec.count(), 1);

    stale.name = "after".into();
    stale.payload = Payload::AgentPrompt {
        text: "after".into(),
    };
    assert!(store.update(&stale).unwrap());
    let fresh = store.get(stale.id).unwrap().unwrap();
    assert_eq!(fresh.last_run_at, Some(1_000));
    assert_eq!(fresh.next_run_at, Some(2_000));
    scheduler.settle().await;
    assert_eq!(
        exec.count(),
        1,
        "an edit cannot replay the consumed occurrence"
    );
    scheduler.shutdown().await;
}

#[derive(Clone, Copy)]
enum Change {
    Delete,
    Disable,
    DisableAndEnable,
    Payload,
}

struct ChangeAfterFirst {
    store: Arc<ScheduleStore>,
    change: Change,
    runs: Mutex<Vec<Schedule>>,
}

#[async_trait]
impl ScheduleExecutor for ChangeAfterFirst {
    async fn execute(&self, schedule: &Schedule) -> Result<()> {
        let first = {
            let mut runs = self.runs.lock();
            runs.push(schedule.clone());
            runs.len() == 1
        };
        if first {
            match self.change {
                Change::Delete => {
                    self.store.delete(schedule.id)?;
                }
                Change::Disable => {
                    self.store.set_enabled(schedule.id, false, None)?;
                }
                Change::DisableAndEnable => {
                    self.store.set_enabled(schedule.id, false, None)?;
                    self.store.set_enabled(schedule.id, true, Some(10_000))?;
                }
                Change::Payload => {
                    let mut edit = self.store.get(schedule.id)?.unwrap();
                    edit.payload = Payload::AgentPrompt {
                        text: "after".into(),
                    };
                    self.store.update(&edit)?;
                }
            }
        }
        Ok(())
    }
}

async fn change_after_first(change: Change) {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let row = recurring();
    store.insert(&row).unwrap();
    let exec = Arc::new(ChangeAfterFirst {
        store: Arc::clone(&store),
        change,
        runs: Mutex::new(Vec::new()),
    });
    // Five due slots, all computed from the fake clock, without any sleeps.
    let clock = Arc::new(FakeClock::new(5_000));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;
    assert_eq!(
        exec.runs.lock().len(),
        1,
        "old catch-up work must be cancelled"
    );
    assert_eq!(exec.runs.lock()[0].payload, row.payload);
    if matches!(change, Change::Payload) {
        clock.advance_ms(1_000);
        scheduler.settle().await;
        let runs = exec.runs.lock();
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[1].payload,
            Payload::AgentPrompt {
                text: "after".into()
            }
        );
    }
    scheduler.shutdown().await;
}

#[tokio::test]
async fn run_all_stops_after_delete() {
    change_after_first(Change::Delete).await;
}

#[tokio::test]
async fn run_all_stops_after_disable() {
    change_after_first(Change::Disable).await;
}

#[tokio::test]
async fn run_all_stops_after_disable_and_reenable() {
    change_after_first(Change::DisableAndEnable).await;
}

#[tokio::test]
async fn run_all_stops_using_old_payload_after_edit() {
    change_after_first(Change::Payload).await;
}

#[tokio::test]
async fn unresolved_intents_survive_restart_without_unsafe_replay() {
    for dispatched in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let row = recurring();
        {
            let store = ScheduleStore::open(dir.path()).unwrap();
            store.insert(&row).unwrap();
            assert!(
                store
                    .reserve_occurrences(&row, &[1_000], Some(2_000))
                    .unwrap()
            );
            if dispatched {
                assert!(
                    store
                        .claim_occurrence(row.id, 0, 1_000, 1_000)
                        .unwrap()
                        .is_some()
                );
            }
            // Crash after reservation, or after dispatch but before a durable
            // start acknowledgment. Neither case may be invented as started.
        }
        let store = Arc::new(ScheduleStore::open(dir.path()).unwrap());
        let exec = RecordingExecutor::new();
        let scheduler = Scheduler::start_with_clock(
            Arc::clone(&store),
            exec.clone(),
            Arc::new(FakeClock::new(1_000)),
        );
        scheduler.settle().await;
        assert_eq!(exec.count(), 0, "task 136 must supply safe replay first");
        // The row is retained, but reclassified from live work to visible
        // history: nothing may stay `pending` / `dispatched` forever.
        assert_eq!(store.unresolved_occurrence_count().unwrap(), 0);
        let after = store.get(row.id).unwrap().unwrap();
        assert_eq!(after.missed_occurrences, 1);
        assert_eq!(after.next_run_at, Some(2_000));
        assert!(after.enabled);
        scheduler.shutdown().await;
    }
}

#[tokio::test]
async fn consumed_identity_does_not_block_later_catch_up_slots() {
    let store = Arc::new(ScheduleStore::open_in_memory().unwrap());
    let row = recurring();
    store.insert(&row).unwrap();
    let exec = RecordingExecutor::new();
    let clock = Arc::new(FakeClock::new(1_000));
    let scheduler = Scheduler::start_with_clock(Arc::clone(&store), exec.clone(), clock.clone());
    scheduler.settle().await;
    assert_eq!(exec.count(), 1);
    // Revisit an already consumed identity (e.g. a backwards wall-clock
    // re-anchor), followed by two new identities in the same catch-up window.
    store.advance_next_run(row.id, Some(1_000)).unwrap();
    clock.jump_wall_ms(2_001);
    scheduler.settle().await;
    assert_eq!(exec.count(), 3, "only the two new identities may run");
    scheduler.shutdown().await;
}

#[tokio::test]
async fn interrupted_one_shot_is_parked_visibly_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut once = Schedule::new(
        BotId::new(),
        None,
        "once",
        ScheduleSpec::At { unix_ms: 1_000 },
        Payload::AgentPrompt {
            text: "once".into(),
        },
        MisfirePolicy::RunOnce,
        0,
    )
    .unwrap();
    {
        let store = ScheduleStore::open(dir.path()).unwrap();
        store.insert(&once).unwrap();
        // Reserved (timer consumed) and dispatched, then the process died.
        assert!(store.reserve_occurrences(&once, &[1_000], None).unwrap());
        assert!(
            store
                .claim_occurrence(once.id, 0, 1_000, 1_000)
                .unwrap()
                .is_some()
        );
    }
    let store = Arc::new(ScheduleStore::open(dir.path()).unwrap());
    let exec = RecordingExecutor::new();
    let scheduler = Scheduler::start_with_clock(
        Arc::clone(&store),
        exec.clone(),
        Arc::new(FakeClock::new(5_000)),
    );
    scheduler.settle().await;
    assert_eq!(exec.count(), 0, "no unsafe replay");
    once = store.list().unwrap().into_iter().next().unwrap();
    assert!(!once.enabled, "not silently enabled-and-dead");
    assert_eq!(once.next_run_at, None);
    assert_eq!(once.missed_occurrences, 1);
    scheduler.shutdown().await;
}

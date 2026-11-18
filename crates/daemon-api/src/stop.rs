//! `POST /v1/runs/stop`: one Stop, one target set, fixed at first receipt.
//!
//! The caller cannot know which runs "existed" when the user pressed Stop, so
//! the daemon decides: the first time a `stop_id` arrives it reads the user's
//! open runs once and stores the target set. A repeat of the same `stop_id`
//! never adds a run, and a run created after the first receipt is never
//! cancelled by it. No clock of the caller is compared with anything.
//!
//! State is in memory and bounded, per actor (a paired peer cannot push the
//! local user's Stops out):
//! - target sets: [`LOCAL_MAX_STOPS`] (local) / [`PEER_MAX_STOPS`] (each peer),
//!   each for [`STOP_TTL`];
//! - a compact set of every `stop_id` ever seen: [`LOCAL_MAX_SEEN`] (local) /
//!   [`PEER_MAX_SEEN`] (each peer) ids, 16 bytes each plus ordering. A repeat
//!   whose target set was dropped but whose id is still in this set answers
//!   `expired: true` and cancels nothing (fail closed). Only when an actor has
//!   sent more than that many Stops is its oldest id forgotten; that id would
//!   then count as a first receipt again.
//!
//! Cost of the per-peer seen-id sets: they are keyed by the peer's transport
//! key and never removed (not on revoke, not on re-pair), so the memory held is
//! bounded by (distinct authenticated peer keys over the daemon's lifetime) x
//! [`PEER_MAX_SEEN`] ids x ~50 B. Unauthenticated callers cannot grow it.
//! Dropping an actor's set when its pairing is revoked is a possible follow-up.
//!
//! Nothing is persisted: after a daemon restart a repeat is a first receipt;
//! the restart pauses every running run (`AgentLoop::recover`), so it stops
//! the runs that were live before the restart.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Extension, State};
use protocol::RunId;
use task_runtime::RunState;
use uuid::Uuid;

use crate::dto::{StopOutcome, StopRunResult, StopRunsInput, StopRunsResponse, StopScope};
use crate::live_view::{Actor, require_local};
use crate::routes::ApiJson;
use crate::{ApiError, AppState};

/// Target sets kept for the local user.
pub(crate) const LOCAL_MAX_STOPS: usize = 256;
/// Target sets kept per paired peer.
pub(crate) const PEER_MAX_STOPS: usize = 32;
/// Seen `stop_id`s kept for the local user.
pub(crate) const LOCAL_MAX_SEEN: usize = 65_536;
/// Seen `stop_id`s kept per paired peer.
pub(crate) const PEER_MAX_SEEN: usize = 1024;
/// How long a stop's target set is kept.
pub(crate) const STOP_TTL: Duration = Duration::from_secs(3600);
/// Largest `run_ids` list.
pub(crate) const MAX_RUN_IDS: usize = 1024;
/// Request body limit (1024 ids plus slack).
pub(crate) const MAX_BODY_BYTES: usize = 128 * 1024;

struct Member {
    run: RunId,
    /// The run's state when the target set was fixed (`None`: unknown id).
    at_receipt: Option<RunState>,
    outcome: StopOutcome,
    error: Option<String>,
}

struct Entry {
    at: Instant,
    members: Vec<Member>,
}

#[derive(Default)]
struct Seen {
    set: HashSet<Uuid>,
    order: VecDeque<Uuid>,
}

/// Target sets by `(actor, stop_id)`: another actor reusing an id gets its own
/// stop and never sees or retries somebody else's.
#[derive(Default)]
pub(crate) struct StopBook {
    entries: HashMap<(String, Uuid), Entry>,
    seen: HashMap<String, Seen>,
}

impl StopBook {
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, e| now.duration_since(e.at) < STOP_TTL);
    }

    fn is_seen(&self, actor: &str, id: Uuid) -> bool {
        self.seen.get(actor).is_some_and(|s| s.set.contains(&id))
    }

    /// Remember `id` for `actor`; forget that actor's oldest id at the bound.
    fn note_seen(&mut self, actor: &str, id: Uuid, local: bool) {
        let max = if local { LOCAL_MAX_SEEN } else { PEER_MAX_SEEN };
        let seen = self.seen.entry(actor.to_owned()).or_default();
        if seen.set.insert(id) {
            seen.order.push_back(id);
        }
        while seen.order.len() > max {
            if let Some(old) = seen.order.pop_front() {
                seen.set.remove(&old);
            }
        }
    }

    /// Drop the actor's oldest target sets until one more fits.
    fn make_room(&mut self, actor: &str, local: bool) {
        let max = if local {
            LOCAL_MAX_STOPS
        } else {
            PEER_MAX_STOPS
        };
        loop {
            let mine: Vec<_> = self
                .entries
                .iter()
                .filter(|((a, _), _)| a == actor)
                .map(|(k, e)| (e.at, k.clone()))
                .collect();
            if mine.len() < max {
                return;
            }
            let Some((_, oldest)) = mine.into_iter().min_by_key(|(at, _)| *at) else {
                return;
            };
            self.entries.remove(&oldest);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn len_of(&self, actor: &str) -> usize {
        self.entries.keys().filter(|(a, _)| a == actor).count()
    }

    /// Age every target set past the TTL (the book otherwise expires them by
    /// wall time).
    #[cfg(test)]
    pub(crate) fn expire_all(&mut self) {
        self.entries.clear();
    }
}

/// Test seams: an event trace, and forced audit / cancel failures.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Seam {
    pub(crate) trace: parking_lot::Mutex<Vec<String>>,
    pub(crate) fail_audit: std::sync::atomic::AtomicBool,
    pub(crate) fail_cancel: parking_lot::Mutex<HashSet<RunId>>,
    /// Runs whose cancel "fires the token" (the call succeeds) but which stay
    /// live: the run is still winding down, nothing reaches its state row.
    pub(crate) hold_live: parking_lot::Mutex<HashSet<RunId>>,
}

fn is_open(state: Option<RunState>) -> bool {
    state.is_some_and(|s| !s.is_terminal())
}

fn state_of(s: &AppState, run: RunId) -> Option<(String, Option<RunState>)> {
    s.storage()
        .run_state_of_user(s.user_id(), run)
        .ok()
        .flatten()
        .map(|st| {
            let parsed = RunState::parse(&st);
            (st, parsed)
        })
}

fn attempt_cancel(s: &AppState, run: RunId, force: bool) -> Result<(), String> {
    #[cfg(test)]
    {
        s.stop_seam().trace.lock().push(format!("cancel:{}", run.0));
        if s.stop_seam().fail_cancel.lock().contains(&run) {
            return Err("test: cancel failed".into());
        }
        if s.stop_seam().hold_live.lock().contains(&run) {
            return Ok(());
        }
    }
    if force {
        s.agent().force_cancel(run).map_err(|e| e.to_string())
    } else {
        s.agent().cancel(run).map_err(|e| e.to_string())
    }
}

/// Cancel one member through the per-run cancel path and classify the result.
fn cancel_member(s: &AppState, m: &mut Member, force: bool) {
    m.error = None;
    match attempt_cancel(s, m.run, force) {
        Ok(()) => refresh(s, m),
        Err(e) => match state_of(s, m.run) {
            // Cancelled meanwhile: by this stop (a parent took the child with
            // it) if the run was open when the set was fixed.
            Some((_, Some(RunState::Cancelled))) if is_open(m.at_receipt) => {
                m.outcome = StopOutcome::Cancelled;
            }
            Some((_, Some(st))) if st.is_terminal() => m.outcome = StopOutcome::AlreadyTerminal,
            None => m.outcome = StopOutcome::NotFound,
            Some(_) => {
                m.outcome = StopOutcome::Failed;
                m.error = Some(e);
            }
        },
    }
}

/// Re-read a member's state and settle `cancel_requested` once it is terminal.
fn refresh(s: &AppState, m: &mut Member) {
    m.outcome = match state_of(s, m.run) {
        Some((_, Some(RunState::Cancelled))) => StopOutcome::Cancelled,
        Some((_, Some(st))) if st.is_terminal() => StopOutcome::AlreadyTerminal,
        Some(_) => StopOutcome::CancelRequested,
        None => StopOutcome::NotFound,
    };
}

fn result_of(s: &AppState, m: &Member) -> StopRunResult {
    StopRunResult {
        run_id: m.run.0,
        outcome: m.outcome,
        state: state_of(s, m.run).map(|(st, _)| st),
        error: m.error.clone(),
    }
}

fn write_audit(
    s: &AppState,
    actor: &Actor,
    action: &str,
    meta: &serde_json::Value,
) -> Result<(), ApiError> {
    #[cfg(test)]
    {
        s.stop_seam().trace.lock().push("audit".into());
        if s.stop_seam()
            .fail_audit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ApiError::internal("test: audit write failed"));
        }
    }
    let who = if actor.local {
        "user"
    } else {
        actor.key.as_str()
    };
    s.storage()
        .append_audit(s.user_id(), None, who, action, meta)
        .map(|_| ())
        .map_err(ApiError::internal)
}

/// The audit row precedes the cancels. A peer whose row cannot be written is
/// refused (nothing is cancelled, nothing recorded); the local user's Stop
/// goes ahead regardless (the panic button must not be weaker than the
/// per-run cancel, which writes no row) and the response says `audited: false`.
fn audit_first(
    s: &AppState,
    actor: &Actor,
    action: &str,
    meta: &serde_json::Value,
) -> Result<bool, ApiError> {
    match write_audit(s, actor, action, meta) {
        Ok(()) => Ok(true),
        Err(e) if actor.local => {
            eprintln!("runs.stop: audit write failed, stopping anyway: {e:?}");
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

fn respond(
    s: &AppState,
    stop_id: Uuid,
    first_seen: bool,
    audited: bool,
    members: &[Member],
) -> StopRunsResponse {
    StopRunsResponse {
        stop_id,
        first_seen,
        expired: false,
        audited,
        results: members.iter().map(|m| result_of(s, m)).collect(),
    }
}

fn repeat(
    s: &AppState,
    actor: &Actor,
    stop_id: Uuid,
    entry: &mut Entry,
    force: bool,
) -> Result<StopRunsResponse, ApiError> {
    // Members cancelled earlier but not terminal then: settle them, no cancel.
    for m in &mut entry.members {
        if m.outcome == StopOutcome::CancelRequested {
            refresh(s, m);
        }
    }
    let failed = entry
        .members
        .iter()
        .filter(|m| m.outcome == StopOutcome::Failed)
        .count();
    // One audit row per retry of failed cancels; re-issuing the cancel of a
    // slow-unwinding live run (every few seconds) writes none.
    let audited = if failed > 0 {
        audit_first(
            s,
            actor,
            "runs.stop_retry",
            &serde_json::json!({"stop_id": stop_id, "count": failed}),
        )?
    } else {
        true
    };
    for m in &mut entry.members {
        if matches!(
            m.outcome,
            StopOutcome::Failed | StopOutcome::CancelRequested
        ) {
            cancel_member(s, m, force);
        }
    }
    Ok(respond(s, stop_id, false, audited, &entry.members))
}

fn first_receipt(
    s: &AppState,
    actor: &Actor,
    input: &StopRunsInput,
) -> Result<(Vec<Member>, bool), ApiError> {
    // Causal boundary: `all_existing` reads the open runs in ONE statement; a
    // run inserted after it is not in the set. `listed` only looks at its ids.
    let open: HashMap<Uuid, Option<RunState>> = if input.scope == StopScope::AllExisting {
        s.storage()
            .list_open_run_states(s.user_id())?
            .into_iter()
            .map(|(id, st)| (id.0, RunState::parse(&st)))
            .collect()
    } else {
        HashMap::new()
    };
    let mut seen = HashSet::new();
    let mut members = Vec::new();
    let mut add = |id: Uuid| -> Result<(), ApiError> {
        if !seen.insert(id) {
            return Ok(());
        }
        let known = match open.get(&id) {
            Some(st) => Some(*st),
            None => s
                .storage()
                .run_state_of_user(s.user_id(), RunId(id))?
                .map(|st| RunState::parse(&st)),
        };
        let (at_receipt, outcome) = match known {
            None => (None, StopOutcome::NotFound),
            Some(st) if st.is_some_and(RunState::is_terminal) => (st, StopOutcome::AlreadyTerminal),
            // Pending: cancelled below.
            Some(st) => (st, StopOutcome::Failed),
        };
        members.push(Member {
            run: RunId(id),
            at_receipt,
            outcome,
            error: None,
        });
        Ok(())
    };
    for id in &input.run_ids {
        add(*id)?;
    }
    for id in open
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
    {
        add(id)?; // UUIDv7: oldest first
    }
    // The audit row is durable before any cancel is issued. A stop that
    // targets nothing has no effect to attribute and writes none.
    let audited = if members.is_empty() {
        true
    } else {
        audit_first(
            s,
            actor,
            "runs.stop",
            &serde_json::json!({
                "stop_id": input.stop_id,
                "scope": input.scope,
                "count": members.len(),
            }),
        )?
    };
    for m in &mut members {
        if m.outcome == StopOutcome::Failed {
            cancel_member(s, m, input.force);
        }
    }
    Ok((members, audited))
}

fn process(
    s: &AppState,
    actor: &Actor,
    input: &StopRunsInput,
) -> Result<StopRunsResponse, ApiError> {
    // The whole first receipt (read, audit, cancels) and every repeat run under
    // this lock: two requests with one `stop_id` cannot fix two sets, and the
    // second one sees the first one's outcomes.
    let mut book = s.stops().lock();
    book.prune(Instant::now());
    let key = (actor.key.clone(), input.stop_id);
    if let Some(entry) = book.entries.get_mut(&key) {
        return repeat(s, actor, input.stop_id, entry, input.force);
    }
    if book.is_seen(&key.0, key.1) {
        // Known id whose target set is gone: never sweep again.
        return Ok(StopRunsResponse {
            stop_id: input.stop_id,
            first_seen: false,
            expired: true,
            audited: true,
            results: Vec::new(),
        });
    }
    let (members, audited) = first_receipt(s, actor, input)?;
    let out = respond(s, input.stop_id, true, audited, &members);
    book.note_seen(&key.0, key.1, actor.local);
    book.make_room(&key.0, actor.local);
    book.entries.insert(
        key,
        Entry {
            at: Instant::now(),
            members,
        },
    );
    Ok(out)
}

/// Stop runs: cancel a target set that is fixed at the first receipt of
/// `stop_id`. Idempotent per `stop_id`.
#[utoipa::path(post, path = "/v1/runs/stop", tag = "runs", security(("bearer" = [])),
    request_body = StopRunsInput,
    responses((status = 200, body = StopRunsResponse),
              (status = 400, body = crate::error::ErrorBody),
              (status = 403, body = crate::error::ErrorBody,
               description = "scope all_existing is for the local user only")))]
pub(crate) async fn stop_runs(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    ApiJson(input): ApiJson<StopRunsInput>,
) -> Result<Json<StopRunsResponse>, ApiError> {
    if input.run_ids.len() > MAX_RUN_IDS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_RUN_IDS} run ids"
        )));
    }
    // Paired peers may stop runs they can cancel one by one (the per-run cancel
    // route has no stricter rule); sweeping everything is the local user's.
    if input.scope == StopScope::AllExisting {
        require_local(&s, &actor, "runs_stop_all")?;
    }
    // Storage and the agent's cancel are blocking calls.
    let out = tokio::task::spawn_blocking(move || process(&s, &actor, &input))
        .await
        .map_err(ApiError::internal)??;
    Ok(Json(out))
}

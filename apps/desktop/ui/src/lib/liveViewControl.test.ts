import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  ControlStateDto,
  ReleaseRequest,
  TakeoverInfo,
  TargetInfo
} from "../api/types.ts";
import {
  applyControlStateDto,
  applyFrameRendered,
  applyInputAck,
  applyReleaseFailed,
  applyReleaseStarted,
  applySessionCreated,
  applyStatusRecord,
  applyStreamDisconnected,
  applyStreamStalenessCheck,
  applyTakeoverAccepted,
  applyTakeoverFailed,
  applyTakeoverStarted,
  buildReleasePayload,
  buildSessionHeartbeat,
  canSendLiveInput,
  computeWantSession,
  THUMBNAIL_REFRESH_MS,
  thumbnailDue,
  controlSnapshotKey,
  createInitialLiveViewState,
  isControlBusy,
  needsPauseRecovery,
  describeRecoveryPrompt,
  planViewerClose,
  reconnectDelayMs,
  releaseConfirmedBy,
  shouldOverrideOnStop,
  type LiveViewControllerState
} from "./liveViewState.ts";
import { applyReleaseConfirmed } from "./liveViewState.ts";
import { createModalStack } from "./modalStack.ts";
import { runRelease } from "./liveViewRelease.ts";
import type { FrameHeader, StatusHeader } from "./liveViewWire.ts";

const target: TargetInfo = {
  target_id: "t-1",
  bot_id: "bot-1",
  run_id: "run-1",
  host_id: "local",
  kind: "browser",
  generation: 2,
  capabilities: { codecs: ["jpeg"], view: true, input: true, reason: null },
  unavailable_reason: null
};

const session = {
  session_id: "sess-1",
  generation: 2,
  expires_in_ms: 20_000,
  input_capabilities: true,
  effective_profile: {
    profile: "watch" as const,
    codec: "jpeg" as const,
    max_width: 960,
    max_height: 600,
    max_fps: 1,
    quality: 60,
    burst_bytes: 2_000_000,
    bytes_per_second: 1_000_000
  },
  stream_url: "/v1/view/sessions/sess-1/frames"
};

const frame: FrameHeader = {
  v: 1,
  type: "frame",
  session_id: "sess-1",
  sent_at_ms: 1,
  target_id: "t-1",
  generation: 2,
  frame_seq: 1,
  captured_at_ms: 1,
  cause: "refresh",
  action_seq: null,
  codec: "jpeg",
  width: 1280,
  height: 800,
  source_rect: { x: 0, y: 0, width: 1280, height: 800, units: "physical_px" },
  geometry_revision: 7,
  repeated: false
};

const statusActive: StatusHeader = {
  v: 1,
  type: "status",
  session_id: "sess-1",
  state: "active",
  reason: null,
  effective_profile: { ...session.effective_profile, profile: "live" },
  control_epoch: 5,
  input_allowed: true
};

function takeover(over: Partial<TakeoverInfo> = {}): TakeoverInfo {
  return {
    id: "to-1",
    control_epoch: 5,
    input_expires_in_ms: 10_000,
    owner: "local-ui:1",
    reason: null,
    session_id: "sess-1",
    state: "active",
    target_id: "t-1",
    ...over
  };
}

function dto(t: TakeoverInfo | null): ControlStateDto {
  return {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: t ? "human_driving" : null,
    mode: t ? "human_driving" : "idle",
    release: null,
    takeover: t
  };
}

/** A session in which this client owns `to-1` and input is open. */
function owning(): LiveViewControllerState {
  let st = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  st = applyFrameRendered(st, frame, 1000);
  st = applyTakeoverAccepted(st, {
    takeover_id: "to-1",
    control_epoch: 5,
    state: "active"
  });
  st = applyStatusRecord(st, statusActive, 1010);
  assert.equal(canSendLiveInput(st, 1020).ok, true);
  return st;
}

// ---- finding 4: failed release must not be reported as confirmed -----------

test("release: rejected POST + daemon still active => failed, never confirmed", async () => {
  const active = dto(takeover());
  const outcome = await runRelease(
    {
      release: async () => {
        throw Object.assign(new Error("conflict"), { code: "conflict" });
      },
      getControlState: async () => active
    },
    {
      takeover_id: "to-1",
      control_epoch: 5,
      request_id: "req-1",
      outcome: "completed"
    }
  );
  assert.equal(outcome.kind, "failed");
  if (outcome.kind === "failed") {
    assert.equal(outcome.code, "conflict");
    assert.equal(outcome.dto?.takeover?.id, "to-1");
  }
});

test("release: a failed hand-back keeps ownership and the SAME request id for retry", async () => {
  let st = owning();
  const first = buildReleasePayload(st, "req-rel-1", "login_completed");
  assert.ok(first);
  st = applyReleaseStarted(st, first.request_id, first.human_action ?? null);

  const outcome = await runRelease(
    {
      release: async () => {
        throw new Error("network");
      },
      getControlState: async () => dto(takeover())
    },
    first
  );
  assert.equal(outcome.kind, "failed");
  if (outcome.kind === "failed") {
    if (outcome.dto) {
      st = applyControlStateDto(st, outcome.dto);
    }
    st = applyReleaseFailed(st, outcome.code, outcome.message);
  }
  assert.equal(st.ownedTakeoverId, "to-1", "ownership kept");
  assert.equal(st.pendingRequest?.kind, "release");
  assert.equal(st.lastError?.code, "release_failed", "error is surfaced");
  const retry = buildReleasePayload(st, "req-new-ignored", null);
  assert.equal(retry?.request_id, "req-rel-1");
  assert.equal(retry?.human_action, "login_completed");
});

test("release: confirmed only when fresh state shows the takeover gone", async () => {
  const payload: ReleaseRequest = {
    takeover_id: "to-1",
    control_epoch: 5,
    request_id: "req-1",
    outcome: "completed"
  };
  // response lost after the daemon applied it: GET proves it
  const lost = await runRelease(
    {
      release: async () => {
        throw new Error("timeout");
      },
      getControlState: async () => dto(null)
    },
    payload
  );
  assert.equal(lost.kind, "confirmed");

  const ok = await runRelease(
    { release: async () => dto(null), getControlState: async () => dto(null) },
    payload
  );
  assert.equal(ok.kind, "confirmed");

  // POST "succeeded" but the daemon still reports the takeover => not confirmed
  const unconfirmed = await runRelease(
    {
      release: async () => dto(takeover()),
      getControlState: async () => dto(takeover())
    },
    payload
  );
  assert.equal(unconfirmed.kind, "failed");
  if (unconfirmed.kind === "failed") {
    assert.equal(unconfirmed.code, "release_unconfirmed");
  }

  assert.equal(releaseConfirmedBy(dto(takeover({ id: "other" })), "to-1"), true);
  assert.equal(releaseConfirmedBy(dto(takeover()), "to-1"), false);
});

// ---- finding 6: authoritative state clears a pending release ---------------

test("pending release is cleared by state: takeover gone, lapsed to paused, or someone else's", () => {
  const releasing = (): LiveViewControllerState => {
    let st = owning();
    st = applyReleaseStarted(st, "req-1", null);
    return applyReleaseFailed(st, "release_failed", "x");
  };

  const gone = applyControlStateDto(releasing(), dto(null));
  assert.equal(gone.pendingRequest, null);
  assert.equal(gone.ownedTakeoverId, null);
  assert.equal(gone.lastError, null);

  const lapsed = applyControlStateDto(
    releasing(),
    dto(takeover({ state: "paused", reason: "disconnected" }))
  );
  assert.equal(lapsed.pendingRequest, null, "no dead retry loop on a lapsed lease");
  assert.equal(lapsed.viewState, "paused");

  const other = applyControlStateDto(
    releasing(),
    dto(takeover({ id: "to-peer", owner: "peer:phone", session_id: "s-9" }))
  );
  assert.equal(other.pendingRequest, null);

  const still = applyControlStateDto(releasing(), dto(takeover()));
  assert.equal(still.pendingRequest?.kind, "release", "still active keeps the retry");
});

// ---- finding 8: another actor's takeover ----------------------------------

test("another actor's active takeover is a watch state and can never be released by us", () => {
  let st = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  st = applyFrameRendered(st, frame, 1000);
  st = applyControlStateDto(
    st,
    dto(takeover({ id: "to-peer", owner: "peer:phone", session_id: "s-9" }))
  );
  assert.equal(st.viewState, "watch", "not 'active' (You are in control)");
  assert.equal(st.takeover?.id, "to-peer");
  assert.equal(st.ownedTakeoverId, null);
  assert.equal(canSendLiveInput(st, 2000).ok, false);
  assert.equal(
    buildReleasePayload(st, "req-x", null),
    null,
    "no release for a takeover we do not own"
  );
  assert.equal(applyReleaseStarted(st, "req-x"), st);
});

// ---- finding 9: input only from the status stream -------------------------

test("a control snapshot or ack can lower input access but never raise it", () => {
  let st = owning();
  // daemon flags geometry stale: input_allowed false
  st = applyStatusRecord(st, { ...statusActive, input_allowed: false }, 2000);
  assert.equal(st.inputAllowed, false);
  // a snapshot racing in must not re-enable it
  st = applyControlStateDto(st, dto(takeover()));
  assert.equal(st.inputAllowed, false);
  assert.equal(canSendLiveInput(st, 2100).ok, false);
  // an input ack must not either
  st = applyInputAck(st, 1, "active");
  assert.equal(st.inputAllowed, false);
  // only a status record raises it again
  st = applyStatusRecord(st, statusActive, 2200);
  assert.equal(canSendLiveInput(st, 2300).ok, true);
});

test("takeover accepted before the status record does not open input by itself", () => {
  let st = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  st = applyFrameRendered(st, frame, 1000);
  st = applyTakeoverAccepted(st, {
    takeover_id: "to-1",
    control_epoch: 5,
    state: "active"
  });
  assert.equal(canSendLiveInput(st, 1100).ok, false);
  st = applyStatusRecord(st, statusActive, 1200);
  assert.equal(canSendLiveInput(st, 1300).ok, true);
});

// ---- finding 7: dead stream stops renewing the lease ----------------------

test("heartbeat stops carrying takeover_id once the stream is dead or frozen", () => {
  const st = owning();
  assert.deepEqual(buildSessionHeartbeat(st), {
    takeover_id: "to-1",
    control_epoch: 5
  });
  assert.deepEqual(buildSessionHeartbeat(applyStreamDisconnected(st)), {});
  const stale = applyStreamStalenessCheck(st, 1010 + 60_000);
  assert.equal(stale.streamStale, true);
  assert.deepEqual(buildSessionHeartbeat(stale), {});
});

test("reconnect backoff is bounded and a new session never auto-resumes input", () => {
  assert.deepEqual(
    [1, 2, 3, 4, 5, 6, 10].map(reconnectDelayMs),
    [1000, 2000, 4000, 8000, 15000, 15000, 15000]
  );
  let st = applyStreamDisconnected(owning());
  assert.equal(st.disconnected, true);
  st = applySessionCreated(st, target, { ...session, session_id: "sess-2" });
  assert.equal(st.disconnected, false);
  assert.equal(st.inputAllowed, false, "reacquire/status needed before input");
  assert.equal(canSendLiveInput(st, 5000).ok, false);
});

// ---- finding 14: lost takeover response -----------------------------------

test("a takeover whose response was lost is retried with the same request id", () => {
  let st = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  st = applyTakeoverStarted(st, "req-tk-1");
  st = applyTakeoverFailed(st, "takeover_failed", "timeout", {
    retainRequestId: true
  });
  assert.equal(st.pendingRequest, null);
  assert.equal(st.retryTakeoverRequestId, "req-tk-1");
  // a definitive daemon error starts afresh
  st = applyTakeoverStarted(st, "req-tk-1");
  st = applyTakeoverFailed(st, "conflict", "no", { retainRequestId: false });
  assert.equal(st.retryTakeoverRequestId, null);
});

test("a takeover this very session created is adopted, not shown as 'other owner'", () => {
  let st = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  st = applyFrameRendered(st, frame, 1000);
  st = applyStatusRecord(st, statusActive, 1010);
  st = applyControlStateDto(st, dto(takeover()));
  assert.equal(st.ownedTakeoverId, "to-1");
  assert.equal(st.viewState, "active");
  assert.equal(canSendLiveInput(st, 1100).ok, true);
});

test("a takeover we already handed back is not re-adopted from a stale snapshot", () => {
  let st = owning();
  st = applyReleaseStarted(st, "req-1", null);
  st = applyReleaseConfirmed(st, dto(null));
  const stale = applyControlStateDto(st, dto(takeover()));
  assert.equal(stale.ownedTakeoverId, null);
  assert.equal(stale.viewState, "watch");
});

// ---- finding 10/12: busy + close ------------------------------------------

test("target switching is locked while holding, acquiring or handing back control", () => {
  const watching = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  assert.equal(isControlBusy(watching), false);
  assert.equal(isControlBusy(owning()), true);
  assert.equal(
    isControlBusy(applyTakeoverStarted(watching, "req-1")),
    true,
    "acquiring"
  );
  assert.equal(
    isControlBusy(applyReleaseStarted(owning(), "req-2", null)),
    true,
    "releasing"
  );
});

test("closing the viewer while in control hands control back first", () => {
  const watching = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  assert.equal(planViewerClose(watching), "close");
  assert.equal(planViewerClose(owning()), "release_first");
  assert.equal(
    planViewerClose(applyReleaseStarted(owning(), "req-1", null)),
    "dismiss"
  );
});

test("panel and viewer closed (or window hidden) stop the capture session", () => {
  const base = {
    hasClient: true,
    documentHidden: false,
    target,
    viewerOpen: false
  };
  // The card alone never holds a live session: the host stays idle until someone clicks.
  assert.equal(computeWantSession(base), false);
  assert.equal(computeWantSession({ ...base, viewerOpen: true }), true);
  assert.equal(
    computeWantSession({ ...base, viewerOpen: true, documentHidden: true }),
    false
  );
  assert.equal(
    computeWantSession({
      ...base,
      viewerOpen: true,
      target: { ...target, unavailable_reason: "browser_idle" }
    }),
    false,
    "merely opening never launches a browser"
  );
});

test("the card's still image is due at most once a minute, only while seen and focused", () => {
  const seen = { panelVisible: true, viewerOpen: false, windowActive: true, lastFetchMs: null, nowMs: 1_000 };
  assert.equal(thumbnailDue(seen), true, "first one when the panel opens");
  assert.equal(thumbnailDue({ ...seen, lastFetchMs: 1_000, nowMs: 30_000 }), false);
  assert.equal(thumbnailDue({ ...seen, lastFetchMs: 1_000, nowMs: 1_000 + THUMBNAIL_REFRESH_MS }), true);
  assert.equal(thumbnailDue({ ...seen, windowActive: false }), false, "unfocused window");
  assert.equal(thumbnailDue({ ...seen, panelVisible: false }), false, "panel closed");
  assert.equal(thumbnailDue({ ...seen, viewerOpen: true }), false, "the live viewer has its own stream");
});

// ---- finding 5: Stop stays independent ------------------------------------

test("global Stop calls the daemon override only while a takeover is held or in progress", () => {
  const watching = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  assert.equal(shouldOverrideOnStop(watching, null), false, "plain Stop");
  assert.equal(shouldOverrideOnStop(watching, dto(null)), false);
  assert.equal(shouldOverrideOnStop(owning(), dto(takeover())), true);
  assert.equal(
    shouldOverrideOnStop(watching, dto(takeover({ owner: "peer:p", id: "x" }))),
    true,
    "a live takeover (even a peer's) is stopped by the local user"
  );
  assert.equal(
    shouldOverrideOnStop(watching, dto(takeover({ state: "paused" }))),
    false,
    "already paused: nothing to latch"
  );
});

test("dismissing the viewer with a failed hand-back keeps ownership and the retry request id", () => {
  let st = applyReleaseStarted(owning(), "req-keep", "login_completed");
  st = applyReleaseFailed(st, "release_failed", "daemon unreachable");
  assert.equal(planViewerClose(st), "dismiss", "never traps the user");
  assert.equal(st.ownedTakeoverId, "to-1");
  assert.equal(st.pendingRequest?.kind, "release");
  assert.equal(buildReleasePayload(st, "ignored", null)?.request_id, "req-keep");
  assert.equal(isControlBusy(st), true, "panel keeps Bitirdim / Retry");
});

test("Stop reads the newest control snapshot, not stale local state", () => {
  const watching = applySessionCreated(
    createInitialLiveViewState(target),
    target,
    session
  );
  const stale = { ...watching, takeover: takeover({ owner: "peer:p", id: "x" }) };
  assert.equal(shouldOverrideOnStop(stale, dto(null)), false);
  assert.equal(
    shouldOverrideOnStop(watching, dto(takeover({ id: "nil", owner: "peer:p" }))),
    true,
    "a takeover only the bridge re-read is overridden"
  );
  assert.equal(shouldOverrideOnStop(stale, null), true, "fallback before any snapshot");
});

test("controlSnapshotKey ignores countdown noise but sees takeover changes", () => {
  const a = dto(takeover({ input_expires_in_ms: 10_000 }));
  const b = dto(takeover({ input_expires_in_ms: 4_000 }));
  assert.equal(controlSnapshotKey(a), controlSnapshotKey(b));
  assert.notEqual(
    controlSnapshotKey(a),
    controlSnapshotKey(dto(takeover({ state: "paused", reason: "disconnected" })))
  );
});

// ---- third review ----------------------------------------------------------

const pausedDto = (reason: TakeoverInfo["reason"], over: Partial<TakeoverInfo> = {}) =>
  dto(takeover({ state: "paused", reason, ...over }));

test("a paused owned takeover keeps a recover control with viewer, panel and Stop out of the picture", () => {
  // stream drop while owning -> paused/disconnected, then the daemon snapshot
  let st = applyStreamDisconnected(owning());
  const snapshot = pausedDto("disconnected");
  st = applyControlStateDto(st, snapshot);
  assert.equal(st.viewState, "paused");
  // needsPauseRecovery depends only on state + snapshot: not on the viewer,
  // the panel (wantSession) or the native run mode that Stop sets to idle
  const need = needsPauseRecovery(st, snapshot);
  assert.equal(need?.takeoverId, "to-1");
  assert.equal(need?.reason, "disconnected");
  assert.equal(shouldOverrideOnStop(st, snapshot), false, "Stop does not touch a pause");
  assert.ok(needsPauseRecovery(st, snapshot), "still there after Stop");

  // another actor's paused takeover (nil ids) is not ours
  const nil = "00000000-0000-0000-0000-000000000000";
  assert.equal(
    needsPauseRecovery(
      applySessionCreated(createInitialLiveViewState(target), target, session),
      pausedDto("disconnected", { id: nil, session_id: nil, owner: "peer:p" })
    ),
    null
  );

  // POST /control/recover clears the latch: no takeover, bot may run again
  const cleared = applyControlStateDto(st, dto(null));
  assert.equal(cleared.viewState, "watch");
  assert.equal(cleared.ownedTakeoverId, null);
  assert.equal(needsPauseRecovery(cleared, dto(null)), null);
});

test("a pending release is not settled by the daemon's own suspend pause (no reason)", () => {
  const pending = () =>
    applyReleaseFailed(applyReleaseStarted(owning(), "req-keep", null), "release_failed", "x");
  // suspend_release: paused with reason None, same id
  const suspended = applyControlStateDto(pending(), pausedDto(null));
  assert.equal(suspended.pendingRequest?.kind, "release", "Retry stays");
  assert.equal(suspended.lastError?.code, "release_failed");
  assert.equal(buildReleasePayload(suspended, "ignored", null)?.request_id, "req-keep");
  assert.equal(isControlBusy(suspended), true);
  // a real interruption (reason set), the id gone, or a completed journal settle it
  assert.equal(applyControlStateDto(pending(), pausedDto("disconnected")).pendingRequest, null);
  assert.equal(applyControlStateDto(pending(), dto(null)).pendingRequest, null);
  const journal = {
    ...dto(takeover()),
    release: { takeover_id: "to-1", request_id: "req-keep", state: "released" as const }
  } as ControlStateDto;
  assert.equal(applyControlStateDto(pending(), journal).pendingRequest, null);
});

test("while a release is pending the session heartbeat renews no lease", () => {
  const releasing = applyReleaseStarted(owning(), "req-1", null);
  assert.deepEqual(buildSessionHeartbeat(owning()), { takeover_id: "to-1", control_epoch: 5 });
  assert.deepEqual(buildSessionHeartbeat(releasing), {});
  assert.deepEqual(
    buildSessionHeartbeat(applyReleaseFailed(releasing, "release_failed", "x")),
    {}
  );
});

test("palette and Modal layering: the topmost layer owns Escape in either open order", () => {
  // Settings (Modal) open, then Ctrl+K palette
  let stack = createModalStack();
  const modal = stack.next();
  stack.add(modal);
  const palette = stack.next();
  stack.add(palette);
  assert.equal(stack.isTop(palette), true);
  assert.equal(stack.isTop(modal), false, "Modal's Escape handler stands down");
  stack.remove(palette);
  assert.equal(stack.isTop(modal), true, "Escape reaches the modal again");

  // palette first, then a Modal on top
  stack = createModalStack();
  const palette2 = stack.next();
  stack.add(palette2);
  const modal2 = stack.next();
  stack.add(modal2);
  assert.equal(stack.isTop(modal2), true);
  assert.equal(stack.isTop(palette2), false, "palette must not close on the modal's Escape");
});

test("an unverifiable reacquire leaves a clear-pause path; restart is a clear-pause, not a resume", () => {
  // reacquire attempt fails (503 old target reset cannot be verified)
  let st = applyControlStateDto(applyStreamDisconnected(owning()), pausedDto("disconnected"));
  st = applyTakeoverStarted(st, "req-r", { reacquire: true });
  st = applyTakeoverFailed(st, "unavailable", "old target reset cannot be verified");
  assert.equal(st.viewState, "paused", "still paused");
  assert.equal(describeRecoveryPrompt(st, null)?.mode, "reacquire");
  assert.ok(needsPauseRecovery(st, pausedDto("disconnected")), "banner + clear pause remain");

  // after a daemon restart the reason is `restart`: the action is clear-pause
  const restarted = applyControlStateDto(owning(), pausedDto("restart"));
  assert.notEqual(describeRecoveryPrompt(restarted, null)?.mode, "reacquire");
});

// ---- fourth review ----------------------------------------------------------

import { foreignTakeoverOwner, snapshotPausedTakeoverId } from "./liveViewState.ts";
import { runClearPause } from "./liveViewRelease.ts";

const NIL = "00000000-0000-0000-0000-000000000000";
const OWN = { kind: "own" as const, takeoverId: "to-1" };

test("a missed control_state_changed cannot hide the banner: the polled snapshot or the local mirror shows it", () => {
  // the snapshot ingested by the poll (key changed) shows our paused takeover
  let st = applyStreamDisconnected(owning());
  const polled = pausedDto("disconnected");
  st = applyControlStateDto(st, polled);
  assert.ok(needsPauseRecovery(st, polled));

  // the event was missed and the last snapshot still says active: the locally
  // mirrored pause alone needs the banner
  const mirrored = applyStreamDisconnected(owning());
  assert.equal(mirrored.viewState, "paused");
  assert.ok(needsPauseRecovery(mirrored, dto(takeover())), "stale active snapshot");
  assert.ok(needsPauseRecovery(mirrored, null));
  // ... until a snapshot says otherwise
  assert.equal(needsPauseRecovery(mirrored, dto(null)), null);
});

test("another actor's paused takeover: no Clear pause anywhere, owner named, watch dock", () => {
  let st = applySessionCreated(createInitialLiveViewState(target), target, session);
  const foreign = pausedDto("disconnected", {
    id: NIL,
    session_id: NIL,
    owner: "peer:phone",
    control_epoch: 0
  });
  st = applyControlStateDto(st, foreign);
  assert.equal(st.viewState, "watch", "not the paused dock");
  assert.equal(needsPauseRecovery(st, foreign), null, "banner, panel and dock hide Clear pause");
  assert.equal(describeRecoveryPrompt(st, foreign), null);
  assert.equal(foreignTakeoverOwner(st), "peer:phone");
  assert.equal(snapshotPausedTakeoverId(foreign), null);
  // our own paused takeover (real id) names no foreign owner
  const mine = applyControlStateDto(owning(), pausedDto("disconnected"));
  assert.equal(foreignTakeoverOwner(mine), null);
});

test("Clear pause re-reads state and aborts when the takeover changed within the poll window", async () => {
  const calls: string[] = [];
  const mk = (fresh: ControlStateDto | Error) => ({
    getControlState: async () => {
      calls.push("get");
      if (fresh instanceof Error) {
        throw fresh;
      }
      return fresh;
    },
    recover: async () => {
      calls.push("recover");
      return dto(null);
    },
    releaseLatch: async () => {
      calls.push("release");
      return dto(null);
    }
  });
  // a peer took active control since the banner was drawn
  const stale = await runClearPause(mk(dto(takeover({ id: NIL, owner: "peer:p" }))), OWN);
  assert.equal(stale.kind, "aborted");
  assert.deepEqual(calls, ["get"], "recover never posted");

  // a different paused takeover
  calls.length = 0;
  const other = await runClearPause(mk(pausedDto("disconnected", { id: "to-9" })), OWN);
  assert.equal(other.kind, "aborted");
  assert.ok(!calls.includes("recover"));

  // unreadable state: fail closed
  calls.length = 0;
  const down = await runClearPause(mk(new Error("down")), OWN);
  assert.equal(down.kind, "failed");
  assert.ok(!calls.includes("recover"));

  // same takeover still paused: cleared
  calls.length = 0;
  const ok = await runClearPause(mk(pausedDto("disconnected")), OWN);
  assert.equal(ok.kind, "cleared");
  assert.deepEqual(calls, ["get", "recover"]);
});

test("heartbeat keeps the lease while the pending release's takeover is still active, drops it once paused or gone", () => {
  const releasing = applyReleaseStarted(owning(), "req-1", null);
  const active = applyControlStateDto(releasing, dto(takeover()));
  assert.deepEqual(buildSessionHeartbeat(active), {
    takeover_id: "to-1",
    control_epoch: 5
  });
  // POST not yet applied, no snapshot yet: state.takeover unknown -> still no id
  // (nothing to renew against); the first snapshot decides
  const suspended = applyControlStateDto(releasing, pausedDto(null));
  assert.deepEqual(buildSessionHeartbeat(suspended), {}, "suspend pause");
  const gone = applyControlStateDto(releasing, dto(null));
  assert.deepEqual(buildSessionHeartbeat(gone), {}, "gone");
});

test("Expired during a pending release keeps a visible way forward", () => {
  const pending = applyReleaseFailed(
    applyReleaseStarted(owning(), "req-1", null),
    "release_failed",
    "lost"
  );
  const expired = applyControlStateDto(pending, pausedDto("expired"));
  assert.equal(expired.pendingRequest, null, "a dead retry is dropped...");
  assert.equal(expired.lastError?.code, "release_interrupted", "...but not silently");
  assert.ok(needsPauseRecovery(expired, pausedDto("expired")), "Clear pause stays offered");
});

test("the connection-lost string does not claim the bot is paused; the paused dock keeps the long sentence", async () => {
  const { readFileSync } = await import("node:fs");
  const { fileURLToPath } = await import("node:url");
  for (const l of ["en", "tr"]) {
    const j = JSON.parse(
      readFileSync(fileURLToPath(new URL(`../i18n/locales/${l}.json`, import.meta.url)), "utf8")
    );
    assert.doesNotMatch(j.liveView.connectionLost, /paus|duraklat/i, l);
    assert.match(j.liveView.reasons.disconnected, /paus|duraklat/i, l);
  }
});

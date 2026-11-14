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
  createInitialLiveViewState,
  isControlBusy,
  planViewerClose,
  reconnectDelayMs,
  releaseConfirmedBy,
  shouldOverrideOnStop,
  type LiveViewControllerState
} from "./liveViewState.ts";
import { applyReleaseConfirmed } from "./liveViewState.ts";
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
    "wait"
  );
});

test("panel and viewer closed (or window hidden) stop the capture session", () => {
  const base = {
    hasClient: true,
    documentHidden: false,
    target,
    viewerOpen: false,
    panelActive: false
  };
  assert.equal(computeWantSession(base), false);
  assert.equal(computeWantSession({ ...base, panelActive: true }), true);
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

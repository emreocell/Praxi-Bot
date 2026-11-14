import assert from "node:assert/strict";
import { test } from "node:test";
import type { ControlStateDto, TargetInfo } from "../api/types.ts";
import {
  applyControlStateDto,
  applyFrameRendered,
  applyInputAck,
  applyRateLimited,
  applyReleaseConfirmed,
  applyReleaseFailed,
  applyReleaseStarted,
  applySessionCreated,
  applyStatusRecord,
  applyStreamDisconnected,
  applyTakeoverAccepted,
  applyTakeoverStarted,
  buildReleasePayload,
  buildSessionHeartbeat,
  buildTakeoverRequest,
  canSendLiveInput,
  createInitialLiveViewState,
  describeRecoveryPrompt,
  pickDefaultTarget,
  summarizeTarget
} from "./liveViewState.ts";
import type { FrameHeader, StatusHeader } from "./liveViewWire.ts";

const targetBrowser: TargetInfo = {
  target_id: "t-browser",
  bot_id: "bot-1",
  run_id: "run-1",
  host_id: "local",
  kind: "browser",
  generation: 2,
  capabilities: {
    codecs: ["jpeg"],
    view: true,
    input: true,
    reason: null
  },
  unavailable_reason: null
};

const targetIdle: TargetInfo = {
  ...targetBrowser,
  target_id: "t-idle",
  bot_id: "bot-2",
  run_id: null,
  unavailable_reason: "browser_idle"
};

const targetRemote: TargetInfo = {
  target_id: "t-remote",
  bot_id: null,
  run_id: null,
  host_id: "peer:mac",
  kind: "desktop",
  generation: 1,
  capabilities: {
    codecs: [],
    view: false,
    input: false,
    reason: "remote_node_live_view"
  },
  unavailable_reason: "remote_node_live_view"
};

const sessionFixture = {
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

const frameFixture: FrameHeader = {
  v: 1,
  type: "frame",
  session_id: "sess-1",
  sent_at_ms: 10_010,
  target_id: "t-browser",
  generation: 2,
  frame_seq: 1,
  captured_at_ms: 10_005,
  cause: "refresh",
  action_seq: null,
  codec: "jpeg",
  width: 1280,
  height: 800,
  source_rect: { x: 0, y: 0, width: 1280, height: 800, units: "physical_px" },
  geometry_revision: 7,
  repeated: false
};

const statusActiveFixture: StatusHeader = {
  v: 1,
  type: "status",
  session_id: "sess-1",
  state: "active",
  reason: null,
  effective_profile: { ...sessionFixture.effective_profile, profile: "live" },
  control_epoch: 5,
  input_allowed: true
};

test("pickDefaultTarget and summarizeTarget expose unavailable reasons without auto-launch", () => {
  assert.equal(summarizeTarget(targetIdle).reason, "browser_idle");
  assert.equal(summarizeTarget(targetIdle).available, false);
  assert.equal(summarizeTarget(targetRemote).reason, "remote_node_live_view");
  assert.equal(
    pickDefaultTarget([targetIdle, targetRemote, targetBrowser], { botId: "bot-2" })?.target_id,
    "t-idle"
  );
  assert.equal(
    pickDefaultTarget([targetIdle, targetBrowser])?.target_id,
    "t-browser"
  );
});

test("input gate stays locked during watch and acquiring, unlocks only for active owner", () => {
  let st = createInitialLiveViewState(targetBrowser);
  st = applySessionCreated(st, targetBrowser, sessionFixture);
  assert.equal(canSendLiveInput(st, 10_000).ok, false);

  st = applyFrameRendered(st, frameFixture, 10_010);
  assert.equal(canSendLiveInput(st, 10_020).ok, false, "watch disallows input");

  st = applyTakeoverStarted(st, "req-1");
  assert.equal(canSendLiveInput(st, 10_025).ok, false, "acquiring disallows input");

  st = applyTakeoverAccepted(st, {
    takeover_id: "to-1",
    control_epoch: 5,
    state: "acquiring"
  });
  assert.deepEqual(buildSessionHeartbeat(st), {
    takeover_id: "to-1",
    control_epoch: 5
  });
  assert.equal(canSendLiveInput(st, 10_030).ok, false, "still acquiring");

  st = applyStatusRecord(st, statusActiveFixture, 10_040);
  const gate = canSendLiveInput(st, 10_050);
  assert.equal(gate.ok, true);
  if (gate.ok) {
    assert.equal(gate.controlEpoch, 5);
    assert.equal(gate.geometryRevision, 7);
    assert.equal(gate.generation, 2);
  }

  // Different takeover owner is rejected even if status arrived earlier
  const stolenDto: ControlStateDto = {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: "human_driving",
    mode: "human_driving",
    release: null,
    takeover: {
      id: "to-other",
      control_epoch: 9,
      input_expires_in_ms: 10_000,
      owner: "peer:mobile",
      reason: null,
      session_id: "sess-9",
      state: "active",
      target_id: "t-browser"
    }
  };
  const nonOwner = applyControlStateDto(st, stolenDto);
  assert.equal(canSendLiveInput(nonOwner, 10_060).ok, false);
});

test("Bitirdim release preserves idempotent request_id on retry and never optimistically clears owner", () => {
  let st = applyTakeoverAccepted(
    applyFrameRendered(
      applySessionCreated(
        createInitialLiveViewState(targetBrowser),
        targetBrowser,
        sessionFixture
      ),
      frameFixture,
      1000
    ),
    { takeover_id: "to-1", control_epoch: 5, state: "active" }
  );
  st = applyStatusRecord(st, statusActiveFixture, 1010);
  st = applyInputAck(st, 3, "active");
  assert.equal(st.nextInputSeq, 4);

  const req = buildReleasePayload(st, "req-rel-99", "login_completed");
  assert.deepEqual(req, {
    takeover_id: "to-1",
    control_epoch: 5,
    request_id: "req-rel-99",
    outcome: "completed",
    human_action: "login_completed"
  });

  st = applyReleaseStarted(st, "req-rel-99", "login_completed");
  assert.equal(canSendLiveInput(st, 1020).ok, false, "releasing disables input");
  st = applyReleaseFailed(st, "network", "timeout");
  assert.equal(st.ownedTakeoverId, "to-1", "failed release keeps ownership until acked");
  const retried = buildReleasePayload(st, "req-new-ignored", null);
  assert.equal(retried?.request_id, "req-rel-99", "retry reuses original request_id");

  st = applyReleaseConfirmed(st, {
    app_connected: true,
    expires_in_ms: 15_000,
    latched: null,
    mode: "idle",
    release: { takeover_id: "to-1", request_id: "req-rel-99", state: "released" },
    takeover: null
  });
  assert.equal(st.ownedTakeoverId, null);
  assert.equal(st.pendingRequest, null);
  assert.deepEqual(buildSessionHeartbeat(st), {});
});

test("disconnect and rate limit block input and interrupted pause requires explicit recovery", () => {
  let st = applyStatusRecord(
    applyTakeoverAccepted(
      applyFrameRendered(
        applySessionCreated(
          createInitialLiveViewState(targetBrowser),
          targetBrowser,
          sessionFixture
        ),
        frameFixture,
        1000
      ),
      { takeover_id: "to-1", control_epoch: 5, state: "active" }
    ),
    statusActiveFixture,
    1010
  );

  st = applyRateLimited(st, 2000, 1500);
  assert.equal(canSendLiveInput(st, 2500).ok, false);

  st = applyStatusRecord(
    st,
    {
      ...statusActiveFixture,
      state: "paused",
      reason: "disconnected",
      input_allowed: false
    },
    3000
  );
  st = applyStreamDisconnected(st);
  assert.equal(st.viewState, "paused");
  assert.equal(canSendLiveInput(st, 3100).ok, false);

  const prompt = describeRecoveryPrompt(st, null);
  assert.equal(prompt?.mode, "reacquire");
  const req = buildTakeoverRequest(st, "req-rec-1", {
    reacquireTakeoverId: prompt?.takeoverId
  });
  assert.equal(req?.reacquire_takeover_id, "to-1");
});

import type {
  ControlStateDto,
  EffectiveProfile,
  HumanAction,
  RecoveryReason,
  ReleaseRequest,
  SessionInfo,
  TakeoverAccepted,
  TakeoverInfo,
  TakeoverRequest,
  TargetInfo,
  ViewHeartbeat,
  ViewReason,
  ViewState
} from "../api/types.ts";
import type { FrameHeader, StatusHeader } from "./liveViewWire.ts";

export interface TargetMetadataSummary {
  id: string;
  hostId: string;
  kind: TargetInfo["kind"];
  botId: string | null;
  runId: string | null;
  available: boolean;
  canView: boolean;
  canInput: boolean;
  reason: ViewReason | null;
}

export function summarizeTarget(target: TargetInfo): TargetMetadataSummary {
  const canView =
    target.capabilities.view && target.capabilities.codecs.length > 0;
  const available = canView && target.unavailable_reason == null;
  return {
    id: target.target_id,
    hostId: target.host_id,
    kind: target.kind,
    botId: target.bot_id ?? null,
    runId: target.run_id ?? null,
    available,
    canView,
    canInput: available && target.capabilities.input,
    reason: target.unavailable_reason ?? (canView ? null : "unsupported")
  };
}

/**
 * Pick a target deterministically without launching any browser or side effect.
 */
export function pickDefaultTarget(
  targets: readonly TargetInfo[],
  opts: { preferredTargetId?: string | null; botId?: string | null } = {}
): TargetInfo | null {
  if (targets.length === 0) {
    return null;
  }
  if (opts.preferredTargetId) {
    const exact = targets.find((t) => t.target_id === opts.preferredTargetId);
    if (exact) {
      return exact;
    }
  }
  if (opts.botId) {
    const botAvailable = targets.find(
      (t) => t.bot_id === opts.botId && summarizeTarget(t).available
    );
    if (botAvailable) {
      return botAvailable;
    }
    const botAny = targets.find((t) => t.bot_id === opts.botId);
    if (botAny) {
      return botAny;
    }
  }
  return targets.find((t) => summarizeTarget(t).available) ?? targets[0];
}

export interface RenderedFrameSnapshot {
  generation: number;
  geometryRevision: number;
  frameSeq: number;
  capturedAtMs: number;
  width: number;
  height: number;
  sourceRect: FrameHeader["source_rect"];
  cause: FrameHeader["cause"];
  repeated: boolean;
}

export type PendingControlRequest =
  | {
      kind: "takeover";
      requestId: string;
      reacquire: boolean;
      recover: boolean;
    }
  | {
      kind: "release";
      requestId: string;
      takeoverId: string;
      controlEpoch: number;
      humanAction: HumanAction | null;
    };

export interface RecoveryPrompt {
  mode: "reacquire" | "recover" | "clear_pause";
  reason: ViewReason;
  recoveryReason?: RecoveryReason;
  takeoverId?: string;
}

export interface LiveViewControllerState {
  target: TargetInfo | null;
  session: SessionInfo | null;
  viewState: ViewState;
  reason: ViewReason | null;
  effectiveProfile: EffectiveProfile | null;
  inputAllowed: boolean;
  ownedTakeoverId: string | null;
  ownedControlEpoch: number | null;
  takeover: TakeoverInfo | null;
  pendingRequest: PendingControlRequest | null;
  nextInputSeq: number;
  disconnected: boolean;
  streamStale: boolean;
  needsResync: boolean;
  rateLimitedUntilMs: number | null;
  latestFrame: RenderedFrameSnapshot | null;
  lastActivityAtMs: number | null;
  lastError: { code: string; message: string } | null;
}

export function createInitialLiveViewState(
  target: TargetInfo | null = null
): LiveViewControllerState {
  const unavailableReason = target?.unavailable_reason ?? null;
  return {
    target,
    session: null,
    viewState: unavailableReason ? "unavailable" : "watch",
    reason: unavailableReason,
    effectiveProfile: null,
    inputAllowed: false,
    ownedTakeoverId: null,
    ownedControlEpoch: null,
    takeover: null,
    pendingRequest: null,
    nextInputSeq: 1,
    disconnected: false,
    streamStale: false,
    needsResync: false,
    rateLimitedUntilMs: null,
    latestFrame: null,
    lastActivityAtMs: null,
    lastError: null
  };
}

export function applySessionCreated(
  state: LiveViewControllerState,
  target: TargetInfo,
  session: SessionInfo
): LiveViewControllerState {
  const sameTarget = state.target?.target_id === target.target_id;
  return {
    ...state,
    target,
    session,
    viewState: target.unavailable_reason ? "unavailable" : "watch",
    reason: target.unavailable_reason ?? null,
    effectiveProfile: session.effective_profile,
    inputAllowed: false,
    ownedTakeoverId: sameTarget ? state.ownedTakeoverId : null,
    ownedControlEpoch: sameTarget ? state.ownedControlEpoch : null,
    nextInputSeq: 1,
    disconnected: false,
    streamStale: false,
    needsResync: false,
    latestFrame: sameTarget ? state.latestFrame : null,
    lastError: null
  };
}

export function applyTakeoverStarted(
  state: LiveViewControllerState,
  requestId: string,
  opts: { reacquire?: boolean; recover?: boolean } = {}
): LiveViewControllerState {
  return {
    ...state,
    pendingRequest: {
      kind: "takeover",
      requestId,
      reacquire: Boolean(opts.reacquire),
      recover: Boolean(opts.recover)
    },
    lastError: null
  };
}

export function applyTakeoverAccepted(
  state: LiveViewControllerState,
  accepted: TakeoverAccepted
): LiveViewControllerState {
  const isNewActivation = state.ownedTakeoverId !== accepted.takeover_id ||
    state.ownedControlEpoch !== accepted.control_epoch;
  return {
    ...state,
    ownedTakeoverId: accepted.takeover_id,
    ownedControlEpoch: accepted.control_epoch,
    viewState: accepted.state,
    inputAllowed: accepted.state === "active",
    pendingRequest:
      state.pendingRequest?.kind === "takeover" ? null : state.pendingRequest,
    nextInputSeq: isNewActivation ? 1 : state.nextInputSeq,
    lastError: null
  };
}

export function applyTakeoverFailed(
  state: LiveViewControllerState,
  code: string,
  message: string
): LiveViewControllerState {
  return {
    ...state,
    pendingRequest:
      state.pendingRequest?.kind === "takeover" ? null : state.pendingRequest,
    lastError: { code, message }
  };
}

export function applyReleaseStarted(
  state: LiveViewControllerState,
  requestId: string,
  humanAction: HumanAction | null = null
): LiveViewControllerState {
  const takeoverId = state.takeover?.id ?? state.ownedTakeoverId;
  const controlEpoch =
    state.takeover?.control_epoch ?? state.ownedControlEpoch;
  if (!takeoverId || controlEpoch === null) {
    return state;
  }
  return {
    ...state,
    inputAllowed: false,
    pendingRequest: {
      kind: "release",
      requestId,
      takeoverId,
      controlEpoch,
      humanAction
    },
    lastError: null
  };
}

/** Finish "Bitirdim" release only on authoritative daemon acknowledgment. */
export function applyReleaseConfirmed(
  state: LiveViewControllerState,
  dto: ControlStateDto
): LiveViewControllerState {
  const next = applyControlStateDto(state, dto);
  return {
    ...next,
    ownedTakeoverId: null,
    ownedControlEpoch: null,
    inputAllowed: false,
    pendingRequest: null,
    nextInputSeq: 1,
    rateLimitedUntilMs: null,
    lastError: null
  };
}

export function applyReleaseFailed(
  state: LiveViewControllerState,
  code: string,
  message: string
): LiveViewControllerState {
  // Keep `pendingRequest` (with its idempotent `requestId`) so user can retry cleanly!
  return {
    ...state,
    inputAllowed: false,
    lastError: { code, message }
  };
}

export function applyStatusRecord(
  state: LiveViewControllerState,
  status: StatusHeader,
  nowMs: number
): LiveViewControllerState {
  if (state.session && status.session_id !== state.session.session_id) {
    return state;
  }
  const ownerMatches =
    state.ownedTakeoverId !== null &&
    (state.takeover === null || state.takeover.id === state.ownedTakeoverId);
  const inputAllowed =
    status.state === "active" &&
    status.input_allowed &&
    ownerMatches &&
    state.pendingRequest?.kind !== "release";
  return {
    ...state,
    viewState: status.state,
    reason: status.reason ?? null,
    effectiveProfile: status.effective_profile,
    inputAllowed,
    ownedControlEpoch:
      status.control_epoch > 0 && ownerMatches
        ? status.control_epoch
        : state.ownedControlEpoch,
    disconnected: false,
    streamStale: false,
    lastActivityAtMs: nowMs
  };
}

export function applyFrameRendered(
  state: LiveViewControllerState,
  header: FrameHeader,
  nowMs: number
): LiveViewControllerState {
  if (state.session && header.session_id !== state.session.session_id) {
    return state;
  }
  return {
    ...state,
    latestFrame: {
      generation: header.generation,
      geometryRevision: header.geometry_revision,
      frameSeq: header.frame_seq,
      capturedAtMs: header.captured_at_ms,
      width: header.width,
      height: header.height,
      sourceRect: header.source_rect,
      cause: header.cause,
      repeated: header.repeated
    },
    needsResync: false,
    disconnected: false,
    streamStale: false,
    lastActivityAtMs: nowMs
  };
}

export function applyStreamActivity(
  state: LiveViewControllerState,
  nowMs: number
): LiveViewControllerState {
  return {
    ...state,
    disconnected: false,
    streamStale: false,
    lastActivityAtMs: nowMs
  };
}

export function applyControlStateDto(
  state: LiveViewControllerState,
  dto: ControlStateDto
): LiveViewControllerState {
  const t = dto.takeover ?? null;
  if (!t) {
    const viewState: ViewState =
      state.target?.unavailable_reason
        ? "unavailable"
        : state.viewState === "closed"
          ? "closed"
          : "watch";
    const reason =
      dto.latched === "paused" && viewState === "watch"
        ? state.reason
        : state.target?.unavailable_reason ?? null;
    return {
      ...state,
      takeover: null,
      ownedTakeoverId: null,
      ownedControlEpoch: null,
      viewState,
      reason,
      inputAllowed: false
    };
  }
  const matchesTarget =
    !state.target || t.target_id === state.target.target_id;
  if (!matchesTarget) {
    // Another target has a takeover while we're watching a different target.
    return {
      ...state,
      takeover: t,
      inputAllowed: false
    };
  }
  const weOwnIt =
    state.ownedTakeoverId !== null &&
    t.id === state.ownedTakeoverId &&
    (!state.session || t.session_id === state.session.session_id);
  const inputAllowed =
    weOwnIt &&
    t.state === "active" &&
    state.pendingRequest?.kind !== "release";
  return {
    ...state,
    takeover: t,
    ownedControlEpoch: weOwnIt ? t.control_epoch : state.ownedControlEpoch,
    viewState: t.state,
    reason: t.reason ?? null,
    inputAllowed
  };
}

/**
 * Handle frame stream disconnection. Preserves paused / takeover state and
 * disables interactive input immediately; never auto-resumes on reconnect.
 */
export function applyStreamDisconnected(
  state: LiveViewControllerState
): LiveViewControllerState {
  return {
    ...state,
    disconnected: true,
    inputAllowed: false
  };
}

export function applyStreamStalenessCheck(
  state: LiveViewControllerState,
  nowMs: number,
  staleThresholdMs = 10_000
): LiveViewControllerState {
  if (
    state.lastActivityAtMs !== null &&
    nowMs - state.lastActivityAtMs > staleThresholdMs
  ) {
    return {
      ...state,
      streamStale: true,
      inputAllowed: false
    };
  }
  return state;
}

export function applyRateLimited(
  state: LiveViewControllerState,
  nowMs: number,
  cooldownMs = 1500
): LiveViewControllerState {
  return {
    ...state,
    rateLimitedUntilMs: nowMs + cooldownMs,
    inputAllowed: false
  };
}

export function applyResyncRequired(
  state: LiveViewControllerState
): LiveViewControllerState {
  return {
    ...state,
    needsResync: true
  };
}

export function applyInputAck(
  state: LiveViewControllerState,
  sentCount: number,
  resultState: ViewState
): LiveViewControllerState {
  return {
    ...state,
    nextInputSeq: state.nextInputSeq + Math.max(0, sentCount),
    viewState: resultState,
    inputAllowed:
      resultState === "active" && state.pendingRequest?.kind !== "release"
  };
}

export function isActivelyOwningTakeover(
  state: LiveViewControllerState
): boolean {
  if (!state.ownedTakeoverId || state.ownedControlEpoch === null) {
    return false;
  }
  if (
    state.takeover &&
    state.takeover.id !== state.ownedTakeoverId
  ) {
    return false;
  }
  return state.viewState === "acquiring" || state.viewState === "active";
}

/**
 * Build heartbeat body for `PUT /v1/view/sessions/{id}/heartbeat`.
 * Only echoes `{ takeover_id, control_epoch }` while actively owning.
 */
export function buildSessionHeartbeat(
  state: LiveViewControllerState
): ViewHeartbeat {
  if (isActivelyOwningTakeover(state) && state.ownedTakeoverId !== null) {
    return {
      takeover_id: state.ownedTakeoverId,
      control_epoch: state.ownedControlEpoch
    };
  }
  return {};
}

export function buildReleasePayload(
  state: LiveViewControllerState,
  requestId: string,
  humanAction?: HumanAction | null
): ReleaseRequest | null {
  if (state.pendingRequest?.kind === "release") {
    return {
      takeover_id: state.pendingRequest.takeoverId,
      control_epoch: state.pendingRequest.controlEpoch,
      request_id: state.pendingRequest.requestId,
      outcome: "completed",
      human_action: state.pendingRequest.humanAction ?? null
    };
  }
  const takeoverId = state.takeover?.id ?? state.ownedTakeoverId;
  const controlEpoch =
    state.takeover?.control_epoch ?? state.ownedControlEpoch;
  if (!takeoverId || controlEpoch === null) {
    return null;
  }
  return {
    takeover_id: takeoverId,
    control_epoch: controlEpoch,
    request_id: requestId,
    outcome: "completed",
    human_action: humanAction ?? null
  };
}

export function buildTakeoverRequest(
  state: LiveViewControllerState,
  requestId: string,
  opts: {
    reacquireTakeoverId?: string | null;
    recover?: boolean;
    reason?: RecoveryReason | null;
  } = {}
): TakeoverRequest | null {
  if (!state.session) {
    return null;
  }
  const req: TakeoverRequest = {
    session_id: state.session.session_id,
    request_id: requestId
  };
  if (opts.reacquireTakeoverId) {
    req.reacquire_takeover_id = opts.reacquireTakeoverId;
  }
  if (opts.recover) {
    req.recover = true;
    req.reason = opts.reason ?? "owner_lost";
  }
  return req;
}

export type InputGateResult =
  | {
      ok: true;
      sessionId: string;
      takeoverId: string;
      controlEpoch: number;
      generation: number;
      geometryRevision: number;
    }
  | { ok: false; blockReason: string };

/**
 * Authoritative guard before dispatching any interactive input batch.
 */
export function canSendLiveInput(
  state: LiveViewControllerState,
  nowMs: number
): InputGateResult {
  if (state.disconnected) {
    return { ok: false, blockReason: "disconnected" };
  }
  if (state.streamStale) {
    return { ok: false, blockReason: "stream_stale" };
  }
  if (!state.session || !state.target) {
    return { ok: false, blockReason: "no_session" };
  }
  if (
    !state.target.capabilities.input ||
    !state.session.input_capabilities
  ) {
    return { ok: false, blockReason: "input_unsupported" };
  }
  if (state.pendingRequest?.kind === "release") {
    return { ok: false, blockReason: "releasing" };
  }
  if (state.pendingRequest?.kind === "takeover") {
    return { ok: false, blockReason: "acquiring" };
  }
  if (state.viewState !== "active" || !state.inputAllowed) {
    return { ok: false, blockReason: `state_${state.viewState}` };
  }
  if (!state.ownedTakeoverId || state.ownedControlEpoch === null) {
    return { ok: false, blockReason: "not_owner" };
  }
  if (state.takeover && state.takeover.id !== state.ownedTakeoverId) {
    return { ok: false, blockReason: "different_owner" };
  }
  if (state.needsResync) {
    return { ok: false, blockReason: "resync_required" };
  }
  if (state.rateLimitedUntilMs !== null && nowMs < state.rateLimitedUntilMs) {
    return { ok: false, blockReason: "rate_limited" };
  }
  if (!state.latestFrame) {
    return { ok: false, blockReason: "no_frame" };
  }
  if (state.latestFrame.generation !== state.session.generation) {
    return { ok: false, blockReason: "generation_mismatch" };
  }
  return {
    ok: true,
    sessionId: state.session.session_id,
    takeoverId: state.ownedTakeoverId,
    controlEpoch: state.ownedControlEpoch,
    generation: state.latestFrame.generation,
    geometryRevision: state.latestFrame.geometryRevision
  };
}

/** Derive recovery/reacquire affordance when paused or interrupted. */
export function describeRecoveryPrompt(
  state: LiveViewControllerState,
  controlDto: ControlStateDto | null
): RecoveryPrompt | null {
  if (state.viewState !== "paused") {
    return null;
  }
  const reason: ViewReason = state.reason ?? state.takeover?.reason ?? "disconnected";
  const t = state.takeover ?? controlDto?.takeover ?? null;
  const ownedId = state.ownedTakeoverId;
  const matchesOwned = Boolean(ownedId && (!t || t.id === ownedId));
  if (matchesOwned && ownedId && reason === "disconnected") {
    return {
      mode: "reacquire",
      reason,
      takeoverId: ownedId
    };
  }
  const mappedRecovery: RecoveryReason =
    reason === "revoked"
      ? "device_revoked"
      : reason === "restart"
        ? "daemon_restart"
        : "owner_lost";
  return {
    mode:
      reason === "local_override" ? "clear_pause" : "recover",
    reason,
    recoveryReason: mappedRecovery,
    takeoverId: t?.id
  };
}

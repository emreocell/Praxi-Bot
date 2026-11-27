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
import { type BotCursorPose, parseBotPointer } from "./botCursor.ts";
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
 *
 * A browser belongs to one bot, so automatic selection never lands on ANOTHER
 * bot's browser: with a focused bot it considers that bot's own targets and the
 * shared (bot-less) desktop; without one, only the shared desktop. Showing a
 * different bot's browser takes an explicit `preferredTargetId`.
 */
export function pickDefaultTarget(
  targets: readonly TargetInfo[],
  opts: { preferredTargetId?: string | null; botId?: string | null; preferDesktop?: boolean } = {}
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
  const own = opts.botId
    ? targets.filter((t) => t.bot_id === opts.botId)
    : [];
  const shared = targets.filter((t) => !t.bot_id);
  const available = (t: TargetInfo) => summarizeTarget(t).available;
  const desktop = opts.preferDesktop ? shared.find((t) => t.kind === "desktop" && available(t)) : null;
  return (
    desktop ??
    own.find(available) ??
    own[0] ??
    shared.find(available) ??
    shared[0] ??
    null
  );
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
  /** Where the bot's own pointer is on this frame; null when it has none here. */
  botPointer: BotCursorPose | null;
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
      /** When the hand-back started: bounds the lease renewal of a release that never reached the daemon. */
      startedAtMs: number;
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
  /** Effective gate for forwarding input; only the status stream may raise it. */
  inputAllowed: boolean;
  /** Last `input_allowed` the daemon reported on this session's status stream. */
  statusInputAllowed: boolean;
  /** Request id kept after a takeover whose response may have been lost (idempotent retry). */
  retryTakeoverRequestId: string | null;
  /** Takeover this client already handed back; never re-adopted from a stale snapshot. */
  lastReleasedTakeoverId: string | null;
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
    statusInputAllowed: false,
    retryTakeoverRequestId: null,
    lastReleasedTakeoverId: null,
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
  // A paused takeover of ours survives a new session: the user must reacquire
  // (or recover) it; "Watching + Take control" would only 409.
  const keepPaused =
    sameTarget &&
    state.ownedTakeoverId !== null &&
    state.viewState === "paused" &&
    !target.unavailable_reason;
  return {
    ...state,
    target,
    session,
    viewState: target.unavailable_reason
      ? "unavailable"
      : keepPaused
        ? "paused"
        : "watch",
    reason: target.unavailable_reason ?? (keepPaused ? state.reason : null),
    effectiveProfile: session.effective_profile,
    inputAllowed: false,
    statusInputAllowed: false,
    retryTakeoverRequestId: null,
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
    // Only the status stream may raise input; the response just records ownership.
    inputAllowed: accepted.state === "active" && state.statusInputAllowed,
    pendingRequest:
      state.pendingRequest?.kind === "takeover" ? null : state.pendingRequest,
    retryTakeoverRequestId: null,
    nextInputSeq: isNewActivation ? 1 : state.nextInputSeq,
    lastError: null
  };
}

export function applyTakeoverFailed(
  state: LiveViewControllerState,
  code: string,
  message: string,
  opts: { retainRequestId?: boolean } = {}
): LiveViewControllerState {
  const pending =
    state.pendingRequest?.kind === "takeover" ? state.pendingRequest : null;
  return {
    ...state,
    pendingRequest: pending ? null : state.pendingRequest,
    // A lost response may still have created the takeover: the retry must reuse the id.
    retryTakeoverRequestId:
      pending && opts.retainRequestId ? pending.requestId : null,
    lastError: { code, message }
  };
}

/** Id and epoch of the takeover this client owns (never a peer's). */
function ownedTakeoverRef(
  state: LiveViewControllerState
): { takeoverId: string; controlEpoch: number } | null {
  const takeoverId = state.ownedTakeoverId;
  if (!takeoverId) {
    return null;
  }
  const live = state.takeover?.id === takeoverId ? state.takeover : null;
  const controlEpoch = live?.control_epoch ?? state.ownedControlEpoch;
  return controlEpoch === null ? null : { takeoverId, controlEpoch };
}

export function applyReleaseStarted(
  state: LiveViewControllerState,
  requestId: string,
  humanAction: HumanAction | null = null,
  nowMs: number = Date.now()
): LiveViewControllerState {
  const owned = ownedTakeoverRef(state);
  if (!owned) {
    return state;
  }
  const { takeoverId, controlEpoch } = owned;
  return {
    ...state,
    inputAllowed: false,
    pendingRequest: {
      kind: "release",
      requestId,
      takeoverId,
      controlEpoch,
      humanAction,
      startedAtMs: nowMs
    },
    lastError: null
  };
}

/** Finish "Bitirdim" release only on authoritative daemon acknowledgment. */
export function applyReleaseConfirmed(
  state: LiveViewControllerState,
  dto: ControlStateDto
): LiveViewControllerState {
  const releasedId =
    state.pendingRequest?.kind === "release"
      ? state.pendingRequest.takeoverId
      : state.ownedTakeoverId;
  const next = applyControlStateDto(state, dto);
  return {
    ...next,
    lastReleasedTakeoverId: releasedId ?? next.lastReleasedTakeoverId,
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
  // A watch status of a session that does not own the (paused) takeover must not
  // erase the pause: only fresh control state can end it.
  if (
    status.state === "watch" &&
    state.ownedTakeoverId !== null &&
    state.viewState === "paused"
  ) {
    return {
      ...state,
      effectiveProfile: status.effective_profile,
      statusInputAllowed: false,
      inputAllowed: false,
      disconnected: false,
      streamStale: false,
      lastActivityAtMs: nowMs
    };
  }
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
    statusInputAllowed: status.input_allowed,
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
      repeated: header.repeated,
      botPointer: parseBotPointer((header as { bot_pointer?: unknown }).bot_pointer)
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
  const pendingRelease =
    state.pendingRequest?.kind === "release" ? state.pendingRequest : null;
  // A pending hand-back is settled by authoritative state: the takeover is gone,
  // someone else's, or no longer active/acquiring (lapsed lease, Stop, restart).
  const takeoverGone =
    pendingRelease !== null && (t === null || t.id !== pendingRelease.takeoverId);
  // Settled only by a real end: the takeover is gone, the daemon's release
  // journal completed, or the takeover was interrupted for a REASON (lapsed
  // lease, Stop, restart). A pause without a reason is the daemon's own
  // `suspend_release` while a hand-back is in progress or failed: the retry
  // (same request id) must stay available.
  const journalDone =
    pendingRelease !== null &&
    dto.release?.request_id === pendingRelease.requestId &&
    (dto.release?.state as string) === "released";
  const interrupted =
    t !== null && t.state === "paused" && (t.reason ?? null) !== null;
  const releaseSettled =
    pendingRelease !== null && (takeoverGone || journalDone || interrupted);
  const settled = {
    pendingRequest: releaseSettled ? null : state.pendingRequest,
    // A hand-back overtaken by an interruption (expired lease, Stop, restart) did
    // not complete: say so instead of silently dropping Retry.
    lastError: releaseSettled
      ? interrupted && !takeoverGone && !journalDone
        ? {
            code: "release_interrupted",
            message: "takeover interrupted before the hand-back finished"
          }
        : null
      : state.lastError,
    lastReleasedTakeoverId:
      takeoverGone && pendingRelease
        ? pendingRelease.takeoverId
        : state.lastReleasedTakeoverId
  };
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
      ...settled,
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
      ...settled,
      takeover: t,
      inputAllowed: false
    };
  }
  const sameSession =
    state.session !== null && t.session_id === state.session.session_id;
  // A takeover created by this very session whose response never arrived is ours:
  // adopt it instead of showing "controlled by <ourselves>".
  const adopt =
    state.ownedTakeoverId === null &&
    sameSession &&
    (t.state === "active" || t.state === "acquiring") &&
    t.id !== settled.lastReleasedTakeoverId &&
    settled.pendingRequest?.kind !== "release";
  const ownedId = adopt ? t.id : state.ownedTakeoverId;
  const weOwnIt =
    ownedId !== null &&
    t.id === ownedId &&
    (!state.session || t.session_id === state.session.session_id);
  // Someone else's live takeover is a watch state for us, never "you are in control".
  const ours = ownedId === t.id || (t.id !== NIL_UUID && t.id !== "");
  const foreignLive =
    !weOwnIt &&
    (t.state === "active" ||
      t.state === "acquiring" ||
      // another actor's paused takeover (redacted nil id) is theirs to clear
      (t.state === "paused" && !ours));
  // The snapshot may only lower input access; the status stream raises it.
  const inputAllowed =
    weOwnIt &&
    t.state === "active" &&
    settled.pendingRequest?.kind !== "release" &&
    (adopt ? state.statusInputAllowed : state.inputAllowed);
  return {
    ...state,
    ...settled,
    takeover: t,
    ownedTakeoverId: ownedId,
    ownedControlEpoch:
      weOwnIt || adopt ? t.control_epoch : state.ownedControlEpoch,
    viewState: foreignLive ? "watch" : t.state,
    reason: foreignLive ? null : t.reason ?? null,
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
  // The daemon pauses the takeover of a session whose stream ends (reason
  // `disconnected`); mirror that locally so the dock offers Reacquire at once.
  const wasOwning =
    isActivelyOwningTakeover(state) &&
    (state.viewState === "active" || state.viewState === "acquiring");
  return {
    ...state,
    disconnected: true,
    inputAllowed: false,
    statusInputAllowed: false,
    viewState: wasOwning ? "paused" : state.viewState,
    reason: wasOwning ? "disconnected" : state.reason
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
      inputAllowed: false,
      statusInputAllowed: false
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
    // An acknowledgement can keep input open or close it, never re-open it.
    inputAllowed:
      state.inputAllowed &&
      resultState === "active" &&
      state.pendingRequest?.kind !== "release"
  };
}

/**
 * The profile of the view session. Only an explicit, accepted take-over ("Take control" in the
 * viewer) is `live`; the right panel's preview and every passive look are `watch`, which carries no
 * input and therefore never says "the human is driving" to the daemon (card 266-A).
 */
export function sessionProfile(state: LiveViewControllerState): "live" | "watch" {
  return isActivelyOwningTakeover(state) ? "live" : "watch";
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
/** How long a pending release may keep renewing the input lease before it is left to lapse. */
export const RELEASE_LEASE_MAX_MS = 20_000;

export function buildSessionHeartbeat(
  state: LiveViewControllerState,
  nowMs: number = Date.now()
): ViewHeartbeat {
  // A dead or frozen stream must not keep renewing the 15 s input lease.
  if (state.disconnected || state.streamStale) {
    return {};
  }
  // A pending hand-back keeps renewing the lease only while the daemon still
  // reports that takeover `active` (the POST may not have been applied yet, and
  // a lapse would pause it as Expired). Once the snapshot shows it suspended,
  // paused or gone, the empty heartbeat is right.
  const pending = state.pendingRequest;
  if (pending?.kind === "release") {
    const t = state.takeover;
    // Bounded: a release that failed before the daemon suspended the takeover
    // (input sink gone, ...) must not renew the lease for as long as a view
    // session lives; after the bound it lapses into the paused banner state.
    return t &&
      t.id === pending.takeoverId &&
      t.state === "active" &&
      nowMs - pending.startedAtMs <= RELEASE_LEASE_MAX_MS
      ? { takeover_id: pending.takeoverId, control_epoch: pending.controlEpoch }
      : {};
  }
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
  // Only OUR takeover can be handed back, never a peer's.
  const owned = ownedTakeoverRef(state);
  if (!owned) {
    return null;
  }
  const { takeoverId, controlEpoch } = owned;
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

/** The target selector is locked while holding, acquiring or handing back control. */
export function isControlBusy(state: LiveViewControllerState): boolean {
  return (
    isActivelyOwningTakeover(state) ||
    state.viewState === "acquiring" ||
    state.pendingRequest !== null
  );
}

/**
 * Whether the global Stop (hotkey / banner) must also call `POST /v1/control/override`.
 * Plain Stop stays independent: it only latches the daemon paused when a screen
 * takeover is held or in progress.
 */
export function shouldOverrideOnStop(
  state: LiveViewControllerState,
  control: ControlStateDto | null
): boolean {
  if (isActivelyOwningTakeover(state)) {
    return true;
  }
  // One source of truth: the newest control snapshot (kept fresh by the bridge's
  // poll); local state is only a fallback before any snapshot exists. A paused
  // takeover needs no override (already stopped): leaving that latch is the
  // viewer's Reacquire / Clear pause, never Stop.
  const t = control ? control.takeover : state.takeover;
  return Boolean(t && (t.state === "active" || t.state === "acquiring"));
}

/** A release is confirmed only by fresh state with the takeover gone (or released). */
export function releaseConfirmedBy(
  dto: ControlStateDto,
  takeoverId: string
): boolean {
  const t = dto.takeover ?? null;
  return t === null || t.id !== takeoverId || (t.state as string) === "released";
}

/** Bounded exponential reconnect backoff: 0.5 s, 1 s, 2 s, 4 s, capped at 4 s. */
export function reconnectDelayMs(attempt: number): number {
  const n = Math.max(1, Math.floor(attempt));
  return Math.min(4000, 500 * 2 ** (n - 1));
}

/**
 * Whether the open viewer or an explicit request from the visible card needs a capture session.
 */
export function computeWantSession(a: {
  hasClient: boolean;
  documentHidden: boolean;
  target: TargetInfo | null;
  viewerOpen: boolean;
  cardRequested?: boolean;
}): boolean {
  return (
    a.hasClient &&
    !a.documentHidden &&
    a.target !== null &&
    summarizeTarget(a.target).canView &&
    !a.target.unavailable_reason &&
    (a.viewerOpen || a.cardRequested === true)
  );
}

export type ViewerClosePlan = "close" | "release_first" | "dismiss";

/**
 * What closing the viewer must do: hand control back first while owning it
 * (a hidden screen must not leave the bot paused), dismiss locally while a
 * hand-back is pending, otherwise just close.
 */
export function planViewerClose(state: LiveViewControllerState): ViewerClosePlan {
  // A pending hand-back (failed, or daemon unreachable) must never trap the user:
  // dismiss only hides the viewer, ownership and the retry request id stay.
  if (state.pendingRequest?.kind === "release") {
    return "dismiss";
  }
  return isActivelyOwningTakeover(state) ? "release_first" : "close";
}

/** Fields of a control snapshot that matter for state; ignores countdown noise. */
export function controlSnapshotKey(dto: ControlStateDto): string {
  const t = dto.takeover;
  return JSON.stringify([
    dto.latched,
    dto.mode,
    t ? [t.id, t.state, t.reason ?? null, t.owner, t.target_id, t.session_id, t.control_epoch] : null
  ]);
}

const NIL_UUID = "00000000-0000-0000-0000-000000000000";

export interface PauseRecovery {
  takeoverId: string;
  reason: ViewReason | null;
}

/**
 * A takeover of THIS client that the daemon left paused (stream drop, lapsed
 * lease, local Stop, restart). The daemon only shows real ids to the owning
 * actor, so a non-nil id in the snapshot means it is ours. Independent of the
 * viewer, the right panel and the native run state: while this is non-null the
 * bot is paused behind the user and a control must stay on screen.
 */
/** Id of a paused takeover that is OURS in a daemon snapshot (others are redacted to nil). */
export function snapshotPausedTakeoverId(
  control: ControlStateDto | null
): string | null {
  const t = control?.takeover;
  return t && t.state === "paused" && t.id !== NIL_UUID && t.id !== ""
    ? t.id
    : null;
}

/** Owner label of a takeover that is not ours (live, or paused with a redacted id). */
export function foreignTakeoverOwner(
  state: LiveViewControllerState
): string | null {
  const t = state.takeover;
  if (!t || t.id === state.ownedTakeoverId) {
    return null;
  }
  if (t.state === "paused") {
    return t.id === NIL_UUID || t.id === "" ? t.owner : null;
  }
  return t.state === "active" || t.state === "acquiring" ? t.owner : null;
}

export function needsPauseRecovery(
  state: LiveViewControllerState,
  control: ControlStateDto | null
): PauseRecovery | null {
  const snap = control ? control : state.takeover ? { takeover: state.takeover } : null;
  const id = snapshotPausedTakeoverId(snap as ControlStateDto | null);
  if (id !== null) {
    return { takeoverId: id, reason: snap?.takeover?.reason ?? null };
  }
  // A locally mirrored owned pause (stream drop) counts until a snapshot says
  // otherwise, so a missed `control_state_changed` cannot hide the control.
  const owned = state.ownedTakeoverId;
  if (
    owned !== null &&
    state.viewState === "paused" &&
    (control === null || control.takeover?.id === owned)
  ) {
    return { takeoverId: owned, reason: state.reason };
  }
  return null;
}

export interface ForeignPaused {
  owner: string;
  targetId: string;
  reason: ViewReason | null;
}

/**
 * Another actor's PAUSED takeover as the local snapshot shows it (id redacted to
 * nil). Only the local user may clear it (`POST /v1/control/recover`).
 */
export function foreignPausedTakeover(
  state: LiveViewControllerState,
  control: ControlStateDto | null
): ForeignPaused | null {
  const t = control ? control.takeover : state.takeover;
  if (!t || t.state !== "paused" || (t.id !== NIL_UUID && t.id !== "")) {
    return null;
  }
  return { owner: t.owner, targetId: t.target_id, reason: t.reason ?? null };
}

/** Does a fresh snapshot still show exactly that foreign paused takeover? */
export function sameForeignPaused(
  control: ControlStateDto | null,
  expected: ForeignPaused
): boolean {
  const t = control?.takeover;
  return (
    !!t &&
    t.state === "paused" &&
    (t.id === NIL_UUID || t.id === "") &&
    t.owner === expected.owner &&
    t.target_id === expected.targetId &&
    (t.reason ?? null) === expected.reason
  );
}

/**
 * Loopback endpoints that are NOT this computer's daemon: the guarded proxy to a
 * paired host also listens on 127.0.0.1, but whoever uses it is a paired Desktop
 * peer of the host, never its local user (card 252).
 */
const remoteLoopbackBaseUrls = new Set<string>();

function normalizeBaseUrl(baseUrl: string): string {
  return baseUrl.replace(/\/$/, "");
}

/** Mark `baseUrl` (a loopback proxy to a paired host) as a peer connection. */
export function markRemoteBaseUrl(baseUrl: string): () => void {
  const key = normalizeBaseUrl(baseUrl);
  remoteLoopbackBaseUrls.add(key);
  return () => {
    remoteLoopbackBaseUrls.delete(key);
  };
}

/** Loopback (or mock) daemon URL: this client is the local user, not a paired peer. */
export function isLocalBaseUrl(baseUrl: string): boolean {
  if (baseUrl.startsWith("mock://")) {
    return true;
  }
  if (remoteLoopbackBaseUrls.has(normalizeBaseUrl(baseUrl))) {
    return false;
  }
  try {
    const host = new URL(baseUrl).hostname;
    return host === "localhost" || host === "127.0.0.1" || host === "[::1]" || host === "::1";
  } catch {
    return false;
  }
}

export type UiAction =
  | "bitirdim"
  | "retry_bitirdim"
  | "reacquire"
  | "clear_pause"
  | "clear_foreign_pause"
  | "clear_latch"
  | "open_screen"
  | "stop";

/**
 * `stop` means the viewer header's global Stop button, which is always offered
 * (the dock list is the set of buttons the viewer renders for a state).
 */
export interface VisibleActions {
  dock: UiAction[];
  panel: UiAction[];
  banner: UiAction[];
}

/**
 * The control actions each surface offers for a state. The dock, the panel block
 * and the banner render exactly this list, and a table-driven test pins it so a
 * change cannot strand a state without a way to stop input, hand back / be told
 * the hand-back is gone, and unpause the bot.
 */
export function visibleActions(
  state: LiveViewControllerState,
  control: ControlStateDto | null,
  isLocal: boolean
): VisibleActions {
  const out: VisibleActions = { dock: ["stop"], panel: [], banner: [] };
  const releasePending = state.pendingRequest?.kind === "release";
  const ownsActive =
    isActivelyOwningTakeover(state) && state.viewState === "active";
  const own = needsPauseRecovery(state, control);
  const foreign = foreignPausedTakeover(state, control);

  if (ownsActive || releasePending) {
    out.dock.push(state.lastError && releasePending ? "retry_bitirdim" : "bitirdim");
  } else if (state.viewState === "paused" && own) {
    const reacquire = describeRecoveryPrompt(state, control)?.mode === "reacquire";
    if (reacquire) {
      out.dock.push("reacquire");
    }
    out.dock.push("clear_pause");
  } else if (!state.disconnected) {
    if (foreign && isLocal) {
      out.dock.push("clear_foreign_pause");
    }
    if (control?.latched === "paused" && !state.takeover) {
      out.dock.push("clear_latch");
    }
  }

  if (isActivelyOwningTakeover(state) || releasePending) {
    out.panel.push(state.lastError && releasePending ? "retry_bitirdim" : "bitirdim");
  } else if (own) {
    if (describeRecoveryPrompt(state, control)?.mode === "reacquire") {
      out.panel.push("reacquire");
    }
    // "Clear pause" lives in the banner (always mounted): a second button in the panel is a duplicate.
  } else {
    out.panel.push("open_screen");
  }

  if (own) {
    out.banner.push("clear_pause", "open_screen");
  } else if (foreign && isLocal) {
    out.banner.push("clear_foreign_pause", "open_screen");
  }
  return out;
}

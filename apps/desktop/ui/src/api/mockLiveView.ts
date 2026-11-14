import type {
  ControlModeName,
  ControlStateDto,
  CreateSessionInput,
  EffectiveProfile,
  InputBatch,
  InputResult,
  ReleaseRequest,
  SessionInfo,
  SessionUpdate,
  TakeoverAccepted,
  TakeoverInfo,
  TakeoverRequest,
  TargetInfo,
  ViewHeartbeat,
  ViewReason,
  ViewState
} from "./types.ts";
import { ApiError, LIVE_VIEW_MEDIA_TYPE } from "./client.ts";
import { encodeLiveViewRecord, type RecordHeader } from "../lib/liveViewWire.ts";

// Valid 1x1 JPEG bytes used only in non-DOM Node test environments.
const NODE_FALLBACK_JPEG = new Uint8Array([
  0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01,
  0x01, 0x01, 0x00, 0x48, 0x00, 0x48, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43,
  0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09,
  0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d, 0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12,
  0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d, 0x1a, 0x1c, 0x1c, 0x20,
  0x24, 0x2e, 0x27, 0x20, 0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28, 0x37, 0x29,
  0x2c, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1f, 0x27, 0x39, 0x3d, 0x38, 0x32,
  0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01,
  0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xff, 0xc4, 0x00, 0x14, 0x00, 0x01,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x09, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00,
  0xd2, 0xcf, 0x20, 0xff, 0xd9
]);

const ATLAS_BOT = "11111111-1111-4111-8111-111111111111";
const NOVA_BOT = "22222222-2222-4222-8222-222222222222";

export const MOCK_TARGETS: readonly TargetInfo[] = [
  {
    target_id: "a1000000-0000-4000-8000-000000000001",
    bot_id: ATLAS_BOT,
    run_id: "mock-live",
    host_id: "local",
    kind: "browser",
    generation: 1,
    capabilities: {
      codecs: ["jpeg", "webp"],
      view: true,
      input: true,
      reason: null
    },
    unavailable_reason: null
  },
  {
    target_id: "a1000000-0000-4000-8000-000000000002",
    bot_id: null,
    run_id: null,
    host_id: "local",
    kind: "desktop",
    generation: 1,
    capabilities: {
      codecs: ["jpeg"],
      view: true,
      input: true,
      reason: null
    },
    unavailable_reason: null
  },
  {
    target_id: "a1000000-0000-4000-8000-000000000003",
    bot_id: NOVA_BOT,
    run_id: null,
    host_id: "local",
    kind: "browser",
    generation: 1,
    capabilities: {
      codecs: ["jpeg"],
      view: true,
      input: true,
      reason: "browser_idle"
    },
    unavailable_reason: "browser_idle"
  },
  {
    target_id: "a1000000-0000-4000-8000-000000000004",
    bot_id: null,
    run_id: null,
    host_id: "peer:studio-node",
    kind: "desktop",
    generation: 1,
    capabilities: {
      codecs: [],
      view: false,
      input: false,
      reason: "remote_node_live_view"
    },
    unavailable_reason: "remote_node_live_view"
  }
];

interface MockSession {
  id: string;
  target: TargetInfo;
  profile: EffectiveProfile;
  nextSeq: number;
  frameSeq: number;
  geometryRevision: number;
  subscribers: Set<(bytes: Uint8Array) => void>;
}

function defaultProfile(input: CreateSessionInput): EffectiveProfile {
  return {
    profile: input.profile,
    codec: "jpeg",
    max_width: Math.max(320, Math.min(1920, input.max_width)),
    max_height: Math.max(200, Math.min(1080, input.max_height)),
    max_fps: input.profile === "live" ? 10 : 1,
    quality: input.profile === "live" ? 76 : 60,
    burst_bytes: 2 * 1024 * 1024,
    bytes_per_second: 1024 * 1024
  };
}

function mockLvStateParam(): string | null {
  if (typeof window === "undefined") {
    return null;
  }
  return new URLSearchParams(window.location.search).get("lvState");
}

async function renderSyntheticSceneJpeg(params: {
  kind: TargetInfo["kind"];
  width: number;
  height: number;
  cursor: { x: number; y: number } | null;
  typedText: string;
  clicks: number;
  state: ViewState;
}): Promise<Uint8Array> {
  if (typeof document === "undefined") {
    return NODE_FALLBACK_JPEG;
  }
  const w = Math.max(480, Math.min(1280, params.width));
  const h = Math.max(300, Math.min(800, params.height));
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    return NODE_FALLBACK_JPEG;
  }

  // Stylized non-sensitive workspace / browser scene (clean UI surfaces).
  const bg = ctx.createLinearGradient(0, 0, w, h);
  if (params.kind === "desktop") {
    bg.addColorStop(0, "#191c24");
    bg.addColorStop(1, "#11131a");
  } else {
    bg.addColorStop(0, "#1d2028");
    bg.addColorStop(1, "#14161c");
  }
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, w, h);

  // Subtle ambient glow inside the fake screen
  const glow = ctx.createRadialGradient(w * 0.72, h * 0.25, 20, w * 0.72, h * 0.25, w * 0.45);
  glow.addColorStop(0, "rgba(157, 154, 232, 0.16)");
  glow.addColorStop(1, "rgba(157, 154, 232, 0)");
  ctx.fillStyle = glow;
  ctx.fillRect(0, 0, w, h);

  // Window frame
  const padX = Math.round(w * 0.06);
  const padY = Math.round(h * 0.08);
  const winW = w - padX * 2;
  const winH = h - padY * 2;
  ctx.fillStyle = "#20232d";
  ctx.strokeStyle = "rgba(255,255,255,0.1)";
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.roundRect(padX, padY, winW, winH, 14);
  ctx.fill();
  ctx.stroke();

  // Top chrome bar
  ctx.fillStyle = "#272b37";
  ctx.beginPath();
  ctx.roundRect(padX, padY, winW, 44, [14, 14, 0, 0]);
  ctx.fill();

  const dots = ["#e06c75", "#e5c07b", "#64b98d"];
  dots.forEach((c, idx) => {
    ctx.fillStyle = c;
    ctx.beginPath();
    ctx.arc(padX + 22 + idx * 18, padY + 22, 5, 0, Math.PI * 2);
    ctx.fill();
  });

  // Address / app bar
  ctx.fillStyle = "rgba(255,255,255,0.06)";
  ctx.beginPath();
  ctx.roundRect(padX + 96, padY + 11, winW - 140, 22, 11);
  ctx.fill();
  ctx.fillStyle = "rgba(255,255,255,0.68)";
  ctx.font = "500 12px system-ui, -apple-system, sans-serif";
  ctx.fillText(
    params.kind === "browser"
      ? "https://workspace.example.local/portal/review"
      : "Praxi Workstation · Display 1",
    padX + 110,
    padY + 26
  );

  // Content area cards
  const contentX = padX + 28;
  const contentY = padY + 70;
  ctx.fillStyle = "rgba(255,255,255,0.92)";
  ctx.font = "600 20px system-ui, -apple-system, sans-serif";
  ctx.fillText(
    params.kind === "browser"
      ? "Two-factor verification checkpoint"
      : "Workstation desktop session",
    contentX,
    contentY + 12
  );

  ctx.fillStyle = "rgba(255,255,255,0.58)";
  ctx.font = "400 13px system-ui, -apple-system, sans-serif";
  ctx.fillText(
    params.state === "active"
      ? "Interactive input enabled — type or click directly inside the stage."
      : "Atlas paused on a protected step waiting for your inspection.",
    contentX,
    contentY + 36
  );

  // Interactive mock card
  const cardY = contentY + 62;
  const cardW = Math.min(460, winW - 56);
  ctx.fillStyle = "rgba(255,255,255,0.04)";
  ctx.strokeStyle = "rgba(255,255,255,0.1)";
  ctx.beginPath();
  ctx.roundRect(contentX, cardY, cardW, 176, 12);
  ctx.fill();
  ctx.stroke();

  ctx.fillStyle = "rgba(255,255,255,0.65)";
  ctx.font = "500 12px system-ui, -apple-system, sans-serif";
  ctx.fillText("Verification note", contentX + 20, cardY + 32);

  // Mock input field showing synthetic typed buffer
  ctx.fillStyle = "#151821";
  ctx.strokeStyle =
    params.state === "active" ? "rgba(157, 154, 232, 0.7)" : "rgba(255,255,255,0.14)";
  ctx.beginPath();
  ctx.roundRect(contentX + 20, cardY + 46, cardW - 40, 38, 8);
  ctx.fill();
  ctx.stroke();

  ctx.fillStyle = "rgba(255,255,255,0.9)";
  ctx.font = "400 14px ui-monospace, monospace";
  const displayValue = params.typedText || "ready-to-continue";
  ctx.fillText(displayValue.slice(-36), contentX + 32, cardY + 70);

  // Primary mock CTA button
  const btnX = contentX + 20;
  const btnY = cardY + 106;
  ctx.fillStyle = "#9D9AE8";
  ctx.beginPath();
  ctx.roundRect(btnX, btnY, 170, 38, 8);
  ctx.fill();
  ctx.fillStyle = "#14141b";
  ctx.font = "600 13px system-ui, -apple-system, sans-serif";
  ctx.fillText(`Continue step (${params.clicks})`, btnX + 22, btnY + 24);

  // Side status pills
  const statX = contentX + cardW + 24;
  if (statX + 150 < padX + winW - 20) {
    ctx.fillStyle = "rgba(255,255,255,0.035)";
    ctx.strokeStyle = "rgba(255,255,255,0.08)";
    ctx.beginPath();
    ctx.roundRect(statX, cardY, padX + winW - 24 - statX, 176, 12);
    ctx.fill();
    ctx.stroke();

    ctx.fillStyle = "#64b98d";
    ctx.beginPath();
    ctx.arc(statX + 24, cardY + 30, 4.5, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "rgba(255,255,255,0.8)";
    ctx.font = "500 13px system-ui, -apple-system, sans-serif";
    ctx.fillText("Session healthy", statX + 38, cardY + 34);
    ctx.fillStyle = "rgba(255,255,255,0.5)";
    ctx.font = "400 12px system-ui, -apple-system, sans-serif";
    ctx.fillText(`Resolution ${w}×${h}`, statX + 20, cardY + 64);
    ctx.fillText("No local storage", statX + 20, cardY + 86);
  }

  // Pointer feedback ripple
  if (params.cursor) {
    const cx = params.cursor.x * w;
    const cy = params.cursor.y * h;
    ctx.strokeStyle = "#9D9AE8";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(cx, cy, 14, 0, Math.PI * 2);
    ctx.stroke();
    ctx.fillStyle = "#ffffff";
    ctx.beginPath();
    ctx.arc(cx, cy, 4, 0, Math.PI * 2);
    ctx.fill();
  }

  return await new Promise<Uint8Array>((resolve) => {
    canvas.toBlob(
      (blob) => {
        if (!blob) {
          resolve(NODE_FALLBACK_JPEG);
          return;
        }
        void blob
          .arrayBuffer()
          .then((buf) => resolve(new Uint8Array(buf)))
          .catch(() => resolve(NODE_FALLBACK_JPEG));
      },
      "image/jpeg",
      0.8
    );
  });
}

export class MockLiveViewService {
  private targets: TargetInfo[] = structuredClone([...MOCK_TARGETS]);
  private sessions = new Map<string, MockSession>();
  private takeover: TakeoverInfo | null = null;
  private latched: ControlModeName | null = null;
  private mode: ControlModeName = "idle";
  private lastRelease: ControlStateDto["release"] = null;
  private epoch = 1;
  private lastRequestIds = new Map<string, TakeoverAccepted>();
  private cursor: { x: number; y: number } | null = null;
  private typedText = "";
  private clicks = 0;
  private timers = new Set<ReturnType<typeof setTimeout>>();

  constructor() {
    const forced = mockLvStateParam();
    if (forced === "paused") {
      this.mode = "paused";
      this.latched = "paused";
      this.takeover = {
        id: "to-mock-paused-1",
        control_epoch: this.epoch,
        input_expires_in_ms: 0,
        owner: "local-ui:1",
        reason: "local_override",
        session_id: "sess-mock-paused",
        state: "paused",
        target_id: this.targets[0].target_id
      };
    } else if (forced === "unavailable") {
      this.targets = [
        {
          ...this.targets[0],
      capabilities: {
        ...this.targets[0].capabilities,
        reason: "browser_idle"
      },
      unavailable_reason: "browser_idle"
        },
        ...this.targets.slice(1)
      ];
    }
  }

  listTargets(): TargetInfo[] {
    return structuredClone(this.targets);
  }

  getControlState(): ControlStateDto {
    return {
      app_connected: true,
      expires_in_ms: 15_000,
      latched: this.latched,
      mode: this.mode,
      release: this.lastRelease,
      takeover: this.takeover ? structuredClone(this.takeover) : null
    };
  }

  putControlState(mode: ControlModeName, release = false): ControlStateDto {
    if (this.takeover) {
      if (mode === "paused" && !release) {
        return this.overrideControl();
      }
      throw new ApiError(409, "conflict", "owned takeover controls the latch");
    }
    this.mode = mode;
    if (release) {
      this.latched = null;
    } else if (mode === "paused" || mode === "human_driving") {
      this.latched = mode;
    }
    return this.getControlState();
  }

  createSession(input: CreateSessionInput): SessionInfo {
    const target =
      this.targets.find((t) => t.target_id === input.target_id) ??
      this.targets[0];
    if (!target) {
      throw new ApiError(404, "not_found", "target not found");
    }
    const id = `sess-${crypto.randomUUID()}`;
    const profile = defaultProfile(input);
    const s: MockSession = {
      id,
      target,
      profile,
      nextSeq: 1,
      frameSeq: 1,
      geometryRevision: 1,
      subscribers: new Set()
    };
    this.sessions.set(id, s);
    return {
      session_id: id,
      generation: target.generation,
      expires_in_ms: 20_000,
      input_capabilities: target.capabilities.input,
      effective_profile: profile,
      stream_url: `/v1/view/sessions/${id}/frames`
    };
  }

  updateSession(id: string, patch: SessionUpdate): EffectiveProfile {
    const s = this.requireSession(id);
    s.profile = {
      ...s.profile,
      ...(patch.profile ? { profile: patch.profile, max_fps: patch.profile === "live" ? 10 : 1 } : {}),
      ...(typeof patch.max_width === "number" ? { max_width: patch.max_width } : {}),
      ...(typeof patch.max_height === "number" ? { max_height: patch.max_height } : {}),
      ...(typeof patch.quality === "number" ? { quality: patch.quality } : {})
    };
    return structuredClone(s.profile);
  }

  heartbeatSession(id: string, input: ViewHeartbeat): void {
    this.requireSession(id);
    if (input.takeover_id && this.takeover?.id === input.takeover_id) {
      this.takeover = {
        ...this.takeover,
        input_expires_in_ms: 15_000
      };
    }
  }

  deleteSession(id: string): void {
    this.sessions.delete(id);
  }

  requestTakeover(input: TakeoverRequest): TakeoverAccepted {
    const cached = this.lastRequestIds.get(input.request_id);
    if (cached) {
      return cached;
    }
    const s = this.requireSession(input.session_id);
    if (s.target.unavailable_reason) {
      throw new ApiError(409, "conflict", s.target.unavailable_reason);
    }
    const takeoverId =
      input.reacquire_takeover_id ?? `to-${crypto.randomUUID()}`;
    this.epoch += 1;
    this.takeover = {
      id: takeoverId,
      control_epoch: this.epoch,
      input_expires_in_ms: 15_000,
      owner: "local-ui:1",
      reason: null,
      session_id: s.id,
      state: "acquiring",
      target_id: s.target.target_id
    };
    this.mode = "human_driving";
    this.latched = "human_driving";
    s.nextSeq = 1;
    s.profile = { ...s.profile, profile: "live", max_fps: 10 };
    this.broadcastStatus(s, "acquiring", null, false);

    const accepted: TakeoverAccepted = {
      takeover_id: takeoverId,
      control_epoch: this.epoch,
      state: "acquiring"
    };
    this.lastRequestIds.set(input.request_id, accepted);

    if (mockLvStateParam() !== "acquiring") {
      const timer = setTimeout(() => {
        this.timers.delete(timer);
        if (this.takeover?.id === takeoverId && this.takeover.state === "acquiring") {
          this.takeover = { ...this.takeover, state: "active" };
          this.broadcastStatus(s, "active", null, true);
        }
      }, 360);
      this.timers.add(timer);
    }
    return accepted;
  }

  releaseControl(req?: ReleaseRequest): ControlStateDto {
    if (!req) {
      this.latched = null;
      this.mode = "idle";
      return this.getControlState();
    }
    if (this.lastRelease?.request_id === req.request_id) {
      return this.getControlState();
    }
    if (!this.takeover || this.takeover.id !== req.takeover_id) {
      throw new ApiError(409, "conflict", "no matching active takeover");
    }
    const s = this.sessions.get(this.takeover.session_id);
    this.takeover = null;
    this.latched = null;
    this.mode = "bot_driving";
    this.lastRelease = {
      takeover_id: req.takeover_id,
      request_id: req.request_id,
      state: "released"
    };
    if (s) {
      s.profile = { ...s.profile, profile: "watch", max_fps: 1 };
      this.broadcastStatus(s, "watch", null, false);
    }
    return this.getControlState();
  }

  overrideControl(): ControlStateDto {
    if (this.takeover) {
      const s = this.sessions.get(this.takeover.session_id);
      this.takeover = {
        ...this.takeover,
        state: "paused",
        reason: "local_override"
      };
      if (s) {
        this.broadcastStatus(s, "paused", "local_override", false);
      }
    }
    this.latched = "paused";
    this.mode = "paused";
    return this.getControlState();
  }

  recoverControl(): ControlStateDto {
    const prevSession = this.takeover
      ? this.sessions.get(this.takeover.session_id)
      : undefined;
    this.takeover = null;
    this.latched = null;
    this.mode = "bot_driving";
    if (prevSession) {
      this.broadcastStatus(prevSession, "watch", null, false);
    }
    return this.getControlState();
  }

  async sendInput(id: string, batch: InputBatch): Promise<InputResult> {
    const s = this.requireSession(id);
    if (!this.takeover || this.takeover.id !== batch.takeover_id) {
      throw new ApiError(403, "forbidden", "takeover owner required");
    }
    if (this.takeover.control_epoch !== batch.control_epoch) {
      throw new ApiError(403, "forbidden", "control epoch mismatch");
    }
    if (this.takeover.state !== "active") {
      throw new ApiError(409, "conflict", "takeover not active");
    }
    if (batch.first_seq !== s.nextSeq) {
      throw new ApiError(409, "resync_required", "sequence mismatch");
    }
    for (const ev of batch.events) {
      if (ev.type === "pointer") {
        this.cursor = { x: ev.x, y: ev.y };
        if (ev.phase === "down") {
          this.clicks += 1;
        }
      } else if (ev.type === "text") {
        this.typedText = (this.typedText + ev.text).slice(-48);
      } else if (ev.type === "key" && ev.phase === "down") {
        if (ev.key === "Backspace") {
          this.typedText = this.typedText.slice(0, -1);
        } else if (ev.key.length === 1) {
          this.typedText = (this.typedText + ev.key).slice(-48);
        }
      } else if (ev.type === "reset") {
        this.cursor = null;
      }
    }
    s.nextSeq += batch.events.length;
    await this.pushFrame(s, "action");
    return {
      accepted_through: s.nextSeq - 1,
      applied_through: s.nextSeq - 1,
      state: this.takeover.state
    };
  }

  openStream(sessionId: string, signal?: AbortSignal): Response {
    const s = this.requireSession(sessionId);
    const encoderStream = new ReadableStream<Uint8Array>({
      start: (controller) => {
        const push = (bytes: Uint8Array) => {
          try {
            controller.enqueue(bytes);
          } catch {
            // closed
          }
        };
        s.subscribers.add(push);

        const initialViewState: ViewState = s.target.unavailable_reason
          ? "unavailable"
          : this.takeover?.session_id === s.id
            ? this.takeover.state
            : this.takeover?.state === "paused"
              ? "paused"
              : "watch";
        const initialReason: ViewReason | null =
          s.target.unavailable_reason ?? this.takeover?.reason ?? null;

        push(
          encodeLiveViewRecord({
            v: 1,
            type: "status",
            session_id: s.id,
            state: initialViewState,
            reason: initialReason,
            effective_profile: s.profile,
            control_epoch: this.takeover?.control_epoch ?? 0,
            input_allowed: initialViewState === "active"
          })
        );

        void this.pushFrameTo(s, push, "refresh");

        const beat = setInterval(() => {
          if (!this.sessions.has(s.id)) {
            return;
          }
          push(
            encodeLiveViewRecord({
              v: 1,
              type: "heartbeat",
              server_time_ms: Date.now()
            })
          );
          if (s.profile.profile === "watch") {
            void this.pushFrameTo(s, push, "idle");
          }
        }, 2500);

        const cleanup = () => {
          clearInterval(beat);
          s.subscribers.delete(push);
          try {
            controller.close();
          } catch {
            // already closed
          }
        };
        if (signal) {
          if (signal.aborted) {
            cleanup();
          } else {
            signal.addEventListener("abort", cleanup, { once: true });
          }
        }
      }
    });

    return new Response(encoderStream, {
      status: 200,
      headers: {
        "Content-Type": LIVE_VIEW_MEDIA_TYPE,
        "Cache-Control": "no-store"
      }
    });
  }

  private requireSession(id: string): MockSession {
    const s = this.sessions.get(id);
    if (!s) {
      throw new ApiError(404, "not_found", `view session ${id} not found`);
    }
    return s;
  }

  private broadcastStatus(
    s: MockSession,
    state: ViewState,
    reason: ViewReason | null,
    inputAllowed: boolean
  ): void {
    const rec = encodeLiveViewRecord({
      v: 1,
      type: "status",
      session_id: s.id,
      state,
      reason,
      effective_profile: s.profile,
      control_epoch: this.takeover?.control_epoch ?? 0,
      input_allowed: inputAllowed
    });
    for (const sub of s.subscribers) {
      sub(rec);
    }
  }

  private async pushFrame(
    s: MockSession,
    cause: "idle" | "action" | "refresh"
  ): Promise<void> {
    const frameRec = await this.encodeFrame(s, cause);
    for (const sub of s.subscribers) {
      sub(frameRec);
    }
  }

  private async pushFrameTo(
    s: MockSession,
    push: (bytes: Uint8Array) => void,
    cause: "idle" | "action" | "refresh"
  ): Promise<void> {
    const frameRec = await this.encodeFrame(s, cause);
    push(frameRec);
  }

  private async encodeFrame(
    s: MockSession,
    cause: "idle" | "action" | "refresh"
  ): Promise<Uint8Array> {
    const seq = s.frameSeq++;
    const w = Math.min(1280, Math.max(480, s.profile.max_width));
    const h = Math.min(800, Math.max(300, s.profile.max_height));
    const state: ViewState =
      this.takeover?.session_id === s.id ? this.takeover.state : "watch";
    const jpeg = await renderSyntheticSceneJpeg({
      kind: s.target.kind,
      width: w,
      height: h,
      cursor: this.cursor,
      typedText: this.typedText,
      clicks: this.clicks,
      state
    });
    const header: RecordHeader = {
      v: 1,
      type: "frame",
      session_id: s.id,
      sent_at_ms: Date.now(),
      target_id: s.target.target_id,
      generation: s.target.generation,
      frame_seq: seq,
      captured_at_ms: Date.now(),
      cause,
      action_seq: cause === "action" ? s.nextSeq - 1 : null,
      codec: "jpeg",
      width: w,
      height: h,
      source_rect: {
        x: 0,
        y: 0,
        width: w,
        height: h,
        units: "physical_px"
      },
      geometry_revision: s.geometryRevision,
      repeated: false
    };
    return encodeLiveViewRecord(header, jpeg);
  }
}

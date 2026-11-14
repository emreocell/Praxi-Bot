import type {
  Codec,
  EffectiveProfile,
  FrameCause,
  FrameMetadata,
  RecordHeader,
  SourceRect,
  SourceUnits,
  ViewReason,
  ViewState
} from "../api/types.ts";

export type { RecordHeader };

export const LIVE_VIEW_MEDIA_TYPE = "application/vnd.praxi.live-view.v1";
export const LIVE_VIEW_PROTOCOL_VERSION = 1;
export const PREFIX_BYTES = 8;
export const MAX_HEADER_BYTES = 8192;
export const MAX_PAYLOAD_BYTES = 2 * 1024 * 1024;
export const MAX_FRAME_WIDTH = 1920;
export const MAX_FRAME_HEIGHT = 1080;

export type WireErrorCode =
  | "invalid_prefix"
  | "header_too_large"
  | "payload_too_large"
  | "invalid_utf8"
  | "invalid_json"
  | "unsupported_version"
  | "invalid_record"
  | "truncated"
  | "poisoned";

export class LiveViewWireError extends Error {
  readonly code: WireErrorCode;

  constructor(code: WireErrorCode, message: string) {
    super(message);
    this.name = "LiveViewWireError";
    this.code = code;
  }
}

export type FrameHeader = Extract<RecordHeader, { type: "frame" }>;
export type StatusHeader = Extract<RecordHeader, { type: "status" }>;
export type HeartbeatHeader = Extract<RecordHeader, { type: "heartbeat" }>;

export interface DecodedFrameRecord {
  kind: "frame";
  header: FrameHeader;
  payload: Uint8Array;
  byteLength: number;
}

export type DecodedRecord =
  | DecodedFrameRecord
  | { kind: "status"; header: StatusHeader; byteLength: number }
  | { kind: "heartbeat"; header: HeartbeatHeader; byteLength: number };

const VALID_CODECS = new Set<Codec>(["jpeg", "webp"]);
const VALID_CAUSES = new Set<FrameCause>(["idle", "action", "refresh"]);
const VALID_UNITS = new Set<SourceUnits>(["physical_px", "css_px"]);
const VALID_STATES = new Set<ViewState>([
  "watch",
  "acquiring",
  "active",
  "paused",
  "unavailable",
  "closed"
]);

const textDecoder = new TextDecoder("utf-8", { fatal: true });
const textEncoder = new TextEncoder();

function isNonNegativeInt(v: unknown): v is number {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
}

function isFiniteNonNeg(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v) && v >= 0;
}

function isValidSourceRect(v: unknown): v is SourceRect {
  if (!v || typeof v !== "object") {
    return false;
  }
  const r = v as Record<string, unknown>;
  return (
    typeof r.x === "number" &&
    Number.isFinite(r.x) &&
    typeof r.y === "number" &&
    Number.isFinite(r.y) &&
    typeof r.width === "number" &&
    Number.isFinite(r.width) &&
    r.width > 0 &&
    typeof r.height === "number" &&
    Number.isFinite(r.height) &&
    r.height > 0 &&
    typeof r.units === "string" &&
    VALID_UNITS.has(r.units as SourceUnits)
  );
}

function isValidEffectiveProfile(v: unknown): v is EffectiveProfile {
  if (!v || typeof v !== "object") {
    return false;
  }
  const p = v as Record<string, unknown>;
  return (
    (p.profile === "watch" || p.profile === "live") &&
    typeof p.codec === "string" &&
    VALID_CODECS.has(p.codec as Codec) &&
    isNonNegativeInt(p.max_width) &&
    isNonNegativeInt(p.max_height) &&
    isNonNegativeInt(p.max_fps) &&
    isNonNegativeInt(p.quality) &&
    isNonNegativeInt(p.burst_bytes) &&
    isNonNegativeInt(p.bytes_per_second)
  );
}

/** Validate a decoded JSON header against wire invariants and payload size. */
export function validateRecordHeader(
  raw: unknown,
  payloadLength: number
): RecordHeader {
  if (!raw || typeof raw !== "object") {
    throw new LiveViewWireError("invalid_record", "header must be an object");
  }
  const h = raw as Record<string, unknown>;
  if (h.v !== LIVE_VIEW_PROTOCOL_VERSION) {
    throw new LiveViewWireError(
      "unsupported_version",
      `unsupported live-view record version: ${String(h.v)}`
    );
  }
  if (h.type === "frame") {
    if (payloadLength <= 0 || payloadLength > MAX_PAYLOAD_BYTES) {
      throw new LiveViewWireError(
        "invalid_record",
        "frame record must have non-empty bounded payload"
      );
    }
    const f = h as unknown as Partial<FrameHeader & FrameMetadata>;
    if (
      typeof f.session_id !== "string" ||
      f.session_id.length === 0 ||
      typeof f.target_id !== "string" ||
      f.target_id.length === 0 ||
      typeof f.codec !== "string" ||
      !VALID_CODECS.has(f.codec as Codec) ||
      typeof f.cause !== "string" ||
      !VALID_CAUSES.has(f.cause as FrameCause) ||
      typeof f.width !== "number" ||
      !Number.isSafeInteger(f.width) ||
      f.width < 1 ||
      f.width > MAX_FRAME_WIDTH ||
      typeof f.height !== "number" ||
      !Number.isSafeInteger(f.height) ||
      f.height < 1 ||
      f.height > MAX_FRAME_HEIGHT ||
      !isNonNegativeInt(f.generation) ||
      !isNonNegativeInt(f.frame_seq) ||
      !isNonNegativeInt(f.geometry_revision) ||
      !isFiniteNonNeg(f.captured_at_ms) ||
      !isFiniteNonNeg(f.sent_at_ms) ||
      (f.action_seq !== undefined &&
        f.action_seq !== null &&
        !isNonNegativeInt(f.action_seq)) ||
      typeof f.repeated !== "boolean" ||
      !isValidSourceRect(f.source_rect)
    ) {
      throw new LiveViewWireError(
        "invalid_record",
        "invalid frame metadata fields"
      );
    }
    return h as unknown as FrameHeader;
  }
  if (h.type === "status") {
    if (payloadLength !== 0) {
      throw new LiveViewWireError(
        "invalid_record",
        "status record must have zero-length payload"
      );
    }
    const s = h as Record<string, unknown>;
    if (
      typeof s.session_id !== "string" ||
      s.session_id.length === 0 ||
      typeof s.state !== "string" ||
      !VALID_STATES.has(s.state as ViewState) ||
      !isNonNegativeInt(s.control_epoch) ||
      typeof s.input_allowed !== "boolean" ||
      !isValidEffectiveProfile(s.effective_profile) ||
      (s.reason !== undefined &&
        s.reason !== null &&
        typeof s.reason !== "string")
    ) {
      throw new LiveViewWireError("invalid_record", "invalid status record");
    }
    return {
      v: 1,
      type: "status",
      session_id: s.session_id,
      state: s.state as ViewState,
      reason: (s.reason ?? null) as ViewReason | null,
      effective_profile: s.effective_profile,
      control_epoch: s.control_epoch,
      input_allowed: s.input_allowed
    };
  }
  if (h.type === "heartbeat") {
    if (payloadLength !== 0 || !isFiniteNonNeg(h.server_time_ms)) {
      throw new LiveViewWireError(
        "invalid_record",
        "invalid heartbeat record"
      );
    }
    return {
      v: 1,
      type: "heartbeat",
      server_time_ms: h.server_time_ms as number
    };
  }
  throw new LiveViewWireError("invalid_record", "unknown record type");
}

/** Encode one wire record (`u32_be(header_len) | u32_be(payload_len) | header | payload`). */
export function encodeLiveViewRecord(
  header: RecordHeader,
  payload: Uint8Array = new Uint8Array(0)
): Uint8Array {
  const headerBytes = textEncoder.encode(JSON.stringify(header));
  if (headerBytes.byteLength === 0 || headerBytes.byteLength > MAX_HEADER_BYTES) {
    throw new LiveViewWireError(
      "header_too_large",
      `header length ${headerBytes.byteLength} out of bounds`
    );
  }
  if (payload.byteLength > MAX_PAYLOAD_BYTES) {
    throw new LiveViewWireError(
      "payload_too_large",
      `payload length ${payload.byteLength} out of bounds`
    );
  }
  validateRecordHeader(header, payload.byteLength);
  const out = new Uint8Array(
    PREFIX_BYTES + headerBytes.byteLength + payload.byteLength
  );
  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
  view.setUint32(0, headerBytes.byteLength, false);
  view.setUint32(4, payload.byteLength, false);
  out.set(headerBytes, PREFIX_BYTES);
  if (payload.byteLength > 0) {
    out.set(payload, PREFIX_BYTES + headerBytes.byteLength);
  }
  return out;
}

/**
 * Incremental chunked parser for `application/vnd.praxi.live-view.v1`.
 * Enforces header and payload bounds before waiting for or allocating records,
 * and permanently poisons itself after any framing or schema error.
 */
export class LiveViewDecoder {
  private buf: Uint8Array = new Uint8Array(0);
  private offset = 0;
  private poisonedError: LiveViewWireError | null = null;
  private parsedHeader: RecordHeader | null = null;
  private expectedHeaderLen = 0;
  private expectedPayloadLen = 0;

  push(chunk: Uint8Array): DecodedRecord[] {
    if (this.poisonedError) {
      throw this.poisonedError;
    }
    if (chunk.byteLength === 0) {
      return [];
    }
    this.append(chunk);
    const records: DecodedRecord[] = [];
    try {
      while (true) {
        const unread = this.buf.byteLength - this.offset;
        if (this.expectedHeaderLen === 0) {
          if (unread < PREFIX_BYTES) {
            break;
          }
          const view = new DataView(
            this.buf.buffer,
            this.buf.byteOffset + this.offset,
            PREFIX_BYTES
          );
          const headerLen = view.getUint32(0, false);
          const payloadLen = view.getUint32(4, false);
          if (headerLen === 0) {
            throw new LiveViewWireError(
              "invalid_prefix",
              "header length cannot be zero"
            );
          }
          if (headerLen > MAX_HEADER_BYTES) {
            throw new LiveViewWireError(
              "header_too_large",
              `header length ${headerLen} exceeds ${MAX_HEADER_BYTES}`
            );
          }
          if (payloadLen > MAX_PAYLOAD_BYTES) {
            throw new LiveViewWireError(
              "payload_too_large",
              `payload length ${payloadLen} exceeds ${MAX_PAYLOAD_BYTES}`
            );
          }
          this.offset += PREFIX_BYTES;
          this.expectedHeaderLen = headerLen;
          this.expectedPayloadLen = payloadLen;
        }
        if (this.parsedHeader === null) {
          const unreadHeader = this.buf.byteLength - this.offset;
          if (unreadHeader < this.expectedHeaderLen) {
            break;
          }
          const headerSlice = this.buf.subarray(
            this.offset,
            this.offset + this.expectedHeaderLen
          );
          let jsonText: string;
          try {
            jsonText = textDecoder.decode(headerSlice);
          } catch {
            throw new LiveViewWireError(
              "invalid_utf8",
              "record header is not valid UTF-8"
            );
          }
          let parsedJson: unknown;
          try {
            parsedJson = JSON.parse(jsonText);
          } catch {
            throw new LiveViewWireError(
              "invalid_json",
              "record header is not valid JSON"
            );
          }
          this.parsedHeader = validateRecordHeader(
            parsedJson,
            this.expectedPayloadLen
          );
          this.offset += this.expectedHeaderLen;
        }
        const unreadPayload = this.buf.byteLength - this.offset;
        if (unreadPayload < this.expectedPayloadLen) {
          break;
        }
        const header = this.parsedHeader;
        const payload =
          this.expectedPayloadLen > 0
            ? this.buf.slice(this.offset, this.offset + this.expectedPayloadLen)
            : new Uint8Array(0);
        const totalBytes =
          PREFIX_BYTES + this.expectedHeaderLen + this.expectedPayloadLen;
        this.offset += this.expectedPayloadLen;
        this.parsedHeader = null;
        this.expectedHeaderLen = 0;
        this.expectedPayloadLen = 0;

        if (header.type === "frame") {
          records.push({ kind: "frame", header, payload, byteLength: totalBytes });
        } else if (header.type === "status") {
          records.push({ kind: "status", header, byteLength: totalBytes });
        } else {
          records.push({ kind: "heartbeat", header, byteLength: totalBytes });
        }
      }
      this.compact();
      return records;
    } catch (err) {
      const wireErr =
        err instanceof LiveViewWireError
          ? err
          : new LiveViewWireError("invalid_record", String(err));
      this.poisonedError = wireErr;
      this.buf = new Uint8Array(0);
      this.offset = 0;
      throw wireErr;
    }
  }

  /** Verify stream ended cleanly on a record boundary. */
  finish(): void {
    if (this.poisonedError) {
      throw this.poisonedError;
    }
    const trailing = this.buf.byteLength - this.offset;
    if (trailing > 0 || this.expectedHeaderLen > 0 || this.parsedHeader !== null) {
      const err = new LiveViewWireError(
        "truncated",
        "stream ended mid-record"
      );
      this.poisonedError = err;
      this.buf = new Uint8Array(0);
      this.offset = 0;
      throw err;
    }
  }

  private append(chunk: Uint8Array): void {
    const remaining = this.buf.byteLength - this.offset;
    if (remaining === 0) {
      this.buf = chunk;
      this.offset = 0;
      return;
    }
    const next = new Uint8Array(remaining + chunk.byteLength);
    next.set(this.buf.subarray(this.offset), 0);
    next.set(chunk, remaining);
    this.buf = next;
    this.offset = 0;
  }

  private compact(): void {
    if (this.offset === 0) {
      return;
    }
    const remaining = this.buf.byteLength - this.offset;
    if (remaining === 0) {
      this.buf = new Uint8Array(0);
      this.offset = 0;
      return;
    }
    if (this.offset > 65_536 || this.offset >= remaining) {
      this.buf = this.buf.slice(this.offset);
      this.offset = 0;
    }
  }
}

export interface FramePresenter<H> {
  decodeAndBind(frame: DecodedFrameRecord): Promise<H> | H;
  release(handle: H): void;
}

export interface PresentedFrameMetrics {
  decodeMs: number;
  droppedFrames: number;
  bufferedFrames: number;
  bytesSinceLastReport: number;
}

export interface ConsumeStreamOptions<H> {
  stream: ReadableStream<Uint8Array>;
  presenter: FramePresenter<H>;
  signal?: AbortSignal;
  nowMs?: () => number;
  onFramePresented: (
    frame: DecodedFrameRecord,
    handle: H,
    metrics: PresentedFrameMetrics
  ) => void;
  onStatus?: (status: StatusHeader) => void;
  onHeartbeat?: (heartbeat: HeartbeatHeader) => void;
  onActivity?: () => void;
}

/**
 * Consume a live-view byte stream with latest-only frame coalescing.
 * Unrendered intermediate frames are dropped before decode, and previously
 * bound frame handles are always released on replacement, abort, or stream end.
 */
export async function consumeLiveViewStream<H>(
  opts: ConsumeStreamOptions<H>
): Promise<void> {
  const now = opts.nowMs ?? (() => performance.now());
  const decoder = new LiveViewDecoder();
  const reader = opts.stream.getReader();

  let pending: DecodedFrameRecord | null = null;
  let presenting = false;
  let closed = false;
  let currentHandle: H | null = null;
  let droppedFrames = 0;
  let bytesAccumulated = 0;
  let presentLoopPromise: Promise<void> = Promise.resolve();

  const onAbort = () => {
    closed = true;
    void reader.cancel().catch(() => undefined);
  };
  opts.signal?.addEventListener("abort", onAbort, { once: true });

  const kickPresenter = () => {
    if (presenting || closed || !pending) {
      return;
    }
    presenting = true;
    presentLoopPromise = (async () => {
      try {
        while (pending && !closed && !opts.signal?.aborted) {
          const next = pending;
          pending = null;
          const t0 = now();
          let nextHandle: H;
          try {
            nextHandle = await opts.presenter.decodeAndBind(next);
          } catch {
            continue;
          }
          const decodeMs = Math.max(0, Math.round(now() - t0));
          if (closed || opts.signal?.aborted) {
            opts.presenter.release(nextHandle);
            break;
          }
          const prev = currentHandle;
          currentHandle = nextHandle;
          const bufferedFrames = pending ? 1 : 0;
          const metrics: PresentedFrameMetrics = {
            decodeMs,
            droppedFrames,
            bufferedFrames,
            bytesSinceLastReport: bytesAccumulated
          };
          droppedFrames = 0;
          bytesAccumulated = 0;
          opts.onFramePresented(next, nextHandle, metrics);
          if (prev !== null) {
            opts.presenter.release(prev);
          }
        }
      } finally {
        presenting = false;
      }
    })();
  };

  try {
    while (!opts.signal?.aborted) {
      const { value, done } = await reader.read();
      if (done) {
        break;
      }
      if (!value || value.byteLength === 0) {
        continue;
      }
      const records = decoder.push(value);
      if (records.length > 0) {
        opts.onActivity?.();
      }
      for (const rec of records) {
        if (rec.kind === "status") {
          opts.onStatus?.(rec.header);
        } else if (rec.kind === "heartbeat") {
          opts.onHeartbeat?.(rec.header);
        } else {
          bytesAccumulated += rec.byteLength;
          if (pending !== null) {
            droppedFrames += 1;
          }
          pending = rec;
        }
      }
      if (pending !== null) {
        kickPresenter();
      }
    }
    if (!opts.signal?.aborted) {
      decoder.finish();
    }
    await presentLoopPromise;
  } finally {
    closed = true;
    pending = null;
    opts.signal?.removeEventListener("abort", onAbort);
    try {
      await presentLoopPromise;
    } catch {
      // ignore presenter teardown errors
    }
    if (currentHandle !== null) {
      opts.presenter.release(currentHandle);
      currentHandle = null;
    }
    try {
      reader.releaseLock();
    } catch {
      // already released
    }
  }
}

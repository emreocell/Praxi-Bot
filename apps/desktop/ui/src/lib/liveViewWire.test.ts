import assert from "node:assert/strict";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import {
  consumeLiveViewStream,
  encodeLiveViewRecord,
  LiveViewDecoder,
  LiveViewWireError,
  MAX_HEADER_BYTES,
  MAX_PAYLOAD_BYTES,
  type FrameHeader,
  type HeartbeatHeader,
  type StatusHeader
} from "./liveViewWire.ts";

const SAMPLE_SESSION = "00000000-0000-4000-8000-000000000010";
const SAMPLE_TARGET = "00000000-0000-4000-8000-000000000020";

function makeFrameHeader(overrides: Partial<FrameHeader> = {}): FrameHeader {
  return {
    v: 1,
    type: "frame",
    session_id: SAMPLE_SESSION,
    sent_at_ms: 1_750_000_000_120,
    target_id: SAMPLE_TARGET,
    generation: 1,
    frame_seq: 1,
    captured_at_ms: 1_750_000_000_100,
    cause: "action",
    action_seq: 3,
    codec: "jpeg",
    width: 1280,
    height: 800,
    source_rect: { x: 0, y: 0, width: 1280, height: 800, units: "physical_px" },
    geometry_revision: 1,
    repeated: false,
    ...overrides
  };
}

function makeStatusHeader(overrides: Partial<StatusHeader> = {}): StatusHeader {
  return {
    v: 1,
    type: "status",
    session_id: SAMPLE_SESSION,
    state: "active",
    reason: null,
    effective_profile: {
      profile: "live",
      codec: "jpeg",
      max_width: 1280,
      max_height: 800,
      max_fps: 10,
      quality: 75,
      burst_bytes: 2 * 1024 * 1024,
      bytes_per_second: 1024 * 1024
    },
    control_epoch: 4,
    input_allowed: true,
    ...overrides
  };
}

function makeHeartbeatHeader(ms = 1_750_000_000_500): HeartbeatHeader {
  return { v: 1, type: "heartbeat", server_time_ms: ms };
}

test("decodes frame, status and heartbeat split at arbitrary 1-byte boundaries", () => {
  const payload = new Uint8Array([0xff, 0xd8, 0xff, 0xd9, 1, 2, 3]);
  const r1 = encodeLiveViewRecord(makeStatusHeader());
  const r2 = encodeLiveViewRecord(makeFrameHeader({ codec: "webp" }), payload);
  const r3 = encodeLiveViewRecord(makeHeartbeatHeader());

  const combined = new Uint8Array(r1.length + r2.length + r3.length);
  combined.set(r1, 0);
  combined.set(r2, r1.length);
  combined.set(r3, r1.length + r2.length);

  const decoder = new LiveViewDecoder();
  const got = [];
  for (let i = 0; i < combined.length; i += 1) {
    got.push(...decoder.push(combined.subarray(i, i + 1)));
  }
  decoder.finish();

  assert.equal(got.length, 3);
  assert.equal(got[0].kind, "status");
  assert.equal(got[1].kind, "frame");
  assert.deepEqual(got[1].payload, payload);
  assert.equal(got[1].header.codec, "webp");
  assert.equal(got[2].kind, "heartbeat");
  assert.equal(got[2].header.server_time_ms, 1_750_000_000_500);
});

test("rejects truncated trailing bytes at EOF", () => {
  const full = encodeLiveViewRecord(makeFrameHeader(), new Uint8Array([1, 2, 3, 4]));
  const decoder = new LiveViewDecoder();
  decoder.push(full.subarray(0, full.length - 1));
  assert.throws(() => decoder.finish(), (e: unknown) =>
    e instanceof LiveViewWireError && e.code === "truncated"
  );
});

test("rejects oversize header or payload immediately from 8-byte prefix and poisons decoder", () => {
  const prefix = new Uint8Array(8);
  const dv = new DataView(prefix.buffer);

  // Oversize header_len before rest of stream arrives
  dv.setUint32(0, MAX_HEADER_BYTES + 1, false);
  dv.setUint32(4, 0, false);
  const dec1 = new LiveViewDecoder();
  assert.throws(
    () => dec1.push(prefix),
    (e: unknown) => e instanceof LiveViewWireError && e.code === "header_too_large"
  );
  assert.throws(
    () => dec1.push(new Uint8Array([1, 2, 3])),
    (e: unknown) => e instanceof LiveViewWireError && e.code === "header_too_large"
  );

  // Oversize payload_len before payload bytes arrive
  dv.setUint32(0, 64, false);
  dv.setUint32(4, MAX_PAYLOAD_BYTES + 1, false);
  const dec2 = new LiveViewDecoder();
  assert.throws(
    () => dec2.push(prefix),
    (e: unknown) => e instanceof LiveViewWireError && e.code === "payload_too_large"
  );
});

test("rejects invalid version, codec, dimensions, and non-empty status payloads", () => {
  const badJsonHeader = new TextEncoder().encode(
    JSON.stringify({ v: 2, type: "heartbeat", server_time_ms: 1 })
  );
  const wire = new Uint8Array(8 + badJsonHeader.length);
  const dv = new DataView(wire.buffer);
  dv.setUint32(0, badJsonHeader.length, false);
  dv.setUint32(4, 0, false);
  wire.set(badJsonHeader, 8);
  assert.throws(
    () => new LiveViewDecoder().push(wire),
    (e: unknown) => e instanceof LiveViewWireError && e.code === "unsupported_version"
  );

  // Invalid dimensions (1921 > 1920)
  const badDim = new TextEncoder().encode(
    JSON.stringify(makeFrameHeader({ width: 1921 }))
  );
  const wireDim = new Uint8Array(8 + badDim.length + 1);
  new DataView(wireDim.buffer).setUint32(0, badDim.length, false);
  new DataView(wireDim.buffer).setUint32(4, 1, false);
  wireDim.set(badDim, 8);
  wireDim[8 + badDim.length] = 1;
  assert.throws(
    () => new LiveViewDecoder().push(wireDim),
    (e: unknown) => e instanceof LiveViewWireError && e.code === "invalid_record"
  );

  // Status record with non-empty payload
  const statusJson = new TextEncoder().encode(JSON.stringify(makeStatusHeader()));
  const wireStatus = new Uint8Array(8 + statusJson.length + 1);
  new DataView(wireStatus.buffer).setUint32(0, statusJson.length, false);
  new DataView(wireStatus.buffer).setUint32(4, 1, false);
  wireStatus.set(statusJson, 8);
  wireStatus[8 + statusJson.length] = 9;
  assert.throws(
    () => new LiveViewDecoder().push(wireStatus),
    (e: unknown) => e instanceof LiveViewWireError && e.code === "invalid_record"
  );
});

test("consumeLiveViewStream keeps only latest pending frame and releases all handles without leaks", async () => {
  const frames = [1, 2, 3, 4, 5].map((seq) =>
    encodeLiveViewRecord(
      makeFrameHeader({ frame_seq: seq }),
      new Uint8Array([0xff, 0xd8, seq, 0xff, 0xd9])
    )
  );

  let releaseFirstGate!: () => void;
  const firstGate = new Promise<void>((r) => {
    releaseFirstGate = r;
  });

  const liveHandles = new Set<number>();
  const releasedHandles: number[] = [];
  const presentedSeqs: number[] = [];
  let handleId = 0;

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      // Send frame 1 immediately, wait until decode Gate 1 is active, then flood 2..5 before releasing Gate 1
      controller.enqueue(frames[0]);
      setTimeout(() => {
        for (const f of frames.slice(1)) {
          controller.enqueue(f);
        }
        setTimeout(() => {
          releaseFirstGate();
          setTimeout(() => controller.close(), 15);
        }, 10);
      }, 5);
    }
  });

  await consumeLiveViewStream<number>({
    stream,
    presenter: {
      decodeAndBind: async () => {
        const id = ++handleId;
        if (id === 1) {
          await firstGate;
        }
        liveHandles.add(id);
        return id;
      },
      release: (h) => {
        liveHandles.delete(h);
        releasedHandles.push(h);
      }
    },
    onFramePresented: (frame) => {
      presentedSeqs.push(frame.header.frame_seq);
    }
  });

  assert.deepEqual(presentedSeqs, [1, 5], "intermediate pending frames coalesced before decode");
  assert.equal(liveHandles.size, 0, "all decoded handles released on close");
  assert.deepEqual(releasedHandles.sort((a, b) => a - b), [1, 2]);
});

test("MockApiClient live view stream decodes valid wire status and jpeg frames", async () => {
  const mock = new MockApiClient();
  const targets = await mock.listViewTargets();
  const firstAvailable = targets.find((t) => !t.unavailable_reason);
  assert.ok(firstAvailable);

  const session = await mock.createViewSession({
    client_id: "00000000-0000-4000-8000-000000000099",
    target_id: firstAvailable.target_id,
    profile: "watch",
    codecs: ["jpeg"],
    max_width: 640,
    max_height: 400
  });

  const abort = new AbortController();
  const res = await mock.openViewFrameStream(session.session_id, abort.signal);
  assert.ok(res.body);

  let gotStatus = false;
  let gotFrame = false;
  await consumeLiveViewStream<string>({
    stream: res.body,
    signal: abort.signal,
    presenter: {
      decodeAndBind: (frame) => `mem://${frame.header.frame_seq}`,
      release: () => {}
    },
    onStatus: (s) => {
      assert.equal(s.session_id, session.session_id);
      gotStatus = true;
    },
    onFramePresented: (f) => {
      assert.equal(f.header.target_id, firstAvailable.target_id);
      assert.ok(f.payload.byteLength > 0);
      gotFrame = true;
      abort.abort();
    }
  });

  assert.ok(gotStatus);
  assert.ok(gotFrame);
  await mock.deleteViewSession(session.session_id);
});

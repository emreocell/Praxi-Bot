import assert from "node:assert/strict";
import { test } from "node:test";
import { OUTPUT_RATE } from "./pcm.ts";
import { createPlaybackQueue, type PlaybackSink } from "./playback.ts";

/** A device clock that only moves when the test says so. */
function fakeSink() {
  const played: { at: number; seconds: number; stopped: boolean; end: () => void }[] = [];
  let clock = 10;
  const sink: PlaybackSink = {
    now: () => clock,
    play(samples, at, onEnded) {
      const item = { at, seconds: samples.length / OUTPUT_RATE, stopped: false, end: onEnded };
      played.push(item);
      return { stop: () => { item.stopped = true; } };
    }
  };
  return { sink, played, advance: (s: number) => { clock += s; } };
}

/** `ms` of 24 kHz PCM16 audio. */
const speech = (ms: number) => new Uint8Array((OUTPUT_RATE * ms) / 1000 * 2);

test("chunks are scheduled back to back after a short lead-in", () => {
  const { sink, played } = fakeSink();
  const q = createPlaybackQueue(sink, () => undefined);
  q.push(speech(100));
  q.push(speech(100));
  assert.ok(played[0]!.at > 10 && played[0]!.at < 10.2);
  assert.ok(Math.abs(played[1]!.at - (played[0]!.at + 0.1)) < 1e-9);
  assert.equal(q.busy, true);
});

test("a late chunk is not scheduled in the past", () => {
  const { sink, played, advance } = fakeSink();
  const q = createPlaybackQueue(sink, () => undefined);
  q.push(speech(100));
  advance(5);
  q.push(speech(100));
  assert.ok(played[1]!.at >= 15);
});

test("flush stops everything queued and a late 'ended' does not report idle", () => {
  const { sink, played } = fakeSink();
  let idle = 0;
  const q = createPlaybackQueue(sink, () => (idle += 1));
  q.push(speech(100));
  q.push(speech(100));
  q.flush();
  assert.deepEqual(played.map((p) => p.stopped), [true, true]);
  assert.equal(q.busy, false);
  played[0]!.end();
  assert.equal(idle, 0);
});

test("after a flush the next speech starts fresh instead of after the dropped audio", () => {
  const { sink, played } = fakeSink();
  const q = createPlaybackQueue(sink, () => undefined);
  q.push(speech(2000));
  q.flush();
  q.push(speech(100));
  assert.ok(played[1]!.at < 10.2);
});

test("idle is reported once the last chunk has played", () => {
  const { sink, played } = fakeSink();
  let idle = 0;
  const q = createPlaybackQueue(sink, () => (idle += 1));
  q.push(speech(100));
  q.push(speech(100));
  played[0]!.end();
  assert.equal(idle, 0);
  played[1]!.end();
  assert.equal(idle, 1);
  assert.equal(q.busy, false);
});

test("empty audio is ignored", () => {
  const { sink, played } = fakeSink();
  const q = createPlaybackQueue(sink, () => undefined);
  q.push(new Uint8Array(0));
  q.push(new Uint8Array(1));
  assert.equal(played.length, 0);
});

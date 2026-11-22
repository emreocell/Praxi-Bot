import assert from "node:assert/strict";
import test from "node:test";
import {
  holdReason,
  isBotMuted,
  isMuteable,
  MUTED_BOTS_MAX,
  parseMutedBots,
  setBotMuted
} from "./botMute.ts";
import { DEFAULT_QUIET_HOURS, type QuietHours } from "./quietHours.ts";

const off: QuietHours = { ...DEFAULT_QUIET_HOURS, enabled: false };
const night = (patch: Partial<QuietHours> = {}): QuietHours => ({
  ...DEFAULT_QUIET_HOURS,
  enabled: true,
  timezone: "UTC",
  from: "22:00",
  to: "07:00",
  ...patch
});
const inWindow = new Date("2026-11-24T03:00:00Z");
const outside = new Date("2026-11-24T12:00:00Z");

test("parseMutedBots keeps unique non-empty strings and drops the rest", () => {
  assert.deepEqual(parseMutedBots(["a", "b", "a", "", "  ", 5, null, { id: "c" }]), ["a", "b"]);
  assert.deepEqual(parseMutedBots(undefined), []);
  assert.deepEqual(parseMutedBots("a"), []);
  assert.deepEqual(parseMutedBots({ a: true }), []);
  const many = Array.from({ length: MUTED_BOTS_MAX + 10 }, (_, i) => `b${i}`);
  assert.equal(parseMutedBots(many).length, MUTED_BOTS_MAX);
});

test("setBotMuted adds and removes once, and is a no-op otherwise", () => {
  const empty: string[] = [];
  const one = setBotMuted(empty, "a", true);
  assert.deepEqual(one, ["a"]);
  assert.equal(setBotMuted(one, "a", true), one);
  assert.equal(setBotMuted(empty, "a", false), empty);
  assert.deepEqual(setBotMuted(["a", "b"], "a", false), ["b"]);
  assert.equal(setBotMuted(empty, "", true), empty);
  assert.equal(isBotMuted(["a"], "a"), true);
  assert.equal(isBotMuted(["a"], "b"), false);
  assert.equal(isBotMuted([""], ""), false);
});

test("a mute silences long-run and failed only, never needs-you", () => {
  assert.equal(isMuteable("longRun"), true);
  assert.equal(isMuteable("failed"), true);
  assert.equal(isMuteable("needsYou"), false);
});

// [quiet hours, time, kind, bot muted?, expected reason]
const cases: [string, QuietHours, Date, "needsYou" | "longRun" | "failed", boolean, string | null][] = [
  ["no quiet hours, not muted: shown", off, outside, "longRun", false, null],
  ["no quiet hours, muted long run: held", off, outside, "longRun", true, "muted"],
  ["no quiet hours, muted failure: held", off, outside, "failed", true, "muted"],
  ["no quiet hours, muted approval: shown", off, outside, "needsYou", true, null],
  ["quiet, muted long run: mute wins", night(), inWindow, "longRun", true, "muted"],
  ["quiet, unmuted failure: quiet", night(), inWindow, "failed", false, "quiet"],
  ["quiet + urgent allowed, muted approval: shown", night(), inWindow, "needsYou", true, null],
  ["quiet + urgent off, muted approval: quiet (not muted)", night({ allowUrgent: false }), inWindow, "needsYou", true, "quiet"],
  ["outside window, muted approval: shown", night({ allowUrgent: false }), outside, "needsYou", true, null]
];

for (const [name, quiet, at, kind, isMuted, want] of cases) {
  test(`holdReason: ${name}`, () => {
    const muted = isMuted ? ["bot-1"] : ["other"];
    assert.equal(holdReason(at, quiet, kind, "bot-1", muted), want);
  });
}

test("holdReason ignores a decision without a bot id", () => {
  assert.equal(holdReason(outside, off, "failed", "", [""]), null);
});

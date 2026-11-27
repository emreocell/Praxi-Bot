import assert from "node:assert/strict";
import { test } from "node:test";
import { parseMissionDigest } from "./missionDigestText.ts";

// The exact shape `missions::digest::render_text` writes (crates/missions/src/digest.rs).
const FULL = [
  "Mission needs you / Görev senden bir şey bekliyor: Backlinks from 15 sites",
  "7 done, 2 blocked, 6 to do, 0 given up, 0 skipped (of 15). 4 sessions, 212 model calls.",
  "Why: A CAPTCHA appeared on the sign-up form.",
  "",
  "Needs you:",
  "- hotel.io: A CAPTCHA appeared on the sign-up form.",
  "- https://india.dev: The site wants the 6-digit code it sent to your phone.",
  "",
  "Given up:",
  "- paywall.biz: no result",
  "",
  "Decisions I made:",
  "- Used the owner's work address for every sign-up.",
  ""
].join("\n");

test("a mission report becomes its outcome, numbers and lists", () => {
  const d = parseMissionDigest(FULL);
  assert.ok(d);
  assert.equal(d.outcome, "blocked");
  assert.equal(d.title, "Backlinks from 15 sites");
  assert.deepEqual(d.counts, { done: 7, blocked: 2, open: 6, failed: 0, skipped: 0, total: 15, sessions: 4, modelSteps: 212 });
  assert.equal(d.reason, "A CAPTCHA appeared on the sign-up form.");
  assert.deepEqual(d.needs.map((r) => r.key), ["hotel.io", "https://india.dev"]);
  assert.equal(d.needs[1].text, "The site wants the 6-digit code it sent to your phone.");
  assert.deepEqual(d.failed, [{ key: "paywall.biz", text: "no result" }]);
  assert.deepEqual(d.decisions, ["Used the owner's work address for every sign-up."]);
});

test("each headline maps to its outcome; singular counts parse", () => {
  const line2 = "1 done, 0 blocked, 0 to do, 0 given up, 0 skipped (of 1). 1 session, 1 model call.";
  assert.equal(parseMissionDigest(`Mission finished / Görev tamamlandı: A\n${line2}\n`)?.outcome, "done");
  assert.equal(parseMissionDigest(`Mission failed / Görev başarısız oldu: A\n${line2}`)?.outcome, "failed");
  assert.equal(parseMissionDigest(`Mission stopped / Görev durduruldu: A\n${line2}`)?.outcome, "cancelled");
  assert.equal(parseMissionDigest(`Mission finished / Görev tamamlandı: A\n${line2}`)?.counts?.sessions, 1);
});

test("an ordinary message, or one with lines a report never writes, stays text", () => {
  assert.equal(parseMissionDigest("Mission finished, thanks!"), null);
  assert.equal(parseMissionDigest("Here is the report:\nMission finished / Görev tamamlandı: A"), null);
  assert.equal(parseMissionDigest("Mission finished / Görev tamamlandı: A\nAnd then I also did something else."), null);
});

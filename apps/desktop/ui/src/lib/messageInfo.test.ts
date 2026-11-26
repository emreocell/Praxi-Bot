import assert from "node:assert/strict";
import { test } from "node:test";
import { durationLabel, messageSourceKind, replyInfoRows, userInfoRows } from "./messageInfo.ts";

const units = { s: "sn", m: "dk", steps: (n: number) => `${n} araç` };
const at = (h: number, m: number, s: number) => new Date(2026, 10, 18, h, m, s).getTime();

test("a bot reply's info lists model, source, duration, times with seconds, steps, computer and tokens", () => {
  const rows = replyInfoRows(
    {
      startedMs: at(21, 27, 5),
      finishedMs: at(21, 27, 17) + 400,
      steps: 3,
      model: "gemini/gemini-3.5-flash-lite",
      computer: "Praxi host",
      promptTokens: 4210,
      completionTokens: 388
    },
    "tr",
    units
  );
  const by = Object.fromEntries(rows.map((r) => [r.key, r.value]));
  assert.deepEqual(
    rows.map((r) => r.key),
    ["model", "source", "duration", "started", "finished", "steps", "where", "tokens"]
  );
  assert.equal(by.source, "Google API");
  assert.equal(by.started, "21:27:05");
  assert.equal(by.finished, "21:27:17");
  assert.equal(by.duration, "12,4 sn");
  assert.equal(by.steps, "3 araç");
  assert.equal(by.tokens, "4.210 / 388");
  assert.match(by.model, /Gemini/);
});

test("what is not known is left out, never invented", () => {
  assert.deepEqual(replyInfoRows({}, "en", units), []);
  const rows = replyInfoRows({ startedMs: at(9, 0, 0), promptTokens: 0, completionTokens: 0 }, "en", units);
  assert.deepEqual(rows.map((r) => r.key), ["started"]);
});

test("the owner's own message shows the sent time with seconds and where it came from", () => {
  const rows = userInfoRows(at(21, 27, 4), "owner", "tr", (k) => k);
  assert.deepEqual(rows, [
    { key: "sent", value: "21:27:04" },
    { key: "source", value: "app" }
  ]);
  assert.deepEqual(userInfoRows(at(1, 2, 3), undefined, "en", (k) => k).map((r) => r.key), ["sent"]);
  assert.equal(durationLabel(65_000, "en", { s: "s", m: "min" }), "1 min 5 s");
});


test("Discord messages show their channel source", () => {
  assert.equal(userInfoRows(at(1, 2, 3), "discord", "en", (k) => k).find((r) => r.key === "source")?.value, "discord");
});

test("the owner's paired Telegram chat shows as Telegram like any other Telegram message", () => {
  assert.equal(messageSourceKind("telegram_owner"), "telegram");
  assert.equal(messageSourceKind("telegram"), "telegram");
});

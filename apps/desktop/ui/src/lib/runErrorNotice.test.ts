import assert from "node:assert/strict";
import { test } from "node:test";
import type { NoticeItem, TextItem, TimelineItem } from "../state/timeline.ts";
import { itemsWithoutDuplicateError, sameFailure } from "./runErrorNotice.ts";

const notice = (message: string, key = "n1"): NoticeItem => ({
  kind: "notice", key, ts: 2, tone: "danger", i18nKey: "chat.notice.error", values: { message }
});
const text: TextItem = { kind: "text", key: "t1", ts: 1, text: "Hi" } as TextItem;
const RAW = "step 'model' failed: invalid request: Antigravity only works with its own tools. (engine_needs_native)";

test("a failed run shows its error once: the notice line that repeats the error card is dropped", () => {
  const items: TimelineItem[] = [text, notice(RAW)];
  assert.deepEqual(itemsWithoutDuplicateError({ items, error: RAW, finishedMs: 3 }), [text]);
});

test("the same failure with an 'Error:' prefix or extra whitespace still counts as the same", () => {
  assert.ok(sameFailure(`Error: ${RAW}`, `  ${RAW}\n`));
  assert.ok(sameFailure("error: error: boom", "Boom"));
  assert.ok(!sameFailure("boom", "bang"));
});

test("a different error on the way (a retry warning) stays; so does everything while the run is still going", () => {
  const other = notice("Rate limited, retrying in 5 s", "n0");
  const items: TimelineItem[] = [other, text, notice(RAW)];
  assert.deepEqual(itemsWithoutDuplicateError({ items, error: RAW, finishedMs: 3 }), [other, text]);
  assert.equal(itemsWithoutDuplicateError({ items, error: undefined, finishedMs: undefined }), items);
  assert.equal(itemsWithoutDuplicateError({ items, error: null, finishedMs: 3 }), items);
});

test("an optional classifier merges two wordings of one known failure (same specific code)", () => {
  const codeOf = (m: string) => (/engine_needs_native/.test(m) ? "engineNeedsNative" : null);
  const items: TimelineItem[] = [notice("invalid request (engine_needs_native)")];
  assert.deepEqual(itemsWithoutDuplicateError({ items, error: RAW, finishedMs: 3 }, codeOf), []);
  assert.equal(itemsWithoutDuplicateError({ items, error: "something else", finishedMs: 3 }, codeOf).length, 1);
});

test("other notices (model fallback, workflow drift) are never touched", () => {
  const fallback: NoticeItem = { ...notice(RAW, "f"), tone: "warning", i18nKey: "chat.notice.modelFallback" };
  assert.deepEqual(itemsWithoutDuplicateError({ items: [fallback], error: RAW, finishedMs: 3 }), [fallback]);
});

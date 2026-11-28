import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import {
  clearScheduleIntent,
  readScheduleIntent,
  setScheduleIntent
} from "./scheduleIntent.ts";

beforeEach(() => clearScheduleIntent());

test("schedule intent is set, read without clearing, then cleared explicitly", () => {
  assert.equal(readScheduleIntent(), null);
  setScheduleIntent({ kind: "draft", botId: "b1", text: "every day at 9" });
  assert.deepEqual(readScheduleIntent(), { kind: "draft", botId: "b1", text: "every day at 9" });
  assert.deepEqual(readScheduleIntent(), { kind: "draft", botId: "b1", text: "every day at 9" });
  setScheduleIntent({ kind: "focus", scheduleId: "s42" });
  assert.deepEqual(readScheduleIntent(), { kind: "focus", scheduleId: "s42" });
  clearScheduleIntent();
  assert.equal(readScheduleIntent(), null);
});
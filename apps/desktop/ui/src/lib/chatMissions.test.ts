import assert from "node:assert/strict";
import { test } from "node:test";
import { addChatMissionLink, parseChatMissions } from "./chatMissions.ts";

test("only well-formed links are read back", () => {
  assert.deepEqual(parseChatMissions(null), []);
  assert.deepEqual(parseChatMissions({ chatId: "c" }), []);
  assert.deepEqual(parseChatMissions([{ chatId: "c", missionId: "m" }, { chatId: 1, missionId: "x" }, "junk"]), [{ chatId: "c", missionId: "m" }]);
});

test("a mission is linked once, and the oldest links go first beyond the cap", () => {
  let all = addChatMissionLink([], { chatId: "c1", missionId: "m1" });
  all = addChatMissionLink(all, { chatId: "c2", missionId: "m1" });
  assert.deepEqual(all, [{ chatId: "c2", missionId: "m1" }]);
  for (let i = 0; i < 250; i += 1) all = addChatMissionLink(all, { chatId: "c", missionId: `m-${i}` });
  assert.equal(all.length, 200);
  assert.equal(all[0].missionId, "m-50");
  assert.equal(all.at(-1)?.missionId, "m-249");
});

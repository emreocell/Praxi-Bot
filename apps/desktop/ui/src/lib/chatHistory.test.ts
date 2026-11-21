import assert from "node:assert/strict";
import test from "node:test";
import { groupHistoryHits, historySnippetParts } from "./chatHistory.ts";

test("history results group chats and highlight Turkish phrases as text", () => {
  const hits = [{ chat_id: "a", id: 1 }, { chat_id: "b", id: 2 }, { chat_id: "a", id: 3 }];
  assert.deepEqual(groupHistoryHits(hits), [[hits[0], hits[2]], [hits[1]]]);
  assert.deepEqual(historySnippetParts("Önce çalışma notları <script>", "calisma"), [
    { text: "Önce ", match: false }, { text: "çalışma", match: true }, { text: " notları <script>", match: false }
  ]);
  assert.deepEqual(historySnippetParts("hello", "missing"), [{ text: "hello", match: false }]);
});

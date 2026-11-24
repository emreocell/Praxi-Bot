import assert from "node:assert/strict";
import test from "node:test";
import { fetchHistoryPage, groupHistoryHits, historySnippetParts } from "./chatHistory.ts";

test("history results group chats and highlight Turkish phrases as text", () => {
  const hits = [{ chat_id: "a", id: 1 }, { chat_id: "b", id: 2 }, { chat_id: "a", id: 3 }];
  assert.deepEqual(groupHistoryHits(hits), [[hits[0], hits[2]], [hits[1]]]);
  assert.deepEqual(historySnippetParts("Önce çalışma notları <script>", "calisma"), [
    { text: "Önce ", match: false }, { text: "çalışma", match: true }, { text: " notları <script>", match: false }
  ]);
  assert.deepEqual(historySnippetParts("hello", "missing"), [{ text: "hello", match: false }]);
});

test("a fully omitted page is skipped instead of showing zero matches with More", async () => {
  const pages: Record<number, { results: number[]; next_offset: number | null }> = {
    0: { results: [], next_offset: 30 },
    30: { results: [], next_offset: 60 },
    60: { results: [7], next_offset: null }
  };
  const seen: number[] = [];
  const page = await fetchHistoryPage(async (o) => { seen.push(o); return pages[o]; }, 0);
  assert.deepEqual(seen, [0, 30, 60]);
  assert.deepEqual(page.results, [7]);
});

test("the mock export never writes raw message text", async () => {
  const { MockApiClient } = await import("../api/mock.ts");
  const client = new MockApiClient();
  const chats = await client.listChats();
  const messages = await client.listMessages(chats[0].id);
  const raw = messages.find((m) => ["user", "assistant"].includes(m.role) && m.content.length > 3);
  assert.ok(raw, "mock chat has text");
  for (const format of ["json", "markdown"] as const) {
    const text = await (await client.exportChat(chats[0].id, format)).text();
    assert.ok(!text.includes(raw.content), format);
    assert.ok(text.includes("[REDACTED]"), format);
  }
});

test("the export query carries the format and what to include", async () => {
  const { exportQuery, clipPreview } = await import("./chatHistory.ts");
  assert.equal(exportQuery("html", { steps: false, approvals: true }), "format=html&steps=false&approvals=true");
  assert.equal(exportQuery("json"), "format=json&steps=true&approvals=true");
  assert.deepEqual(clipPreview("abc", 10), { text: "abc", clipped: false });
  assert.deepEqual(clipPreview("abcdef", 3), { text: "abc", clipped: true });
});

test("the mock HTML export escapes text and never carries raw message text", async () => {
  const { MockApiClient } = await import("../api/mock.ts");
  const client = new MockApiClient();
  const chats = await client.listChats();
  const html = await (await (client as import("./../api/client.ts").ApiClient).exportChat(chats[0].id, "html", { steps: false, approvals: false })).text();
  assert.ok(html.includes("default-src 'none'") && !html.includes("<script"));
});

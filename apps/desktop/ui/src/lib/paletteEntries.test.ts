import assert from "node:assert/strict";
import { test } from "node:test";
import { rank } from "./fuzzy.ts";
import { PALETTE_RECENT_CHATS, recentChats, settingsPageIds } from "./paletteEntries.ts";

const chat = (id: string, at: number, extra: object = {}) => ({
  id,
  bot_id: "b1",
  created_at_ms: at,
  ...extra
});

test("recentChats returns newest first and skips archived chats", () => {
  const out = recentChats([
    chat("old", 1),
    chat("gone", 5, { archived: true }),
    chat("new", 3)
  ]);
  assert.deepEqual(
    out.map((c) => c.id),
    ["new", "old"]
  );
});

test("recentChats keeps at most 20 chats by default", () => {
  const many = Array.from({ length: 30 }, (_, i) => chat(`c${i}`, i));
  const out = recentChats(many);
  assert.equal(out.length, PALETTE_RECENT_CHATS);
  assert.equal(out[0].id, "c29");
  assert.equal(recentChats(many, 3).length, 3);
  assert.equal(recentChats(many, 0).length, 0);
});

test("recentChats does not reorder the caller's array", () => {
  const input = [chat("a", 1), chat("b", 2)];
  recentChats(input);
  assert.deepEqual(
    input.map((c) => c.id),
    ["a", "b"]
  );
});

test("settingsPageIds lists every settings page once", () => {
  const ids = settingsPageIds();
  assert.equal(new Set(ids).size, ids.length);
  assert.ok(ids.length <= 8);
  assert.ok(ids.includes("vault"));
  assert.ok(ids.includes("updates"));
  assert.ok(ids.includes("general"));
  assert.ok(ids.includes("system"));
});

test("the fuzzy ranking finds a chat by a few letters of its title", () => {
  const entries = [
    { title: "Weekly report" },
    { title: "Tax questions" },
    { title: "Travel plan" }
  ];
  const hits = rank(entries, "wkr", (e) => ({ text: e.title }));
  assert.equal(hits[0]?.item.title, "Weekly report");
  assert.equal(rank(entries, "zzz", (e) => ({ text: e.title })).length, 0);
});

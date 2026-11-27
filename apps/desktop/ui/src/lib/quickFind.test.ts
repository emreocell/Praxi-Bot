import assert from "node:assert/strict";
import { test } from "node:test";
import { QUICK_CHATS_MAX, quickFind } from "./quickFind.ts";

const bots = [{ id: "a", name: "Atlas" }, { id: "n", name: "Nova" }, { id: "i", name: "İlknur" }];
const chat = (id: string, bot_id: string, title: string, extra: { archived?: boolean; pinned?: boolean } = {}) => ({ id, bot_id, title, ...extra });

test("quickFind: bots by name, folding case, accents and the Turkish i", () => {
  assert.deepEqual(quickFind(bots, [], "atl").bots.map((b) => b.id), ["a"]);
  assert.deepEqual(quickFind(bots, [], "ilk").bots.map((b) => b.id), ["i"]);
  assert.deepEqual(quickFind(bots, [], "  ").bots, []);
});

test("quickFind: chats by title, with the bot's name as an extra word, never every chat of a bot", () => {
  const chats = [
    chat("1", "a", "Haftalık rapor"),
    chat("2", "n", "Rapor taslağı"),
    chat("3", "a", "Tatil planı"),
    chat("4", "a", "")
  ];
  assert.deepEqual(quickFind(bots, chats, "rapor").chats.map((c) => c.id), ["1", "2"]);
  assert.deepEqual(quickFind(bots, chats, "atlas rapor").chats.map((c) => c.id), ["1"]);
  assert.deepEqual(quickFind(bots, chats, "atlas").chats, [], "the bot row covers a bot's name alone");
});

test("quickFind: pinned first, archived last, otherwise list order, capped", () => {
  const chats = [
    chat("old", "a", "Not 1", { archived: true }),
    chat("x", "a", "Not 2"),
    chat("pin", "n", "Not 3", { pinned: true }),
    ...Array.from({ length: 8 }, (_, i) => chat(`m${i}`, "n", `Not ${i + 10}`))
  ];
  const got = quickFind(bots, chats, "not").chats.map((c) => c.id);
  assert.equal(got[0], "pin");
  assert.equal(got[1], "x");
  assert.equal(got.length, QUICK_CHATS_MAX);
  assert.ok(!got.includes("old"));
});

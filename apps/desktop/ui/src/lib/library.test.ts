import assert from "node:assert/strict";
import { test } from "node:test";
import type { KnowledgeSource, MemoryItem } from "../api/types";
import { libraryShelves, libraryTarget } from "./library.ts";
import { clearPendingBot, openSettingsTarget, peekPendingBot } from "./settingsLink.ts";
import { CARD_META } from "./settingsPages.ts";

const source = (id: string, kind: KnowledgeSource["kind"], documents: number, updated: number): KnowledgeSource =>
  ({ id, kind, label: id, locator: id, documents, chunks: 1, enabled: true, requires_path_selection: false, revision: 1, updated_ms: updated }) as KnowledgeSource;
const memory = (id: string, bot: string | null, extra: Partial<MemoryItem> = {}): MemoryItem =>
  ({ id, bot_id: bot, kind: "bot", content: id, deleted: false, pending: false, ...extra }) as MemoryItem;

test("notes and documents are separate shelves, newest first", () => {
  const shelves = libraryShelves("a", [source("old", "folder", 40, 1), source("note", "note", 1, 5), source("web", "saved_web", 1, 9)], []);
  assert.deepEqual(shelves.documents.map((s) => s.id), ["web", "old"]);
  assert.deepEqual(shelves.notes.map((s) => s.id), ["note"]);
  assert.equal(shelves.documentCount, 41);
});

test("memory counts this bot's and shared memories; waiting ones apart; never another bot's or deleted ones", () => {
  const shelves = libraryShelves("a", [], [
    memory("own", "a"),
    memory("shared", null),
    memory("other", "b"),
    memory("gone", "a", { deleted: true }),
    memory("ask", null, { pending: true })
  ]);
  assert.equal(shelves.remembered, 2);
  assert.equal(shelves.waiting, 1);
  // The separate pending list adds what the memory list left out, without counting twice.
  const both = libraryShelves("a", [], [memory("ask", null, { pending: true })], [
    memory("ask", null, { pending: true }),
    memory("new", "a", { pending: true }),
    memory("theirs", "b", { pending: true })
  ]);
  assert.equal(both.waiting, 2);
});

test("an empty library has empty shelves", () => {
  const shelves = libraryShelves("a", [], []);
  assert.deepEqual([shelves.documents.length, shelves.notes.length, shelves.documentCount, shelves.remembered, shelves.waiting], [0, 0, 0, 0, 0]);
});

test("the Settings links point at real cards and carry the bot once", () => {
  for (const card of ["knowledge", "memory"] as const) {
    const target = libraryTarget(card, "bot-1");
    assert.equal(CARD_META[target.card]?.page, target.page);
  }
  globalThis.window ??= { dispatchEvent: () => true } as unknown as Window & typeof globalThis;
  openSettingsTarget(libraryTarget("memory", "bot-1"));
  assert.equal(peekPendingBot("knowledge"), null);
  assert.equal(peekPendingBot("memory"), "bot-1");
  clearPendingBot("memory");
  assert.equal(peekPendingBot("memory"), null);
});

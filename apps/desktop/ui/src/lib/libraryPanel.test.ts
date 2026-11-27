import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
const panel = read("../components/BotPanelLibrary.tsx");
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const lookup = (tree: Record<string, unknown>, key: string): unknown =>
  key.split(".").reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], tree);

test("the Library tab is shelves in a fixed order: skills, documents, notes, memory, files, chats", () => {
  const order = [...panel.matchAll(/testId="(library-[a-z]+)"/g)].map((m) => m[1]);
  assert.deepEqual(order, ["library-skills", "library-documents", "library-notes", "library-memory", "library-files", "library-chats"]);
  // Every shelf says what it is in one line.
  assert.equal([...panel.matchAll(/<Shelf\b/g)].length, [...panel.matchAll(/\n\s+line=\{t\(/g)].length);
});

test("the shelves sit in two named groups and long lists get a search", () => {
  const knows = panel.indexOf('testId="library-group-knows"');
  const keeps = panel.indexOf('testId="library-group-keeps"');
  assert.ok(knows >= 0 && keeps > knows, "knows, then keeps");
  const at = (id: string) => panel.indexOf(`testId="${id}"`);
  for (const id of ["library-skills", "library-documents", "library-notes", "library-memory"]) assert.ok(at(id) > knows && at(id) < keeps, id);
  for (const id of ["library-files", "library-chats"]) assert.ok(at(id) > keeps, id);
  assert.match(panel, /searchLabel=\{t\("panelTabs\.library\.searchDocuments"\)\}/);
  assert.match(panel, /searchLabel=\{t\("panelTabs\.library\.searchNotes"\)\}/);
});

test("every Library text exists in English and Turkish, and the kinds cover every source kind", () => {
  const keys = new Set([...panel.matchAll(/t\("(library\.[a-zA-Z.]+)"/g)].map((m) => m[1]));
  for (const kind of ["file", "folder", "saved_web", "conversation", "email", "note", "shared_brain"]) keys.add(`library.kind.${kind}`);
  for (const lang of ["en", "tr"]) {
    const tree = locale(lang);
    for (const key of keys) {
      const value = lookup(tree, key) ?? lookup(tree, `${key}_other`);
      assert.equal(typeof value, "string", `${lang}: ${key}`);
    }
  }
});

test("mailboxes moved to the Tools tab; the Library keeps only what the bot knows", () => {
  const botPanel = read("../components/BotPanel.tsx");
  const tools = botPanel.slice(botPanel.indexOf('tab === "tools"'), botPanel.indexOf("<BotPanelLibrary"));
  assert.match(tools, /<BotPanelMail\b/);
  assert.doesNotMatch(panel, /BotPanelMail/);
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { APP_GROUPS } from "./botApps.ts";
import { BOT_EDITOR_SECTIONS } from "./botEditor.ts";
import { INSTRUCTION_EXAMPLES } from "./botInstructions.ts";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const at = (obj: unknown, path: string): unknown =>
  path.split(".").reduce<unknown>((o, k) => (o && typeof o === "object" ? (o as Record<string, unknown>)[k] : undefined), obj);

// The editor is one page: every section is rendered, one after another, and the list on the
// left jumps to them instead of switching tabs.
test("the bot editor renders every section on one page with a jump list", () => {
  const modal = read("../views/BotEditModal.tsx");
  assert.doesNotMatch(modal, /role="tablist"|role="tabpanel"/);
  for (const s of BOT_EDITOR_SECTIONS) {
    assert.ok(modal.includes(`block("${s}"`) || modal.includes(`block(\n            "${s}"`), `section ${s} is on the page`);
  }
  assert.match(modal, /aria-current=\{active \? "location" : undefined\}/);
  // The footer always says where the edits stand, also when nothing changed.
  assert.match(modal, /botEdit\.allSaved/);
  assert.match(modal, /botEdit\.unsavedIn/);
  // Memory and limits are not part of Save, and the page says so.
  assert.match(modal, /SEPARATE_SAVE\[s\]/);
});

test("the instructions box offers examples, and apps are grouped by what they let the bot do", () => {
  assert.match(read("../views/BotEditModal.tsx"), /<BotInstructionsField/);
  assert.match(read("../components/BotEditorFields.tsx"), /appGroups\(servers\)/);
});

test("every section, app group and example has English and Turkish text", () => {
  for (const lang of ["en", "tr"]) {
    const json = locale(lang);
    for (const s of BOT_EDITOR_SECTIONS) {
      assert.equal(typeof at(json, `botEdit.section.${s}`), "string", `${lang} botEdit.section.${s}`);
      assert.equal(typeof at(json, `botEdit.sectionHint.${s}`), "string", `${lang} botEdit.sectionHint.${s}`);
    }
    for (const g of APP_GROUPS) assert.equal(typeof at(json, `botEdit.appGroup.${g}`), "string", `${lang} ${g}`);
    for (const e of INSTRUCTION_EXAMPLES) {
      assert.equal(typeof at(json, `botEdit.instructions.examples.${e}.label`), "string");
      assert.ok(String(at(json, `botEdit.instructions.examples.${e}.text`)).length > 40, `${lang} ${e} is a real example`);
    }
  }
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

type Tree = { [key: string]: string | Tree };
const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
const locale = (lng: string): Tree => JSON.parse(read(`../i18n/locales/${lng}.json`));
const at = (tree: Tree, path: string): string => {
  let node: string | Tree | undefined = tree;
  for (const part of path.split(".")) node = typeof node === "object" ? node[part] : undefined;
  assert.equal(typeof node, "string", `missing ${path}`);
  return node as string;
};

test("the cron hint carries a worked example in every language", () => {
  for (const lng of ["en", "tr", "de"]) {
    assert.match(at(locale(lng), "schedules.form.cronHint"), /0 9 \* \* 1-5/, lng);
  }
});

test("the disabled New schedule button says why, in every language", () => {
  const en = at(locale("en"), "schedules.newNeedsBot");
  for (const lng of ["en", "tr", "de"]) {
    const text = at(locale(lng), "schedules.newNeedsBot");
    assert.ok(text.length > 10, lng);
    if (lng !== "en") assert.notEqual(text, en, `${lng} is still English`);
  }
});

test("the add-server wizard no longer shows developer wording", () => {
  const en = locale("en");
  assert.doesNotMatch(at(en, "addServer.releaseNote"), /published release/i);
  assert.doesNotMatch(at(en, "addServer.lead"), /ticket/i);
  // The host key question explains what a fingerprint is before asking the user to compare it.
  assert.match(at(en, "addServer.hostKeyBody"), /code that identifies the server/);
});

test("the servers list does not call a switch button 'Check now'", () => {
  for (const lng of ["en", "tr", "de"]) {
    const text = at(locale(lng), "servers.checkNow");
    assert.doesNotMatch(text, /check now|şimdi kontrol|jetzt prüfen/i, lng);
  }
});

test("the host key question takes the keyboard when it appears", () => {
  const src = read("../components/AddServerModal.tsx");
  assert.match(src, /keyCard\.current\?\.focus\(\)/);
  assert.match(src, /data-testid="host-key-card"/);
});

test("the schedule header buttons wrap on a narrow window", () => {
  const css = read("../views/SchedulesView.module.css");
  assert.match(css, /\.headerActions \{[^}]*flex-wrap: wrap;/);
});

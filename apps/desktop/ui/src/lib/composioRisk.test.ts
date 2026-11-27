import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
const CLASSES = ["read", "write", "destructive", "money"];

test("every action class has a label and a hint in English and Turkish", () => {
  for (const lng of ["en", "tr"]) {
    const hub = JSON.parse(read(`../i18n/locales/${lng}.json`)).apps.composioHub;
    for (const c of CLASSES) {
      assert.ok(hub.actionClass[c]?.length > 0, `${lng} label ${c}`);
      assert.ok(hub.actionClassHint[c]?.length > 0, `${lng} hint ${c}`);
    }
  }
});

test("the app page shows the class next to each action and the styles exist", () => {
  const page = read("../views/ComposioAppPage.tsx");
  assert.match(page, /data-risk=\{action\.class\}/);
  assert.match(page, /apps\.composioHub\.actionClass\.\$\{action\.class\}/);
  const css = read("../views/AppsGrid.module.css");
  for (const c of ["read", "destructive", "money"]) assert.ok(css.includes(`actionRisk_${c}`), c);
});

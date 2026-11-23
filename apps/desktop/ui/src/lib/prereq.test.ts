import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { PrereqReport } from "../api/connection.ts";
import { adviceIssues, fixLabelKey, leadIssue } from "./prereq.ts";

const report: PrereqReport = {
  details: "webview2: not installed",
  issues: [
    { id: "port_busy", fix: "retry", blocking: true },
    { id: "webview2_missing", fix: "download_webview2", blocking: true },
    { id: "clock_wrong", fix: "retry", blocking: false }
  ]
};

test("the first-run check leads with the first blocking problem and keeps advice apart", () => {
  assert.equal(leadIssue(report)?.id, "port_busy");
  assert.deepEqual(adviceIssues(report).map((i) => i.id), ["clock_wrong"]);
  assert.equal(leadIssue({ details: "", issues: [{ id: "clock_wrong", fix: "retry", blocking: false }] }), null);
  assert.equal(leadIssue(null), null);
});

test("the primary button is named after what it does", () => {
  assert.equal(fixLabelKey("download_webview2"), "errors.prereq.download");
  assert.equal(fixLabelKey("open_data_folder"), "errors.prereq.openDataFolder");
  assert.equal(fixLabelKey("retry"), "errors.startupPanel.retry");
});

test("every issue the app can report has a title and hint in en, tr and de", () => {
  const rust = readFileSync(new URL("../../../src/prereq.rs", import.meta.url), "utf8");
  const ids = [...rust.matchAll(/id: "([a-z0-9_]+)"/g)].map((m) => m[1]);
  assert.ok(ids.length >= 7, "issue ids could not be read from prereq.rs");
  for (const lang of ["en", "tr", "de"]) {
    const prereq = JSON.parse(readFileSync(new URL(`../i18n/locales/${lang}.json`, import.meta.url), "utf8")).errors.prereq;
    for (const id of ids) {
      assert.ok(prereq[id]?.title, `${lang} ${id} title`);
      assert.ok(prereq[id]?.hint, `${lang} ${id} hint`);
    }
  }
});

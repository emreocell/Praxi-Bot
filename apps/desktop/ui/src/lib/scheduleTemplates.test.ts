import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseTime } from "./scheduleBuilder.ts";
import {
  TEMPLATE_LANGS,
  templateText,
  type ScheduleTemplate
} from "./scheduleTemplates.ts";

const file = JSON.parse(
  readFileSync(new URL("../data/scheduleTemplates.json", import.meta.url), "utf8")
) as { templates: ScheduleTemplate[] };

test("the five ready-made jobs exist with real text in en, tr and de", () => {
  const ids = file.templates.map((t) => t.id);
  assert.deepEqual(ids, [
    "daily-briefing",
    "weekly-review",
    "inbox-triage",
    "price-watcher",
    "backup-reminder"
  ]);
  for (const tpl of file.templates) {
    assert.notEqual(parseTime(tpl.time), null, tpl.id);
    const en = tpl.text.en!;
    for (const lang of TEMPLATE_LANGS) {
      const x = tpl.text[lang];
      assert.ok(x && x.name && x.blurb && x.prompt, `${tpl.id}/${lang}`);
      if (lang !== "en") {
        assert.notEqual(x.prompt, en.prompt, `${tpl.id}/${lang} is translated`);
      }
    }
  }
});

test("an unknown language falls back to English", () => {
  const tpl = file.templates[0]!;
  assert.equal(templateText(tpl, "fr").name, "Daily briefing");
  assert.equal(templateText(tpl, "tr-TR").name, "Günlük özet");
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { formatRunPreview } from "./scheduleTime.ts";

test("run preview formats with a weekday in every UI locale and a time zone", () => {
  const ms = Date.UTC(2026, 10, 23, 6, 0);
  for (const locale of ["en", "tr", "de"]) {
    const text = formatRunPreview(ms, locale, "Europe/Istanbul");
    assert.match(text, /2026/);
    assert.match(text, /09[:.]00|9[:.]00/);
  }
});

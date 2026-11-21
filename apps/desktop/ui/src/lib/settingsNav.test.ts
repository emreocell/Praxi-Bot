import assert from "node:assert/strict";
import { test } from "node:test";
import { SETTINGS_SECTIONS } from "../views/settings/types.ts";
import { groupedSections, SETTINGS_NAV_GROUPS } from "./settingsNav.ts";

test("every settings section is in exactly one nav group", () => {
  const grouped = groupedSections();
  assert.equal(new Set(grouped).size, grouped.length);
  assert.deepEqual([...grouped].sort(), [...SETTINGS_SECTIONS].sort());
  assert.ok(SETTINGS_NAV_GROUPS.every((g) => g.sections.length > 0));
});

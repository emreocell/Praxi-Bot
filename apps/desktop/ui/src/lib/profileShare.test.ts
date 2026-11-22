import assert from "node:assert/strict";
import { test } from "node:test";
import {
  initialLevel,
  noteIsWarning,
  SHARE_LEVELS,
  type SharedProfile,
  templateDescription,
  templateName
} from "./profileShare.ts";

const profile = {
  format: "praxi-bot-profile",
  format_version: 1,
  meta: {
    id: "researcher",
    names: { en: "Researcher", tr: "Araştırmacı", de: "Rechercheur" },
    descriptions: { en: "Reads the web.", tr: "Web'i okur." }
  },
  bot: { name: "Researcher", persona: "" }
} as unknown as SharedProfile;

test("a profile from a file starts at the safest ceiling, never above what a template asks", () => {
  assert.equal(initialLevel("file", "full"), "read-only");
  assert.equal(initialLevel("template", "read-and-draft"), "read-and-draft");
  // even a template that asked for everything starts below "Free"
  assert.equal(initialLevel("template", "full"), "approve-destructive");
  assert.equal(initialLevel("template"), "approve-destructive");
  assert.equal(SHARE_LEVELS[0], "read-only");
});

test("gallery text follows the UI language and falls back to English", () => {
  assert.equal(templateName(profile, "tr-TR"), "Araştırmacı");
  assert.equal(templateName(profile, "de"), "Rechercheur");
  assert.equal(templateName(profile, "fr"), "Researcher");
  // no German description: English
  assert.equal(templateDescription(profile, "de"), "Reads the web.");
});

test("only lowered levels and rules are shown as warnings", () => {
  assert.equal(noteIsWarning("level_lowered"), true);
  assert.equal(noteIsWarning("rule_lowered"), true);
  assert.equal(noteIsWarning("tool_dropped_unknown"), false);
});

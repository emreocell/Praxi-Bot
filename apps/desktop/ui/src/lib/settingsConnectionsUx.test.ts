import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

test("custom CalDAV keeps name and email fields reachable", () => {
  const src = read("../views/settings/CalendarCard.tsx");
  assert.match(src, /count=\{isOther \? 2 : 3\}/);
  assert.match(src, /id="calendar-name"/);
  assert.match(src, /id="calendar-email"/);
  assert.doesNotMatch(
    src,
    /\{isOther \? \([\s\S]*?<FormField[\s\S]*?calendar-url[\s\S]*?\) : \([\s\S]*?SettingsDisclosure/
  );
});

test("email channel save bar allows turning off without a mailbox", () => {
  const src = read("../views/settings/EmailChannelCard.tsx");
  assert.match(src, /disabled=\{!dirty \|\| \(form\.enabled && !form\.account\)\}/);
  assert.match(src, /if \(dirty\)[\s\S]*?if \(!form\.enabled\)/);
});

test("email channel controls match label htmlFor ids", () => {
  const src = read("../views/settings/EmailChannelCard.tsx");
  for (const id of ["email-channel-account", "email-channel-folder", "email-channel-bot"]) {
    assert.match(src, new RegExp(`id="${id}"`));
  }
});

test("FieldHelp popover exits on Tab when there are no focusables", () => {
  const src = read("../components/Popover.tsx");
  assert.match(src, /role === "dialog" && all\.length === 0/);
});

test("manual-save connection cards register a leave guard", () => {
  for (const card of ["../views/settings/EmailChannelCard.tsx", "../views/settings/MailGuardCard.tsx"]) {
    assert.match(read(card), /useLeaveGuard\(/, card);
  }
  assert.match(read("../views/SettingsModal.tsx"), /leave\.attempt\(/);
});
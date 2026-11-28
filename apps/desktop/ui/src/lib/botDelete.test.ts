import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { BotDeletePreview, BotDeleteReport, Run } from "../api/types.ts";
import {
  BACKUP_GUIDE_URL,
  deletableInboxes,
  deleteBlock,
  deleteOptions,
  deletedCounts,
  focusAfterStep,
  keptMailboxes,
  leftovers,
  nameMatches,
  reportCounts,
  STOPPED_MISSION_STATES,
  unfinishedRunIds
} from "./botDelete.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const at = (obj: unknown, path: string): unknown =>
  path.split(".").reduce<unknown>((o, k) => (o && typeof o === "object" ? (o as Record<string, unknown>)[k] : undefined), obj);

const preview = (over: Partial<BotDeletePreview> = {}): BotDeletePreview => ({
  bot_id: "b1",
  name: "Nova",
  chats: 0,
  messages: 0,
  runs: 0,
  run_steps: 0,
  approvals: 0,
  schedules: 0,
  webhooks: 0,
  missions: 0,
  memories: 0,
  grants: 0,
  channels: [],
  mailboxes: [],
  active_runs: 0,
  running_missions: 0,
  waiting_missions: 0,
  workspace_folder: null,
  ...over
});

const report = (over: Partial<BotDeleteReport> = {}): BotDeleteReport => ({
  name: "Nova",
  chats: 0,
  messages: 0,
  runs: 0,
  schedules: 0,
  webhooks: 0,
  missions: 0,
  memories: 0,
  mailboxes_removed: 0,
  mailboxes_kept: 0,
  inboxes_deleted: [],
  inboxes_not_deleted: [],
  cleanup_pending: [],
  ...over
});

// ---- step 1: what goes, what stays ------------------------------------------------------------

test("step 1 lists only what the bot has, with its counts", () => {
  assert.deepEqual(deletedCounts(preview()), []);
  const p = preview({
    chats: 2,
    messages: 40,
    runs: 5,
    schedules: 1,
    memories: 3,
    grants: 4,
    channels: [
      { channel: "telegram", chats: 1 },
      { channel: "discord", chats: 2 }
    ],
    mailboxes: [
      { name: "praxi", address: "praxi@agentmail.to", provider: "agentmail", fate: "removed", inbox_deletable: true },
      { name: "team", address: "team@example.com", provider: "imap_smtp", fate: "handed_over", inbox_deletable: false }
    ]
  });
  assert.deepEqual(deletedCounts(p), [
    { key: "chats", count: 2 },
    { key: "messages", count: 40 },
    { key: "runs", count: 5 },
    { key: "schedules", count: 1 },
    { key: "memories", count: 3 },
    { key: "grants", count: 4 },
    { key: "channels", count: 3 },
    { key: "mailboxes", count: 1 } // only the account that goes; the shared one stays
  ]);
});

test("a shared or only-linked mailbox is listed as staying, never as deleted", () => {
  const p = preview({
    mailboxes: [
      { name: "a", address: "a@x", provider: "imap_smtp", fate: "removed", inbox_deletable: false },
      { name: "b", address: "b@x", provider: "imap_smtp", fate: "handed_over", inbox_deletable: false },
      { name: "c", address: "c@x", provider: "imap_smtp", fate: "unlinked", inbox_deletable: false }
    ]
  });
  assert.deepEqual(keptMailboxes(p).map((m) => m.name), ["b", "c"]);
});

test("the inbox choice appears only for an AgentMail inbox only this bot used, and is off unless ticked", () => {
  const none = preview({
    mailboxes: [{ name: "t", address: "t@x", provider: "imap_smtp", fate: "removed", inbox_deletable: false }]
  });
  assert.deepEqual(deletableInboxes(none), []);
  assert.deepEqual(deleteOptions(none, true), { deleteInbox: false }); // nothing to delete: never sent
  const some = preview({
    mailboxes: [{ name: "p", address: "praxi@agentmail.to", provider: "agentmail", fate: "removed", inbox_deletable: true }]
  });
  assert.equal(deletableInboxes(some)[0]?.address, "praxi@agentmail.to");
  assert.deepEqual(deleteOptions(some, false), { deleteInbox: false }); // off by default
  assert.deepEqual(deleteOptions(some, true), { deleteInbox: true });
});

test("a working bot blocks Continue, with the reason; stopping its runs lets it go on", () => {
  assert.equal(deleteBlock(preview()), null);
  assert.equal(deleteBlock(preview({ active_runs: 1 })), "run");
  assert.equal(deleteBlock(preview({ running_missions: 1 })), "mission");
  assert.equal(deleteBlock(preview({ active_runs: 2, running_missions: 1 })), "run");
  // A mission waiting for a timer, a mail or a webhook would wake on its own: cancel it first.
  assert.equal(deleteBlock(preview({ waiting_missions: 1 })), "waiting");
  assert.equal(deleteBlock(preview({ running_missions: 1, waiting_missions: 1 })), "mission");
  assert.ok(STOPPED_MISSION_STATES.has("running") && STOPPED_MISSION_STATES.has("waiting"));
  assert.ok(!STOPPED_MISSION_STATES.has("paused"));
  const runs = [
    { id: "r1", bot_id: "b1", state: "running" },
    { id: "r2", bot_id: "b1", state: "succeeded" },
    { id: "r3", bot_id: "b1", state: "paused" },
    { id: "r4", bot_id: "other", state: "running" }
  ] as Run[];
  assert.deepEqual(unfinishedRunIds(runs, "b1"), ["r1", "r3"]);
});

// ---- step 2: type the name -----------------------------------------------------------------

test("step 2: the red button needs the bot's own name, nothing else", () => {
  assert.equal(nameMatches("", "Nova"), false);
  assert.equal(nameMatches("nova", "Nova"), false); // exact: case counts
  assert.equal(nameMatches("Nov", "Nova"), false);
  assert.equal(nameMatches("Nova", "Nova"), true);
  assert.equal(nameMatches("  Nova ", "Nova"), true); // spaces around it do not count
  assert.equal(nameMatches("", ""), false); // a bot without a name is never matched by an empty box
});

test("what was removed is summarised for the toast, without zeros", () => {
  const r = report({ chats: 1, messages: 8, webhooks: 1, memories: 2, mailboxes_removed: 1 });
  assert.deepEqual(reportCounts(r), [
    { key: "chats", count: 1 },
    { key: "messages", count: 8 },
    { key: "webhooks", count: 1 },
    { key: "memories", count: 2 },
    { key: "mailboxes", count: 1 }
  ]);
  assert.deepEqual(leftovers(r), { steps: [], inboxes: [] });
});

test("what was left after the delete is named, so the toast can offer to try again", () => {
  const r = report({
    cleanup_pending: ["files", "mailboxes"],
    inboxes_not_deleted: [{ account: "praxi", address: "praxi@agentmail.to", reason: "provider down" }]
  });
  assert.deepEqual(leftovers(r), { steps: ["files", "mailboxes"], inboxes: ["praxi@agentmail.to"] });
  const src = read("../components/DeleteBotDialog.tsx");
  assert.match(src, /botDelete\.leftover\.text/);
  // Trying again sends the same request (the daemon finishes what a gone bot left).
  assert.match(src, /client\.deleteBot\(bot\.id, opts\)\.then\(/);
  for (const lang of ["en", "tr"]) {
    const json = locale(lang);
    for (const step of ["knowledge", "memory", "files", "secrets", "missions", "schedules", "links", "mailboxes"]) {
      assert.equal(typeof at(json, `botDelete.step.${step}`), "string", `${lang} ${step}`);
    }
  }
});

test("Back from step 2 clears the typed name, and the name to type is the one the daemon reported", () => {
  const src = read("../components/DeleteBotDialog.tsx");
  assert.match(src, /const back = \(\) => \{\s*setTyped\(""\);/);
  assert.match(src, /data-testid="delete-bot-back" onClick=\{back\}/);
  assert.match(src, /const name = preview\?\.name \?\? bot\.name;/);
  // "Stop it first" also cancels a waiting mission.
  assert.match(src, /STOPPED_MISSION_STATES\.has\(m\.state\)/);
});

test("focus moves to the name box on step 2 and back to Cancel on the way back to step 1", () => {
  assert.equal(focusAfterStep("review", "confirm"), "name");
  assert.equal(focusAfterStep("confirm", "review"), "cancel");
  // Opening on step 1 leaves it to the dialog's own autofocus.
  assert.equal(focusAfterStep(null, "review"), null);
  assert.equal(focusAfterStep("review", "review"), null);
  const src = read("../components/DeleteBotDialog.tsx");
  assert.match(src, /focusAfterStep\(shownStep\.current, step\)/);
  assert.match(src, /if \(target === "cancel"\) cancelButton\.current\?\.focus\(\);/);
  assert.match(src, /<Button ref=\{cancelButton\} variant="ghost"[^>]*onClick=\{onClose\}/);
});

// ---- the dialog and its wiring --------------------------------------------------------------

test("the dialog has the two steps in order: what goes, then the typed name and the red button", () => {
  const src = read("../components/DeleteBotDialog.tsx");
  // Step 1 shows counts, what stays, the optional inbox, the backup advice; Continue goes to step 2.
  for (const id of ["delete-bot-counts", "delete-bot-stays", "delete-bot-inbox", "delete-bot-backup", "delete-bot-continue"]) {
    assert.ok(src.includes(`"${id}"`), id);
  }
  assert.match(src, /onClick=\{\(\) => setStep\("confirm"\)\}/);
  // Continue waits for the preview and for a bot that is not working.
  assert.match(src, /disabled=\{!preview \|\| block !== null \|\| busy\}/);
  // Step 2: the delete button is danger-toned and disabled until the name matches.
  assert.match(src, /variant="danger"[\s\S]{0,120}disabled=\{deleting \|\| !nameMatches\(typed, name\)\}/);
  // The delete call also checks the name, so Enter in the box cannot skip it.
  assert.match(src, /!nameMatches\(typed, name\)\) return;/);
  // The inbox choice starts off and is sent only as chosen.
  assert.match(src, /useState\(false\);\n {2}const \[typed/);
  assert.match(src, /deleteOptions\(preview, alsoInbox\)/);
  // The backup advice links to the guide and never blocks.
  assert.match(src, /openExternal\(BACKUP_GUIDE_URL\)/);
  assert.match(BACKUP_GUIDE_URL, /docs\/guide\/backup\.md$/);
});

test("after the delete the caller navigates away and a toast says what was removed", () => {
  const src = read("../components/DeleteBotDialog.tsx");
  assert.match(src, /toast\.show\(/);
  assert.match(src, /botDelete\.done/);
  assert.match(src, /onDeleted\(report\)/);
  // The lists that showed its chats and runs are refreshed.
  assert.match(src, /Promise\.all\(\[refreshBots\(\), refreshChats\(\), refreshRuns\(\)\]\)/);
  const modal = read("../views/BotEditModal.tsx");
  assert.match(modal, /onDeleted=\{\(\) => \{\s*setConfirmDelete\(false\);\s*onDeleted\(bot\.id\);/);
  // The app leaves the deleted bot for the home screen.
  assert.match(read("../App.tsx"), /onDeleted=\{\(\) => \{\s*setBotEditOpen\(false\);\s*setRoute\(\{ kind: "home" \}\);/);
});

test("every string of the dialog has English and Turkish text", () => {
  const src = read("../components/DeleteBotDialog.tsx");
  const keys = new Set<string>();
  for (const m of src.matchAll(/t\(\s*"(botDelete\.[A-Za-z.]+)"/g)) keys.add(m[1]!);
  assert.ok(keys.size > 20, "the dialog uses its texts");
  const dynamic = [
    ...["chats", "messages", "runs", "schedules", "webhooks", "missions", "memories", "grants", "channels", "mailboxes"].map((k) => `botDelete.item.${k}`),
    ...["removed", "handed_over", "unlinked"].map((k) => `botDelete.mailbox.${k}`),
    "botDelete.working.run",
    "botDelete.working.mission",
    "botDelete.working.waiting"
  ];
  for (const lang of ["en", "tr"]) {
    const json = locale(lang);
    for (const key of [...keys, ...dynamic]) assert.equal(typeof at(json, key), "string", `${lang} ${key}`);
  }
  // The old "cannot be deleted" texts are gone from every language.
  for (const lang of ["en", "tr", "de", "es", "fr", "id", "pt"]) {
    const edit = at(locale(lang), "botEdit") as Record<string, unknown>;
    for (const k of ["deleteBlocked", "deleteBlockedRuns", "deleteConfirmBody", "deleteHint"]) assert.equal(edit[k], undefined, `${lang} botEdit.${k}`);
  }
});

test("the daemon's delete is owner-only and the client sends the inbox choice as a query", () => {
  const client = read("../api/client.ts");
  assert.match(client, /botDeletePreview\(id: string\)/);
  assert.match(client, /\/bots\/\$\{encodeURIComponent\(id\)\}\/delete-preview/);
  assert.match(client, /delete_inbox: "true"/);
});

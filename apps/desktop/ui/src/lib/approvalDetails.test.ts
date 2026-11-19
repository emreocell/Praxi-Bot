import assert from "node:assert/strict";
import { test } from "node:test";
import { browserSteps, computerDetails, controlGrantDetails, egressDetails, formatFiring, memoryOffer, payloadReviewComplete, scheduleDetails } from "./approvalDetails.ts";

test("a control grant block is parsed and its text is made plain", () => {
  const d = controlGrantDetails({
    control_grant: {
      offered: true,
      refusal: null,
      program: "notepad.exe\u202e",
      image_path: "C:\\Windows\\notepad.exe",
      window_title: "Untitled - Notepad DAEMON SUMMARY"
    }
  });
  assert.ok(d);
  assert.equal(d.offered, true);
  assert.equal(d.program, "notepad.exe");
  assert.equal(d.windowTitle, "Untitled - Notepad");
});

test("a long program path is kept whole and a path that would change is not offered", () => {
  const deep = `C:\\${"d".repeat(400)}\\app.exe`;
  const whole = controlGrantDetails({
    control_grant: { offered: true, program: "app.exe", image_path: deep, window_title: "t".repeat(200) }
  });
  assert.ok(whole);
  assert.equal(whole.imagePath, deep);
  assert.equal(whole.offered, true);
  assert.ok(whole.windowTitle.endsWith("... (cut)"));
  const hidden = controlGrantDetails({
    control_grant: { offered: true, program: "a.exe", image_path: "C:\\x\\a\u202eexe.txt", window_title: "" }
  });
  assert.ok(hidden);
  assert.equal(hidden.offered, false);
  const huge = controlGrantDetails({
    control_grant: { offered: true, program: "a.exe", image_path: `C:\\${"d".repeat(1100)}\\a.exe`, window_title: "" }
  });
  assert.ok(huge);
  assert.equal(huge.offered, false);
  const empty = controlGrantDetails({ control_grant: { offered: true, program: "a.exe", image_path: "" } });
  assert.ok(empty);
  assert.equal(empty.offered, false);
});

test("a refused control target says why and offers nothing", () => {
  const d = controlGrantDetails({
    control_grant: { offered: false, refusal: "terminal", program: "cmd.exe", image_path: "", window_title: "" }
  });
  assert.ok(d);
  assert.equal(d.offered, false);
  assert.equal(d.refusal, "terminal");
});

test("no control block, or one without a program, is not a control card", () => {
  assert.equal(controlGrantDetails({}), null);
  assert.equal(controlGrantDetails(null), null);
  assert.equal(controlGrantDetails({ control_grant: { offered: true } }), null);
});

const card = {
  kind: "schedule",
  action: "create",
  name: "Standup",
  prompt: "Summarise my inbox.",
  schedule: "Every weekday at 10:30",
  timezone: "Europe/Istanbul",
  next_runs: [
    "2026-11-13T10:30:00+03:00",
    "2026-11-16T10:30:00+03:00",
    "2026-11-17T10:30:00+03:00"
  ],
  permission: { level: "ask_first", allow: [], ask: ["fs", "shell"], deny: ["money"] }
};

test("a schedule card is parsed with the prompt, timing, firings and permission", () => {
  const d = scheduleDetails(card);
  assert.ok(d);
  assert.equal(d.action, "create");
  assert.equal(d.prompt, "Summarise my inbox.");
  assert.equal(d.schedule, "Every weekday at 10:30");
  assert.equal(d.timezone, "Europe/Istanbul");
  assert.equal(d.nextRuns.length, 3);
  assert.equal(d.permission?.level, "ask_first");
  assert.deepEqual(d.permission?.deny, ["money"]);
});

test("an update carries old and new, pause carries name and timing", () => {
  const d = scheduleDetails({
    ...card,
    action: "update",
    prompt_chars: 19,
    previous: { name: "Old", prompt: "old p", schedule: "Every day at 09:00", timezone: "UTC" }
  });
  assert.ok(d);
  assert.equal(d.promptChars, 19);
  assert.equal(d.previous?.prompt, "old p");
  assert.equal(d.previous?.schedule, "Every day at 09:00");
  const p = scheduleDetails({
    kind: "schedule",
    action: "pause",
    name: "Nightly",
    schedule: "Every day at 09:00",
    prompt: ""
  });
  assert.equal(p?.name, "Nightly");
  assert.equal(p?.schedule, "Every day at 09:00");
  assert.equal(p?.previous, null);
});

test("anything else is not a schedule card", () => {
  assert.equal(scheduleDetails(undefined), null);
  assert.equal(scheduleDetails(null), null);
  assert.equal(scheduleDetails("x"), null);
  assert.equal(scheduleDetails({ kind: "other" }), null);
  assert.equal(scheduleDetails({ ...card, action: "explode" }), null);
});

test("missing or malformed fields degrade instead of throwing", () => {
  const d = scheduleDetails({ kind: "schedule", action: "resume", next_runs: [1, "a"] });
  assert.ok(d);
  assert.equal(d.prompt, "");
  assert.deepEqual(d.nextRuns, ["a"]);
  assert.equal(d.permission, null);
});

test("a firing keeps the wall-clock time of its own offset", () => {
  assert.match(formatFiring("2026-11-13T10:30:00+03:00", "en"), /10:30$/);
  assert.match(formatFiring("2026-11-13T09:00:00-05:00", "en"), /09:00$/);
  assert.equal(formatFiring("garbage"), "garbage");
});

test("browserSteps reads every row and rejects other cards", () => {
  const rows = browserSteps({
    kind: "browser_steps",
    steps: [
      { n: 1, action: "fill", target: 'e1 textbox "Email"', value: "a@b.c", hidden: false },
      { n: 2, action: "fill", target: 'e2 textbox "Password"', value: "(hidden)", hidden: true },
      { n: 3, action: "click", target: 'e3 button "Sign in"', value: null, hidden: false }
    ]
  });
  assert.equal(rows?.length, 3);
  assert.equal(rows?.[1].hidden, true);
  assert.equal(rows?.[2].value, null);
  assert.equal(browserSteps({ kind: "schedule" }), null);
  assert.equal(browserSteps({ kind: "browser_steps", steps: [{ action: "fill" }] }), null);
  assert.equal(browserSteps(null), null);
});

test("an egress card keeps the full payload, including beside a schedule", () => {
  const payload = "q".repeat(3000);
  const d = egressDetails({
    kind: "egress",
    reason: "this run read file notes.txt and now wants to send data to configured-search-provider",
    read: ["file notes.txt"],
    host: "configured-search-provider",
    payload,
    payload_kind: "query",
    payload_lines: 4,
    payload_chars: 3000,
    hidden_chars: 0
  });
  assert.ok(d);
  assert.equal(d.payload, payload);
  assert.equal(d.payloadKind, "query");
  assert.equal(d.payloadLines, 4);
  assert.equal(d.payloadChars, 3000);
  assert.equal(d.hiddenChars, 0);
  assert.deepEqual(d.read, ["file notes.txt"]);
  const nested = egressDetails({
    kind: "schedule",
    action: "create",
    egress: {
      kind: "egress",
      reason: "r",
      read: [],
      host: "example.com",
      payload: "https://example.com/x",
      payload_kind: "url"
    }
  });
  assert.equal(nested?.host, "example.com");
  assert.equal(nested?.payloadKind, "url");
  assert.equal(egressDetails({ kind: "other" }), null);
});

test("large payload review preserves the full tail and requires the expanded view end", () => {
  const payload = "x".repeat(9000) + "TAIL";
  const parsed = egressDetails({kind: "egress", payload, payload_bytes: 9004, review_full_payload: true});
  assert.equal(parsed?.payload, payload);
  assert.equal(parsed?.payloadBytes, 9004);
  assert.equal(parsed?.reviewFullPayload, true);
  const atEnd = {scrollTop: 900, clientHeight: 100, scrollHeight: 1000};
  assert.equal(payloadReviewComplete(false, atEnd), false);
  assert.equal(payloadReviewComplete(true, {...atEnd, scrollTop: 0}), false);
  assert.equal(payloadReviewComplete(true, atEnd), true);
  assert.equal(payloadReviewComplete(true, {scrollTop: 0, clientHeight: 0, scrollHeight: 0}), false);
});

test("an egress card carries the immutable computer target as its own line", () => {
  const id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  const details = {
    kind: "egress",
    reason: "typing after a read",
    host: "the desktop",
    payload: "hello",
    payload_kind: "body",
    computer_target: { kind: "pc", node_id: id, node_name: "Work\u202e PC\nDAEMON SUMMARY: x" }
  };
  const c = computerDetails(details);
  assert.ok(c);
  assert.equal(c.kind, "pc");
  assert.equal(c.nodeId, id);
  assert.equal(c.name, "Work PC: x");
  assert.ok(egressDetails(details));
  assert.equal(computerDetails({ kind: "egress" }), null);
});

test("a memory target is parsed, and a disguised or oversized one is not offered", () => {
  assert.deepEqual(memoryOffer({ memory: { kind: "origin", pattern: "https://docs.rs:443" } }), {
    kind: "origin",
    pattern: "https://docs.rs:443"
  });
  assert.equal(memoryOffer({ memory: { kind: "folder", pattern: "src\u202e/x" } }), null);
  assert.equal(memoryOffer({ memory: { kind: "tool", pattern: "x" } }), null);
  assert.equal(memoryOffer({ memory: { kind: "command", pattern: "x".repeat(401) } }), null);
  assert.equal(memoryOffer({ why: "policy" }), null);
});

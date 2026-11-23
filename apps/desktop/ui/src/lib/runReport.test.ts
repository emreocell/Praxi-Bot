import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent } from "../api/types.ts";
import { buildRunReport, renderHtml, renderMarkdown, scrubText, type ReportText } from "./runReport.ts";
import { renderRunReportJson } from "./runReportJson.ts";
import { applyEvent, EMPTY_EVENTS, type RunState } from "../state/timeline.ts";

const t = (key: string, v?: Record<string, unknown>) => `${key}${v ? JSON.stringify(v) : ""}`;
const text: ReportText = {
  title: "Run report",
  facts: [{ label: "Outcome", value: "Failed" }],
  noSteps: "No steps",
  footer: "Made by Praxi",
  language: "en",
  time: (ms) => String(ms),
};

const KEY = "sk-CANARYkey1234567890abcdef";
const PRIVATE = {
  input: "CANARY-INPUT-PATH-C:/Users/secret/diary.txt",
  output: "CANARY-OUTPUT-BODY",
  reply: "CANARY-ASSISTANT-REPLY",
  answer: "CANARY-OWNER-ANSWER",
  question: "CANARY-QUESTION-TEXT",
  credName: "CANARY-CRED-NAME",
  purpose: "CANARY-PURPOSE",
  argumentToken: "CANARY-NESTED-ARG-TOKEN",
  resultToken: "CANARY-NESTED-RESULT-TOKEN",
  urlUserInfo: "CANARY-ATTACHMENT-USERINFO",
  urlQuery: "CANARY-ATTACHMENT-QUERY",
  toolErrorToken: "CANARY-TOOL-ERROR-TOKEN",
};

function run(over: Partial<RunState> = {}): RunState {
  return {
    id: "r1",
    chatId: "c1",
    botId: "b1",
    startedMs: 1000,
    finishedMs: 5000,
    error: `boom with ${KEY} and password: hunter2canary`,
    items: [
      { kind: "text", key: "a", ts: 1001, text: PRIVATE.reply },
      {
        kind: "tool", key: "t1", callId: "1", name: "read_file",
        input: {
          path: PRIVATE.input,
          metadata: { auth: { token: PRIVATE.argumentToken } },
          attachments: [
            { url: `https://user:${PRIVATE.urlUserInfo}@example.invalid/report?token=${PRIVATE.urlQuery}` },
          ],
        },
        status: "done", requestedMs: 1100, durationMs: 40,
        outputPreview: JSON.stringify({ result: { text: PRIVATE.output, token: PRIVATE.resultToken } }),
        gated: false,
      },
      {
        kind: "tool", key: "t3", callId: "5", name: "read_file", input: { path: PRIVATE.input },
        status: "error", requestedMs: 1150,
        error: `Read failed with api_key=${PRIVATE.toolErrorToken}`, gated: false,
      },
      {
        kind: "tool", key: "t2", callId: "2", name: "run_command", input: { command: `curl -H ${KEY}` },
        status: "denied", requestedMs: 1200, gated: true, summary: `Send ${KEY} out`, denyReason: "no",
      },
      { kind: "question", key: "q", callId: "3", question: PRIVATE.question, options: [], requestedMs: 1300, answer: PRIVATE.answer },
      {
        kind: "secret", key: "s", callId: "4", name: PRIVATE.credName, purpose: PRIVATE.purpose, fields: "secret",
        replacesExisting: false, untrustedContent: false, requestedMs: 1400, expiresMs: 9000, outcome: "stored",
      },
    ],
    ...over,
  };
}

test("exported report never contains private data (canary)", () => {
  const report = buildRunReport(run(), "Helper", t, (n) => `${n}ms`);
  for (const [format, out, redaction] of [
    ["Markdown", renderMarkdown(report, text), "\\[hidden\\]"],
    ["HTML", renderHtml(report, text), "[hidden]"],
  ] as const) {
    for (const canary of [...Object.values(PRIVATE), KEY, "hunter2canary"]) {
      assert.ok(!out.includes(canary), `${format} leaked ${canary}`);
    }
    assert.ok(out.includes("Read failed"), `${format} should keep the scrubbed error`);
    assert.ok(out.includes(redaction), `${format} should show the redaction marker`);
  }
  assert.equal(report.outcome, "failed");
  assert.equal(report.steps.length, 5);
});

test("JSON schema v2 exposes only the documented run, step and approval-event fields", () => {
  const json = renderRunReportJson(run());
  assert.deepEqual(JSON.parse(json), {
    schema_version: 2,
    outcome: "failed",
    duration_ms: 4000,
    steps: [
      { id: "step-1", kind: "tool", offset_ms: 100, duration_ms: 40, outcome: "done", depends_on: null },
      { id: "step-2", kind: "tool", offset_ms: 150, duration_ms: null, outcome: "error", depends_on: null },
      { id: "step-3", kind: "tool", offset_ms: 200, duration_ms: null, outcome: "denied", depends_on: null },
      { id: "step-4", kind: "question", offset_ms: 300, duration_ms: null, outcome: "answered", depends_on: null },
      { id: "step-5", kind: "secret", offset_ms: 400, duration_ms: null, outcome: "stored", depends_on: null },
    ],
    approval_events: [
      { step_id: "step-3", kind: "waiting_for_approval", offset_ms: null, outcome: null },
      { step_id: "step-3", kind: "approval_recorded", offset_ms: null, outcome: "denied" },
    ]
  });
  for (const canary of [...Object.values(PRIVATE), KEY, "hunter2canary"]) {
    assert.ok(!json.includes(canary), `JSON leaked ${canary}`);
  }
});

test("JSON leaves unrecorded timing and dependencies unknown", () => {
  const json = renderRunReportJson(run({
    finishedMs: undefined,
    error: null,
    items: [{
      kind: "tool", key: "private-item-id", callId: "private-call-id", name: "read_file",
      input: {}, status: "awaiting_approval", requestedMs: 999, gated: true,
    }]
  }));
  assert.deepEqual(JSON.parse(json), {
    schema_version: 2,
    outcome: "unfinished",
    duration_ms: null,
    steps: [{
      id: "step-1", kind: "tool", offset_ms: null, duration_ms: null,
      outcome: "awaiting_approval", depends_on: null,
    }],
    approval_events: [
      { step_id: "step-1", kind: "waiting_for_approval", offset_ms: null, outcome: null },
    ]
  });
});

const runId = "PRIVATE-RUN-ID";
const tool = { run_id: runId, call_id: "PRIVATE-CALL-ID", tool_name: "fs_write", input: { path: PRIVATE.input } };
const started: DaemonEvent = { type: "run_started", run_id: runId, chat_id: "PRIVATE-CHAT-ID", bot_id: "PRIVATE-BOT-ID" };
const requested: DaemonEvent = { type: "tool_call_requested", ...tool };
const waiting: DaemonEvent = { type: "approval_needed", ...tool, summary: "PRIVATE-APPROVAL-SUMMARY" };
const approved: DaemonEvent = { type: "tool_call_approved", run_id: runId, call_id: tool.call_id, approved_by: "PRIVATE-APPROVER" };

function exportRecordedEvents(events: DaemonEvent[]) {
  const state = events.reduce(
    (state, event, i) => applyEvent(state, { v: 1, seq: i + 1, gseq: i + 1, ts_ms: 1000 + i * 100, event }),
    EMPTY_EVENTS
  );
  const json = renderRunReportJson(state.runs[runId]);
  assert.ok(!json.includes("PRIVATE-"));
  return JSON.parse(json).approval_events;
}

test("JSON separates observed waiting and decisions, without reconstructing approval after completion", () => {
  const pending = { step_id: "step-1", kind: "waiting_for_approval", offset_ms: null, outcome: null };
  const decision = { step_id: "step-1", kind: "approval_recorded", offset_ms: null, outcome: "approved" };
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, approved]), [pending, decision]);
  assert.deepEqual(exportRecordedEvents([started, requested, approved]), [decision]);
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, approved, {
    type: "tool_call_completed", run_id: runId, call_id: tool.call_id, error: "PRIVATE-ERROR",
    output_preview: PRIVATE.output,
  }]), [pending, { step_id: "step-1", kind: "tool_outcome", offset_ms: 400, outcome: "error" }]);
});

test("JSON distinguishes a recorded denial from expiry or an ungated outcome", () => {
  const pending = { step_id: "step-1", kind: "waiting_for_approval", offset_ms: null, outcome: null };
  const denied: DaemonEvent = {
    type: "tool_call_denied", run_id: runId, call_id: tool.call_id, denied_by: "PRIVATE-DENIER", reason: "PRIVATE-REASON"
  };
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, denied]), [
    pending,
    { step_id: "step-1", kind: "approval_recorded", offset_ms: 300, outcome: "denied" },
  ]);
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, { ...denied, reason: "expired" }]), [pending]);
  assert.deepEqual(exportRecordedEvents([started, requested, {
    type: "tool_call_completed", run_id: runId, call_id: tool.call_id, error: null
  }]), []);
});

test("stopped and unfinished runs still produce a report", () => {
  assert.equal(buildRunReport(run({ stopped: true, error: null }), "H", t, String).outcome, "stopped");
  assert.equal(buildRunReport(run({ finishedMs: undefined, error: undefined }), "H", t, String).outcome, "unfinished");
});

test("html is self-contained and escapes bot-controlled text", () => {
  const report = buildRunReport(run({ items: [] }), "<script>alert(1)</script>", t, String);
  const html = renderHtml(report, text);
  assert.ok(!html.includes("<script>"));
  assert.ok(!/(src|href)=/.test(html));
  assert.ok(html.includes("&lt;script&gt;"));
});

test("scrubText hides labelled values and clips", () => {
  assert.equal(scrubText("token=abc123 ok"), "token=[hidden] ok");
  assert.ok(scrubText("x".repeat(500)).length <= 200);
});

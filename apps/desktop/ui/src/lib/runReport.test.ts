import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { DaemonEvent } from "../api/types.ts";
import { buildRunReport, renderHtml, renderMarkdown, scrubText, type ReportText } from "./runReport.ts";
import { renderRunReportJson } from "./runReportJson.ts";
import { applyEvent, EMPTY_EVENTS, type RunState, type ToolItem } from "../state/timeline.ts";

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

test("JSON schema v4 exposes only the documented run, step and approval-event fields", () => {
  const json = renderRunReportJson(run());
  assert.deepEqual(JSON.parse(json), {
    schema_version: 4,
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
    schema_version: 4,
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
const completed: DaemonEvent = {
  type: "tool_call_completed", run_id: runId, call_id: tool.call_id, error: null,
  output_preview: PRIVATE.output
};
const denied: DaemonEvent = {
  type: "tool_call_denied", run_id: runId, call_id: tool.call_id, denied_by: "PRIVATE-DENIER", reason: "PRIVATE-REASON"
};
const expiryReason = "expired: nobody answered in time";
const policyDenyReason = "denied by policy";
const pending = { step_id: "step-1", kind: "waiting_for_approval", offset_ms: null, outcome: null };
// An approval recorded before the daemon said what granted it, and any denial: nothing is claimed.
const unknownProvenance = { granted_by: null, level: null, rule: null };

function exportRecordedEvents(events: DaemonEvent[], sequences?: number[]) {
  const state = events.reduce(
    (state, event, i) => {
      const seq = sequences?.[i] ?? i + 1;
      return applyEvent(state, { v: 1, seq, gseq: seq, ts_ms: 1000 + i * 100, event });
    },
    EMPTY_EVENTS
  );
  const json = renderRunReportJson(state.runs[runId]);
  assert.ok(!json.includes("PRIVATE-"));
  for (const canary of Object.values(PRIVATE)) assert.ok(!json.includes(canary));
  return JSON.parse(json).approval_events;
}

test("JSON retains the recorded approval time after the same tool completes", () => {
  const decision = { step_id: "step-1", kind: "approval_recorded", offset_ms: 300, outcome: "approved", ...unknownProvenance };
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, approved]), [pending, decision]);
  assert.deepEqual(exportRecordedEvents([started, requested, approved]), [{ ...decision, offset_ms: 200 }]);
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, approved, completed]), [
    pending, decision, { step_id: "step-1", kind: "tool_outcome", offset_ms: 400, outcome: "done" }
  ]);
  assert.deepEqual(
    exportRecordedEvents([started, requested, waiting, approved, approved, { ...completed, error: "PRIVATE-ERROR" }], [1, 2, 3, 4, 4, 5]),
    [pending, decision, { step_id: "step-1", kind: "tool_outcome", offset_ms: 500, outcome: "error" }]
  );
});

test("JSON does not apply an earlier decision to a reopened approval gate", () => {
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, approved, waiting]), [pending]);
});

const granted = (extra: Record<string, unknown>): DaemonEvent =>
  ({ ...approved, ...extra } as DaemonEvent);

test("JSON v4 names what granted an approved call, from the daemon's fields only", () => {
  const recorded = (extra: Record<string, unknown>, offset = 300) => ({
    step_id: "step-1", kind: "approval_recorded", offset_ms: offset, outcome: "approved", ...extra
  });
  const automatic = (extra: Record<string, unknown>, offset = 200) => ({
    step_id: "step-1", kind: "automatic_grant", offset_ms: offset, outcome: null, ...extra
  });
  // An owner's card answer is the only thing that stays `approval_recorded`.
  assert.deepEqual(
    exportRecordedEvents([started, requested, waiting, granted({ granted_by: "owner_card", level: "ask" })]),
    [pending, recorded({ granted_by: "owner_card", level: "ask", rule: null })]
  );
  // An automatic grant has no card: no waiting event and no `approval_recorded` (a version 3
  // reader counts that as an owner's approval), but the grant is recorded and the outcome follows.
  assert.deepEqual(
    exportRecordedEvents([started, requested, granted({ granted_by: "full_access", level: "full" }), completed]),
    [
      automatic({ granted_by: "full_access", level: "full", rule: null }),
      { step_id: "step-1", kind: "tool_outcome", offset_ms: 300, outcome: "done" }
    ]
  );
  assert.deepEqual(
    exportRecordedEvents([started, requested, granted({ granted_by: "remembered_choice", level: "balanced", rule_id: "always:shell_run" })]),
    [automatic({ granted_by: "remembered_choice", level: "balanced", rule: "always:shell_run" })]
  );
  for (const by of ["owner_schedule", "owner_mission", "balanced_level", "standing_grant", "site_rule"]) {
    assert.deepEqual(
      exportRecordedEvents([started, requested, granted({ granted_by: by, level: "full" })]),
      [automatic({ granted_by: by, level: "full", rule: null })]
    );
  }
  // No automatic grant ever shows up as a recorded approval.
  for (const by of ["full_access", "owner_schedule", "owner_mission", "balanced_level", "standing_grant", "remembered_choice", "site_rule"]) {
    const events: Array<{ kind: string }> = exportRecordedEvents([started, requested, granted({ granted_by: by })]);
    assert.deepEqual(events.map((e) => e.kind), ["automatic_grant"], by);
  }
});

test("JSON drops provenance it does not recognise or that does not look like a daemon id", () => {
  const exported = (extra: Record<string, unknown>) =>
    exportRecordedEvents([started, requested, granted(extra)]);
  // A value from a newer daemon is unknown, not guessed.
  assert.deepEqual(exported({ granted_by: "PRIVATE-NEW-KIND", level: "full" }), [
    { step_id: "step-1", kind: "approval_recorded", offset_ms: 200, outcome: "approved", ...unknownProvenance }
  ]);
  // A level or rule id outside the closed shapes is dropped, so call text can never ride along.
  for (const rule_id of [`always:${PRIVATE.input}`, "always:../../etc/passwd", "elsewhere:shell_run", `chat:${"x".repeat(65)}`, 7]) {
    assert.deepEqual(exported({ granted_by: "remembered_choice", level: "PRIVATE-LEVEL", rule_id }), [
      { step_id: "step-1", kind: "automatic_grant", offset_ms: 200, outcome: null, granted_by: "remembered_choice", level: null, rule: null }
    ]);
  }
});

test("JSON re-checks a rule against the rule-id shape at export, whatever the timeline holds", () => {
  const item = (rule: { scope: "always"; name?: string }) => ({
    kind: "tool", key: "k", callId: "c1", name: "shell_run", input: {}, status: "done", requestedMs: 1100,
    gated: false, approvalDecision: { outcome: "approved", offsetMs: 1200, grant: { by: "remembered_choice", rule } }
  }) as unknown as ToolItem;
  const ruleOf = (rule: { scope: "always"; name?: string }) => {
    const report = run({ items: [item(rule)] });
    return JSON.parse(renderRunReportJson(report)).approval_events[0].rule;
  };
  assert.equal(ruleOf({ scope: "always", name: "shell_run" }), "always:shell_run");
  assert.equal(ruleOf({ scope: "always" }), "always");
  for (const name of [`${PRIVATE.input}`, "x".repeat(65), "../a", ""]) assert.equal(ruleOf({ scope: "always", name }), null, name);
});

test("JSON records a denial only after an observed gate and leaves reserved reasons ambiguous", () => {
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, denied]), [
    pending,
    { step_id: "step-1", kind: "approval_recorded", offset_ms: 300, outcome: "denied", ...unknownProvenance },
  ]);
  assert.deepEqual(exportRecordedEvents([started, requested, denied]), []);
  assert.deepEqual(exportRecordedEvents([started, requested, completed]), []);
  assert.deepEqual(exportRecordedEvents([started, requested, waiting, completed]), [
    pending, { step_id: "step-1", kind: "tool_outcome", offset_ms: 300, outcome: "done" }
  ]);
  for (const reason of [expiryReason, policyDenyReason, "expired is the reason I typed"]) {
    // An owner's supplied reason can be identical to a reserved daemon reason.
    assert.deepEqual(exportRecordedEvents([started, requested, waiting, { ...denied, reason }]), [pending]);
  }
});

test("approval report fixtures pin the daemon's expiry and policy-veto reasons", () => {
  const read = (file: string) =>
    readFileSync(new URL(`../../../../../crates/agent-core/src/${file}`, import.meta.url), "utf8");
  assert.equal(/const EXPIRED_REASON: &str = "([^"]+)";/.exec(read("approval.rs"))?.[1], expiryReason);
  for (const file of ["approval.rs", "exec.rs"]) {
    const veto = /Event::ToolCallDenied\(ToolCallDenied \{[^}]*reason: Some\("([^"]+)"\.into\(\)\),\s*\}\)/.exec(read(file))?.[1];
    assert.equal(veto, policyDenyReason, `policy veto reason in ${file}`);
  }
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

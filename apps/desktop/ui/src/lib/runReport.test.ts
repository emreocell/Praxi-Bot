import assert from "node:assert/strict";
import { test } from "node:test";
import { buildRunReport, renderHtml, renderMarkdown, scrubText, type ReportText } from "./runReport.ts";
import type { RunState } from "../state/timeline.ts";

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

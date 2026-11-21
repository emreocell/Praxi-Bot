import assert from "node:assert/strict";
import { test } from "node:test";
import type { TFunction } from "i18next";
import type { ToolItem, ToolStatus } from "../state/timeline.ts";
import { toolSentence } from "./runSentence.ts";
import { describeTool } from "./toolLabels.ts";

// Echoes the key and its values, so the test reads the chosen wording without i18next.
const t = ((key: string, values?: Record<string, unknown>) => key + JSON.stringify(values ?? {})) as unknown as TFunction;

function tool(name: string, status: ToolStatus, input: unknown = {}): ToolItem {
  return { kind: "tool", key: "k", callId: "c", name, input, status, requestedMs: 0, gated: false };
}

const PHASES: [ToolStatus, string][] = [
  ["requested", "running"],
  ["awaiting_approval", "running"],
  ["running", "running"],
  ["error", "failed"],
  ["denied", "failed"],
  ["expired", "failed"],
  ["done", "done"]
];

for (const [status, phase] of PHASES) {
  test(`a ${status} step uses the ${phase} wording`, () => {
    assert.equal(toolSentence(t, tool("web_search", status)), `chat.step.web_search.${phase}{}`);
  });
}

test("the label values from describeTool are passed through unchanged", () => {
  // An MCP tool from a server without a known brand, so its label has values to pass through.
  const step = tool("acme.create_issue", "done", { title: "x" });
  const label = describeTool(step.name, step.input);
  assert.notDeepEqual(label.values, {});
  assert.equal(toolSentence(t, step), `${label.key}.done${JSON.stringify(label.values)}`);
});

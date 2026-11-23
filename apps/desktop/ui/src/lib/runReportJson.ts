import type { RunState, ToolItem, ToolStatus } from "../state/timeline.ts";
import { isDiscoveryTool } from "./toolLabels.ts";

type StepOutcome = ToolStatus | "answered" | "unanswered" | "stored" | "cancelled" | null;

interface ExportStep {
  id: string;
  kind: "tool" | "question" | "secret";
  offset_ms: number | null;
  duration_ms: number | null;
  outcome: StepOutcome;
  depends_on: string[] | null;
}

type ApprovalEvent = { step_id: string; offset_ms: number | null } & (
  | { kind: "waiting_for_approval"; outcome: null }
  | { kind: "approval_recorded"; outcome: "approved" | "denied" }
  | { kind: "tool_outcome"; outcome: "done" | "error" }
);

const toolStatuses = new Set<ToolStatus>([
  "requested", "awaiting_approval", "running", "denied", "done", "error", "expired"
]);

function milliseconds(value: number | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

function appendApprovalEvents(
  events: ApprovalEvent[],
  item: ToolItem,
  stepId: string,
  startedMs: number
): void {
  if (!item.gated) return;
  const completionOffset = item.completedMs === undefined ? null : milliseconds(item.completedMs - startedMs);

  // Only an approval notice or a pending-card snapshot writes the summary. Export its presence, not its text.
  if (item.status === "awaiting_approval" || item.summary !== undefined) {
    events.push({ step_id: stepId, kind: "waiting_for_approval", offset_ms: null, outcome: null });
  }
  if (item.status === "running") {
    events.push({ step_id: stepId, kind: "approval_recorded", offset_ms: null, outcome: "approved" });
  } else if (item.status === "denied") {
    events.push({ step_id: stepId, kind: "approval_recorded", offset_ms: completionOffset, outcome: "denied" });
  } else if (item.status === "done" || item.status === "error") {
    // Completion replaces the running state. It does not retain a separate approval decision.
    events.push({ step_id: stepId, kind: "tool_outcome", offset_ms: completionOffset, outcome: item.status });
  }
}

/** The JSON export is a machine-readable allowlist, not a serialized timeline or transcript. */
export function renderRunReportJson(run: RunState): string {
  const steps: ExportStep[] = [];
  const approvalEvents: ApprovalEvent[] = [];
  const items = run.items
    .filter(item => item.kind === "tool" || item.kind === "question" || item.kind === "secret")
    .sort((a, b) => a.requestedMs - b.requestedMs);
  for (const item of items) {
    if (item.kind === "tool" && isDiscoveryTool(item.name)) continue;
    const stepId = `step-${steps.length + 1}`;
    const outcome = item.kind === "tool"
      ? (toolStatuses.has(item.status) ? item.status : null)
      : item.kind === "question"
        ? (item.answer === undefined ? "unanswered" : "answered")
        : (item.outcome === "stored" || item.outcome === "cancelled" ? item.outcome : "unanswered");
    steps.push({
      id: stepId,
      kind: item.kind,
      offset_ms: milliseconds(item.requestedMs - run.startedMs),
      duration_ms: item.kind === "tool" ? milliseconds(item.durationMs) : null,
      outcome,
      // The folded timeline does not retain causal edges. Adjacency is not a dependency.
      depends_on: null
    });
    if (item.kind === "tool") appendApprovalEvents(approvalEvents, item, stepId, run.startedMs);
  }
  const outcome = run.finishedMs === undefined ? "unfinished"
    : run.stopped ? "stopped"
      : run.error ? "failed" : "done";
  return JSON.stringify({
    schema_version: 2,
    outcome,
    duration_ms: run.finishedMs === undefined ? null : milliseconds(run.finishedMs - run.startedMs),
    steps,
    approval_events: approvalEvents
  }, null, 2) + "\n";
}

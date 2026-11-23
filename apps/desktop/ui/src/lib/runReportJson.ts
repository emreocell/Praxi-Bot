import type { RunState, ToolStatus } from "../state/timeline.ts";
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

const toolStatuses = new Set<ToolStatus>([
  "requested", "awaiting_approval", "running", "denied", "done", "error", "expired"
]);

function milliseconds(value: number | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

/** The JSON export is a machine-readable allowlist, not a serialized timeline or transcript. */
export function renderRunReportJson(run: RunState): string {
  const steps: ExportStep[] = [];
  const items = run.items
    .filter(item => item.kind === "tool" || item.kind === "question" || item.kind === "secret")
    .sort((a, b) => a.requestedMs - b.requestedMs);
  for (const item of items) {
    if (item.kind === "tool" && isDiscoveryTool(item.name)) continue;
    const outcome = item.kind === "tool"
      ? (toolStatuses.has(item.status) ? item.status : null)
      : item.kind === "question"
        ? (item.answer === undefined ? "unanswered" : "answered")
        : (item.outcome === "stored" || item.outcome === "cancelled" ? item.outcome : "unanswered");
    steps.push({
      id: `step-${steps.length + 1}`,
      kind: item.kind,
      offset_ms: milliseconds(item.requestedMs - run.startedMs),
      duration_ms: item.kind === "tool" ? milliseconds(item.durationMs) : null,
      outcome,
      // The folded timeline does not retain causal edges. Adjacency is not a dependency.
      depends_on: null
    });
  }
  const outcome = run.finishedMs === undefined ? "unfinished"
    : run.stopped ? "stopped"
      : run.error ? "failed" : "done";
  return JSON.stringify({
    schema_version: 1,
    outcome,
    duration_ms: run.finishedMs === undefined ? null : milliseconds(run.finishedMs - run.startedMs),
    steps
  }, null, 2) + "\n";
}

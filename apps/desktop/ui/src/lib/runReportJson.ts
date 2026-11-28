import { validRuleId, type GrantedBy, type GrantLevel, type RunState, type ToolItem, type ToolStatus } from "../state/timeline.ts";
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
  | ({ kind: "approval_recorded"; outcome: "approved" | "denied" } & Provenance)
  | ({ kind: "automatic_grant"; outcome: null } & Provenance)
  | { kind: "tool_outcome"; outcome: "done" | "error" }
);

/**
 * What let an approved call run, from the daemon's own `granted_by` / `level` / `rule_id`. Each
 * field is `null` when the daemon did not say (an approval recorded before version 4, or a
 * denial). `rule` is the scope and tool or category name of a remembered rule, never call text;
 * it is checked against the rule-id shape again here, whatever the timeline holds.
 */
interface Provenance {
  granted_by: GrantedBy | null;
  level: GrantLevel | null;
  rule: string | null;
}

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
  const decision = item.approvalDecision;
  const grant = decision?.grant;
  // An automatic grant (Full, Balanced, a standing or remembered rule) is no owner approval and
  // is not exported as one: version 3 readers count `approval_recorded` as an answered card.
  const automatic = decision?.outcome === "approved" && grant !== undefined && grant.by !== "owner_card";
  if (!item.gated && !automatic) return;
  const completionOffset = item.completedMs === undefined ? null : milliseconds(item.completedMs - startedMs);

  // Only an approval notice or a pending-card snapshot writes the summary. Export its presence, not its text.
  if (item.status === "awaiting_approval" || item.summary !== undefined) {
    events.push({ step_id: stepId, kind: "waiting_for_approval", offset_ms: null, outcome: null });
  }
  if (decision?.outcome === "approved" || decision?.outcome === "denied") {
    const provenance: Provenance = {
      granted_by: grant?.by ?? null,
      level: grant?.level ?? null,
      rule: validRuleId(grant?.rule) ?? null
    };
    const offset_ms = milliseconds(decision.offsetMs);
    events.push(
      automatic
        ? { step_id: stepId, kind: "automatic_grant", offset_ms, outcome: null, ...provenance }
        : { step_id: stepId, kind: "approval_recorded", offset_ms, outcome: decision.outcome, ...provenance }
    );
  }
  if (item.status === "done" || item.status === "error") {
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
    schema_version: 4,
    outcome,
    duration_ms: run.finishedMs === undefined ? null : milliseconds(run.finishedMs - run.startedMs),
    steps,
    approval_events: approvalEvents
  }, null, 2) + "\n";
}

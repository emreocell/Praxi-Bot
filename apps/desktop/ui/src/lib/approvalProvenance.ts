/**
 * The step line for a recorded approval decision (pure, no React).
 *
 * What the line says comes only from the daemon's `granted_by`, `level` and `rule_id`
 * (`grantOf` checks them). An approval recorded before the daemon said who granted it stays a
 * plain "Approved", never "You approved".
 */
import type { ApprovalGrant, ToolItem } from "../state/timeline.ts";
import { humanizeName, resolveToolCall } from "./toolLabels.ts";

export interface DecisionLine {
  key: string;
  values: Record<string, string>;
}

/** Permission categories the bot form names (`botForm.category.*`). */
const CATEGORIES = new Set([
  "accounts", "browser", "desktop", "fs", "messaging", "money", "remote", "script", "shell", "system"
]);

/** The tool as a lower-case phrase: `shell_run` -> `shell run`, `mcp:github/x` -> `github x`. */
function toolPhrase(tool: Pick<ToolItem, "name" | "input">): string {
  return humanizeName(resolveToolCall(tool.name, tool.input).name.replace(/^mcp:/, ""));
}

function rememberedLine(grant: ApprovalGrant, tool: Pick<ToolItem, "name" | "input">, t: (key: string) => string): DecisionLine {
  const rule = grant.rule;
  if (rule?.scope === "category" && rule.name && CATEGORIES.has(rule.name)) {
    return { key: "chat.chain.grant.rememberedCategory", values: { what: t(`botForm.category.${rule.name}`) } };
  }
  const what = toolPhrase(tool);
  if (rule?.scope === "always") return { key: "chat.chain.grant.rememberedAlways", values: { what } };
  if (rule?.scope === "chat") return { key: "chat.chain.grant.rememberedChat", values: { what } };
  if (rule?.scope === "task") return { key: "chat.chain.grant.rememberedTask", values: { what } };
  return { key: "chat.chain.grant.remembered", values: {} };
}

/**
 * The line for `tool`'s decision, or null when there is none. `time` is the decision's time into
 * the run, already formatted (null when it was not recorded).
 */
export function decisionLine(
  tool: Pick<ToolItem, "name" | "input" | "approvalDecision">,
  time: string | null,
  t: (key: string) => string
): DecisionLine | null {
  const decision = tool.approvalDecision;
  if (!decision) return null;
  if (decision.outcome === "denied") {
    return time === null ? null : { key: "chat.chain.deniedAt", values: { time } };
  }
  const grant = decision.grant;
  switch (grant?.by) {
    case undefined:
      return time === null ? null : { key: "chat.chain.approvedAt", values: { time } };
    case "owner_card":
      return time === null
        ? { key: "chat.chain.grant.ownerCardNoTime", values: {} }
        : { key: "chat.chain.grant.ownerCard", values: { time } };
    case "full_access":
      return { key: "chat.chain.grant.fullAccess", values: {} };
    case "owner_schedule":
      return { key: "chat.chain.grant.ownerSchedule", values: {} };
    case "owner_mission":
      return { key: "chat.chain.grant.ownerMission", values: {} };
    case "balanced_level":
      return { key: "chat.chain.grant.balancedLevel", values: {} };
    case "standing_grant":
      return { key: "chat.chain.grant.standingGrant", values: {} };
    case "site_rule":
      return { key: "chat.chain.grant.siteRule", values: {} };
    case "remembered_choice":
      return rememberedLine(grant, tool, t);
  }
}

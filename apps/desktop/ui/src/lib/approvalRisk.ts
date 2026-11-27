import { botDetails, egressDetails, mailSendDetails, scheduleDetails } from "./approvalDetails.ts";
import { approvalRiskKind } from "./approvalPlain.ts";
import { resolveToolCall } from "./toolLabels.ts";

/**
 * How much an approval can change, in five plain tiers. It only decides how the approval card LOOKS
 * (calm for something that only looks, warning for something that changes or sends, danger for
 * something that deletes). It never decides whether a call needs an approval, what a key answers or
 * what a standing choice covers: those stay with the daemon and `approvalKeys` / `enterMayAllow`.
 */
export type ApprovalRisk = "read" | "change" | "run" | "send" | "delete";

/** Tools that only look (open a page, read, list, search, take a screenshot). */
const READ_ONLY = new Set([
  "browser_open",
  "pc_browser_open",
  "browser_navigate",
  "browser_tab_new",
  "browser_snapshot",
  "browser_screenshot",
  "browser_view",
  "fs_read",
  "fs_list",
  "fs_stat",
  "fs_search",
  "web_search",
  "web_extract",
  "web_feed",
  "web_metadata",
  "web_sitemap",
  "pdf_extract",
  "docx_read",
  "xlsx_read",
  "pptx_read",
  "csv_read",
  "json_query",
  "git_status",
  "git_diff",
  "git_log",
  "git_show",
  "email_read",
  "email_search",
  "email_list_folders",
  "desktop_get_state",
  "desktop_list_apps",
  "skill_read",
  "skill_list",
  "notes_read",
  "memory_search",
  "session_search",
  "knowledge_search",
  "schedule_list",
  "bot_list"
]);

/** Tools that change something on the computer, in a page, in a file or in the bot's own setup. */
const CHANGES = new Set([
  "fs_write",
  "fs_edit",
  "fs_move",
  "docx_write",
  "docx_append",
  "xlsx_write",
  "xlsx_append_rows",
  "xlsx_update_cells",
  "git_branch",
  "git_commit",
  "browser_act",
  "browser_click",
  "browser_fill",
  "browser_press",
  "browser_select",
  "browser_eval",
  "desktop_click",
  "desktop_drag",
  "desktop_type",
  "desktop_key",
  "desktop_set_value",
  "desktop_launch",
  "skill_write",
  "notes_write",
  "memory_save",
  "memory_forget",
  "schedule_create",
  "schedule_update",
  "schedule_pause",
  "schedule_resume",
  "bot_create",
  "bot_update",
  "email_draft",
  "email_mark_read"
]);

/** `null` when the call is not one the tiers know: the card then stays neutral and says nothing. */
export function approvalRisk(tool: string, input: unknown, details?: unknown): ApprovalRisk | null {
  const kind = approvalRiskKind(tool, input, details);
  if (kind === "delete" || kind === "run" || kind === "send") return kind;
  // A schedule card is about setting up a task, not about sending: when the taint gate also rides
  // on it (the egress review stays in Details), the chip must still say what the owner is allowing.
  const schedule = scheduleDetails(details);
  if (schedule) return schedule.action === "delete" ? "delete" : "change";
  if (mailSendDetails(details) || egressDetails(details)) return "send";
  const bot = botDetails(details);
  if (bot) return bot.action === "delete" ? "delete" : "change";
  const kindOfDetails = (details as { kind?: unknown } | null | undefined)?.kind;
  if (kindOfDetails === "control_grant" || kindOfDetails === "app_action") return "change";
  const name = resolveToolCall(tool, input).name;
  if (READ_ONLY.has(name)) return "read";
  if (CHANGES.has(name)) return "change";
  return null;
}

/** Names that mean money or a credential: such a card is never answered with a key, whatever the tier says. */
const SENSITIVE_NAME = /pay|purchase|checkout|billing|secret|credential|password|passwd|token|api_?key|vault/i;

/**
 * Whether the Enter key may allow this call once. Only the look-only tier qualifies. A call that
 * changes something, starts a program or a shell, deletes, sends, mails, moves data out, pays,
 * touches a secret, or that the tiers do not know (`null`) needs a click on the Allow button.
 * Escape (deny) is never limited by this.
 */
export function enterMayAllowCall(tool: string, input: unknown, details?: unknown): boolean {
  if (SENSITIVE_NAME.test(tool) || SENSITIVE_NAME.test(resolveToolCall(tool, input).name)) return false;
  return approvalRisk(tool, input, details) === "read";
}

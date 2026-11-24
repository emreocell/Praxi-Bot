/** Plain-language helpers of the audit log viewer (Settings > Safety). */

export const AUDIT_KINDS = [
  "approval",
  "permission",
  "takeover",
  "import",
  "update",
  "key_use",
  "other"
] as const;

/** Actions that have their own plain-language name (`audit.event.<key>`). */
export const AUDIT_KNOWN_ACTIONS = [
  "bot.create",
  "bot.delete",
  "bot.update",
  "checkpoint.restored",
  "components.install_started",
  "composio.call",
  "composio.key_removed",
  "composio.key_set",
  "doctor.repair",
  "engine.killed",
  "engine.updated",
  "fill_grant.revoke",
  "fill_secret.delete",
  "grant.always",
  "host.invite",
  "host_link.connect",
  "host_link.disconnect",
  "host_update.start",
  "input.batch_authorized",
  "knowledge.embedding_approved",
  "llm.call_started",
  "mcp.token_created",
  "mcp.token_revoked",
  "mcp.tool_called",
  "peer.decision_completed",
  "peer.decision_requested",
  "profile.export",
  "profile.import",
  "provider.cli_enable",
  "question.answered",
  "skill.update_requested",
  "takeover.active",
  "trusted_folder.add",
  "trusted_folder.remove",
  "usage"
] as const;

/** i18n key of an action's plain name; unknown actions get the generic name. */
export function auditEventKey(action: string): string {
  return (AUDIT_KNOWN_ACTIONS as readonly string[]).includes(action)
    ? `audit.event.${action.replace(/[.]/g, "_")}`
    : "audit.event.unknown";
}

/** i18n key of an actor's plain name, or null to show the raw name (a device, a bot id). */
export function auditActorKey(actor: string): string | null {
  return actor === "user" || actor === "bot" || actor === "system" ? `audit.actor.${actor}` : null;
}

export type AuditFilters = {
  windowMs: number | null;
  actor: string;
  kind: string;
  bot: string;
  q: string;
};

/** Query of the list and export routes; empty filters are left out. */
export function auditQuery(
  f: AuditFilters,
  now: number,
  page?: { offset: number; limit: number }
): Record<string, string | number> {
  const q: Record<string, string | number> = {};
  if (f.windowMs) q.since_ms = now - f.windowMs;
  if (f.actor) q.actor = f.actor;
  if (f.kind) q.kind = f.kind;
  if (f.bot) q.bot = f.bot;
  if (f.q.trim()) q.q = f.q.trim();
  if (page) {
    q.offset = page.offset;
    q.limit = page.limit;
  }
  return q;
}

import type { OpenApiApp, OpenApiAuthInput, OpenApiAuthKind, OpenApiEnabledOperation, OpenApiOperation } from "../api/types.ts";

/** Whose access the operation list is showing: every bot, or one bot. */
export type AccessTarget = { kind: "all" } | { kind: "bot"; id: string };

/** The set of enabled operations as the daemon wants it back (it replaces the whole set). */
export function enabledSet(ops: OpenApiOperation[]): OpenApiEnabledOperation[] {
  return ops.filter((o) => o.enabled).map((o) => ({ name: o.name, bots: o.bots ?? null }));
}

/** Is the operation on for this target? An operation open to every bot is on for each bot. */
export function isOn(op: OpenApiOperation, target: AccessTarget): boolean {
  if (!op.enabled) return false;
  if (target.kind === "all") return op.bots === null || op.bots === undefined;
  return op.bots === null || op.bots === undefined || op.bots.includes(target.id);
}

/**
 * Tick or untick one operation for the target and return the new enabled set.
 *
 * - "All bots" on: open to every bot; off: switched off for everyone.
 * - One bot on: added to the operation's list (an operation already open to every bot stays so);
 *   off: removed from the list, and an operation open to every bot becomes the list of all the
 *   other bots. An operation nobody may use any more is switched off, never widened to everyone.
 */
export function toggleOperation(
  ops: OpenApiOperation[],
  name: string,
  target: AccessTarget,
  on: boolean,
  allBotIds: string[],
): OpenApiEnabledOperation[] {
  const rest = enabledSet(ops).filter((e) => e.name !== name);
  const op = ops.find((o) => o.name === name);
  if (!op) return rest;
  const open = op.enabled && (op.bots === null || op.bots === undefined);
  const list = op.enabled && op.bots ? op.bots : [];
  let bots: string[] | null | "off";
  if (target.kind === "all") {
    bots = on ? null : "off";
  } else if (on) {
    bots = open ? null : list.includes(target.id) ? list : [...list, target.id];
  } else {
    const remaining = (open ? allBotIds : list).filter((id) => id !== target.id);
    bots = remaining.length === 0 ? "off" : remaining;
  }
  if (bots === "off") return rest;
  // Keep the document order of the operations.
  return ops
    .filter((o) => o.name === name || o.enabled)
    .map((o) => (o.name === name ? { name, bots } : { name: o.name, bots: o.bots ?? null }));
}

/** Operations matching a search over name, path, summary and tags. */
export function filterOperations(ops: OpenApiOperation[], query: string): OpenApiOperation[] {
  const q = query.trim().toLowerCase();
  if (!q) return ops;
  return ops.filter((o) => [o.name, o.method, o.path, o.summary, ...o.tags].some((s) => s.toLowerCase().includes(q)));
}

export interface AuthForm {
  kind: OpenApiAuthKind;
  name: string;
  username: string;
  secret: string;
}

export type AuthResult = { ok: true; auth: OpenApiAuthInput } | { ok: false; error: "name" | "username" | "secret" };

/**
 * The auth part of a request from the form. `keepSecret` (an app that already has one) lets the
 * secret stay empty: the daemon then keeps the stored value.
 */
export function authFromForm(form: AuthForm, keepSecret = false): AuthResult {
  const name = form.name.trim();
  const username = form.username.trim();
  if ((form.kind === "api_key_header" || form.kind === "api_key_query") && !name) return { ok: false, error: "name" };
  if (form.kind === "basic" && !username) return { ok: false, error: "username" };
  if (form.kind !== "none" && !form.secret && !keepSecret) return { ok: false, error: "secret" };
  const auth: OpenApiAuthInput = { kind: form.kind };
  if (form.kind === "api_key_header" || form.kind === "api_key_query") auth.name = name;
  if (form.kind === "basic") auth.username = username;
  if (form.kind !== "none" && form.secret) auth.secret = form.secret;
  return { ok: true, auth };
}

/** A spec URL the daemon can fetch: http(s) with a host. */
export function validSpecUrl(raw: string): boolean {
  try {
    const u = new URL(raw.trim());
    return (u.protocol === "https:" || u.protocol === "http:") && u.hostname !== "";
  } catch {
    return false;
  }
}

/** One line for the app card: how many operations are on. */
export function enabledSummary(app: Pick<OpenApiApp, "enabled_count" | "operations">): { enabled: number; total: number } {
  return { enabled: app.enabled_count, total: app.operations.length };
}

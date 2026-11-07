import type { RouteInfo } from "../api/types";

/** What `route_decided` told us about one turn. */
export interface RouteState {
  /** `K0`..`K4` (kept as the daemon sent it). */
  level: string;
  route: RouteInfo;
  /** `0..=1`. */
  confidence: number;
  latencyMs: number;
  reason: string;
}

function strings(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/** Read the untrusted `route` JSON of a `route_decided` event. */
export function parseRoute(raw: unknown): RouteInfo {
  if (typeof raw !== "object" || raw === null) {
    return { kind: "unknown" };
  }
  const r = raw as Record<string, unknown>;
  switch (r.kind) {
    case "command":
      return typeof r.name === "string"
        ? { kind: "command", name: r.name.replace(/^\//, "") }
        : { kind: "unknown" };
    case "workflow":
      return typeof r.name === "string"
        ? {
            kind: "workflow",
            name: r.name,
            params: r.params,
            confidence:
              typeof r.confidence === "number" ? r.confidence : undefined
          }
        : { kind: "unknown" };
    case "tool_hint":
      return { kind: "tool_hint", tools: strings(r.tools) };
    case "skill_hint":
      return { kind: "skill_hint", skills: strings(r.skills) };
    case "agent":
      return { kind: "agent" };
    default:
      return { kind: "unknown" };
  }
}

export interface RouteChipModel {
  /** i18n key (`chat.route.*`) and its values. */
  i18nKey: string;
  values: Record<string, string>;
  /** A deterministic route that skipped the model (shown with the bolt). */
  cheap: boolean;
}

/**
 * The chip for a turn, or `null` for none.  A plain `Agent` decision at K4
 * ("nothing cheaper applied") is the default and stays silent unless
 * `showAgent` (the debug setting) is on.
 */
export function routeChip(
  r: RouteState,
  showAgent: boolean
): RouteChipModel | null {
  const route = r.route;
  switch (route.kind) {
    case "workflow":
      return {
        i18nKey: "chat.route.workflow",
        values: { name: route.name },
        cheap: true
      };
    case "command":
      return {
        i18nKey: "chat.route.command",
        values: { name: route.name },
        cheap: true
      };
    case "tool_hint":
      return {
        i18nKey: "chat.route.toolHint",
        values: { tools: route.tools.join(", ") },
        cheap: false
      };
    case "skill_hint":
      return {
        i18nKey: "chat.route.skillHint",
        values: { skills: route.skills.join(", ") },
        cheap: false
      };
    case "agent":
      if (r.level === "K4" && !showAgent) {
        return null;
      }
      return {
        i18nKey: "chat.route.agent",
        values: { level: r.level },
        cheap: false
      };
    case "unknown":
      return null;
  }
}

/** Values for the hover text (`chat.route.hover`). */
export function routeHoverValues(r: RouteState): Record<string, string> {
  const pct = Math.round(Math.min(1, Math.max(0, r.confidence)) * 100);
  return {
    level: r.level,
    confidence: String(pct),
    latency: String(Math.round(r.latencyMs)),
    reason: r.reason
  };
}

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

/**
 * The id of the chat a `/new` turn created.  The daemon only reports it in the
 * reply text ("Started a new chat: <uuid>"); a structured field would be
 * better (listed as missing in the task report).
 */
export function newChatIdFromText(text: string): string | null {
  const m = /Started a new chat:\s*(\S+)/.exec(text);
  const id = m ? UUID.exec(m[1]) : null;
  return id ? id[0].toLowerCase() : null;
}

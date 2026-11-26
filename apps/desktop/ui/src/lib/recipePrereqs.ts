import type { ComposioApp, EmailAccount, McpServer } from "../api/types.ts";
import { accountsForBot } from "./botMail.ts";
import type { RecipeNeed } from "./recipes.ts";

/**
 * What the app knows right now.  `null` = could not be read: such a need shows as "could not
 * check" and never as met (nothing is marked done on a guess), but it does not block either.
 */
export interface PrereqFacts {
  hasModel: boolean | null;
  hasBrowser: boolean | null;
  mailboxes: readonly EmailAccount[] | null;
  servers: readonly Pick<McpServer, "name" | "status" | "enabled">[] | null;
  composioApps: readonly Pick<ComposioApp, "slug" | "name" | "state" | "enabled">[] | null;
}

/** Where the one-click fix leads: always an existing screen, never a new flow. */
export type PrereqFix =
  | { kind: "settings"; section: "ai" | "computer" | "mail" }
  | { kind: "apps" };

export type PrereqState = "ok" | "missing" | "unknown";

export interface PrereqResult {
  need: RecipeNeed;
  state: PrereqState;
  optional: boolean;
  /** Present when `state` is not "ok". */
  fix?: PrereqFix;
}

/** An id no bot has: the new bot does not exist yet, so only mailboxes any bot may use count. */
const NOT_YET_CREATED = "00000000-0000-0000-0000-000000000000";

const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, "");

/** Does an app name or slug match one of the recipe's wanted names (either contains the other)? */
function matchesApp(name: string, wanted: readonly string[]): boolean {
  const n = norm(name);
  return n !== "" && wanted.some((w) => n.includes(norm(w)) || norm(w).includes(n));
}

function stateOf(need: RecipeNeed, facts: PrereqFacts): PrereqState {
  const yes = (v: boolean | null): PrereqState => (v === null ? "unknown" : v ? "ok" : "missing");
  switch (need.kind) {
    case "model":
      return yes(facts.hasModel);
    case "browser":
      return yes(facts.hasBrowser);
    case "email":
      return yes(facts.mailboxes === null ? null : accountsForBot(facts.mailboxes, NOT_YET_CREATED).length > 0);
    case "app": {
      if (facts.servers === null && facts.composioApps === null) return "unknown";
      const server = (facts.servers ?? []).some((s) => s.enabled && s.status === "connected" && matchesApp(s.name, need.apps));
      const composio = (facts.composioApps ?? []).some(
        (a) => a.enabled && a.state === "active" && (matchesApp(a.slug, need.apps) || matchesApp(a.name, need.apps))
      );
      if (server || composio) return "ok";
      // One list failing leaves the answer open unless the other one already said yes.
      return facts.servers === null || facts.composioApps === null ? "unknown" : "missing";
    }
  }
}

function fixOf(need: RecipeNeed): PrereqFix {
  switch (need.kind) {
    case "model":
      return { kind: "settings", section: "ai" };
    case "browser":
      return { kind: "settings", section: "computer" };
    case "email":
      return { kind: "settings", section: "mail" };
    case "app":
      return { kind: "apps" };
  }
}

/** Each need of a recipe, checked against what is connected now (in the recipe's order). */
export function checkPrereqs(needs: readonly RecipeNeed[], facts: PrereqFacts): PrereqResult[] {
  return needs.map((need) => {
    const state = stateOf(need, facts);
    return { need, state, optional: need.optional === true, ...(state === "ok" ? {} : { fix: fixOf(need) }) };
  });
}

/** The needs that keep the recipe from starting: required and known to be missing. */
export function blockingPrereqs(results: readonly PrereqResult[]): PrereqResult[] {
  return results.filter((r) => r.state === "missing" && !r.optional);
}

export function canStart(results: readonly PrereqResult[]): boolean {
  return blockingPrereqs(results).length === 0;
}

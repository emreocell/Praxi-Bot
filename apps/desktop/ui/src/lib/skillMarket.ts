/**
 * Skill library (issue 206): the rules the dialog mirrors from the daemon.
 *
 * The daemon enforces every one of these (fingerprint, risk acceptance, "asks for more" on an
 * update); this file only decides what the dialog shows and which button is enabled, so a person
 * never has to guess why Install is greyed out. Pure logic, no React.
 */

export type MarketRisk = "low" | "medium" | "high";

/** What the scan saw, as the daemon reports it (`needs`). */
export interface MarketPreviewLike {
  risk: string;
  needs: string[];
  requires_risk_acceptance: boolean;
  requires_access_confirmation: boolean;
  fingerprint: string;
  added_needs: string[];
  removed_needs: string[];
  installed?: { version: string; risk: string; needs: string[] } | null;
}

/** Order the capabilities from most to least serious in the list. */
const NEEDS_ORDER = ["scripts", "credentials", "network", "active_files", "hidden", "steering", "large"];

export function riskOf(value: string): MarketRisk {
  return value === "high" || value === "medium" ? value : "low";
}

/** i18n key of one capability; unknown codes (a newer daemon) fall back to a generic line. */
export function needsKey(code: string): string {
  return NEEDS_ORDER.includes(code) ? `skillMarket.needs.${code}` : "skillMarket.needs.other";
}

export function sortedNeeds(needs: readonly string[]): string[] {
  const rank = (c: string) => {
    const i = NEEDS_ORDER.indexOf(c);
    return i < 0 ? NEEDS_ORDER.length : i;
  };
  return [...new Set(needs)].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
}

export interface ConfirmState {
  /** "I understand the risk" for anything above Low. */
  acceptRisk: boolean;
  /** "I accept that this update asks for more" when requests grew. */
  acceptMore: boolean;
}

export const NOTHING_CONFIRMED: ConfirmState = { acceptRisk: false, acceptMore: false };

/** The boxes the person still has to tick before Install works. */
export function missingConfirmations(p: MarketPreviewLike, s: ConfirmState): Array<"risk" | "access"> {
  const out: Array<"risk" | "access"> = [];
  if (p.requires_risk_acceptance && !s.acceptRisk) out.push("risk");
  if (p.requires_access_confirmation && !s.acceptMore) out.push("access");
  return out;
}

export function canConfirm(p: MarketPreviewLike, s: ConfirmState): boolean {
  return missingConfirmations(p, s).length === 0;
}

export interface InstallBody {
  index_url?: string;
  name: string;
  update: boolean;
  accept: string;
  accept_risk: boolean;
  accept_more_access: boolean;
}

/**
 * The install request. A flag is only ever sent when the dialog actually asked for it, so a
 * leftover tick from another skill can never accept something the person did not see.
 */
export function buildInstallBody(
  p: MarketPreviewLike,
  args: { name: string; indexUrl: string | null; update: boolean; confirm: ConfirmState }
): InstallBody {
  return {
    ...(args.indexUrl ? { index_url: args.indexUrl } : {}),
    name: args.name,
    update: args.update,
    accept: p.fingerprint,
    accept_risk: p.requires_risk_acceptance && args.confirm.acceptRisk,
    accept_more_access: p.requires_access_confirmation && args.confirm.acceptMore
  };
}

export type IndexUrlCheck = "empty" | "ok" | "not_https" | "bad_host";

/** A custom list address: https only, on a named host (the daemon checks again). */
export function checkIndexUrl(text: string): IndexUrlCheck {
  const v = text.trim();
  if (!v) return "empty";
  if (!/^https:\/\//i.test(v)) return "not_https";
  const host = v.slice(8).split(/[/?#]/)[0] ?? "";
  const numeric = /^[\d.:]+$/.test(host);
  if (!host || /[@[\]\s]/.test(host) || !host.includes(".") || numeric || host.toLowerCase() === "localhost") {
    return "bad_host";
  }
  return "ok";
}

export function filterByQuery<T extends { name: string; description: string; tags?: string[] }>(
  list: readonly T[],
  query: string
): T[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...list];
  return list.filter(
    (s) =>
      s.name.toLowerCase().includes(q) ||
      s.description.toLowerCase().includes(q) ||
      (s.tags ?? []).some((t) => t.toLowerCase().includes(q))
  );
}

/** Where an installed skill came from, for the i18n key `skillMarket.source.<kind>`. */
export function sourceKey(kind: string): string {
  return ["bundled", "index", "git", "local"].includes(kind) ? `skillMarket.source.${kind}` : "skillMarket.source.other";
}

export type Tone = "ok" | "warn" | "danger";

export function riskTone(risk: MarketRisk): Tone {
  return risk === "high" ? "danger" : risk === "medium" ? "warn" : "ok";
}

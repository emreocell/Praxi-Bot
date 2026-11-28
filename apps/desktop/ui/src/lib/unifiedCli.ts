import type { AdminCliEngine, CliEngineDto } from "../api/types.ts";
import { cliStatus, canRunEngine, type CliStatus } from "./cliCards.ts";

/** This computer: on / off / not installed (per product brief). */
export type LocalCliPlace =
  | "not_installed"
  | "on"
  | "off"
  | "unreachable";

/** My server: logged in / not logged in / not installed. */
export type ServerCliPlace =
  | "not_installed"
  | "logged_in"
  | "not_logged_in"
  | "unknown"
  | "loading"
  | "unreachable";

/** Where the server's list stands: not asked yet, answered, or the server did not answer. */
export type ServerListStatus = "loading" | "ready" | "unreachable";

export interface UnifiedCliRow {
  id: string;
  name: string;
  local: CliEngineDto | null;
  server: AdminCliEngine | null;
  localPlace: LocalCliPlace;
  localDetail: CliStatus | "na";
  serverPlace: ServerCliPlace;
}

export function localCliPlace(engine: CliEngineDto | null): { place: LocalCliPlace; detail: CliStatus | "na" } {
  if (!engine) {
    return { place: "not_installed", detail: "na" };
  }
  if (!engine.detected.installed) {
    return { place: "not_installed", detail: "not_installed" };
  }
  const status = cliStatus(engine);
  if (status !== "ready") {
    return { place: "off", detail: status };
  }
  return engine.enabled ? { place: "on", detail: "ready" } : { place: "off", detail: "ready" };
}

export function serverCliPlace(engine: AdminCliEngine | null): ServerCliPlace {
  if (!engine || !engine.installed) {
    return "not_installed";
  }
  if (engine.signed_in === true) {
    return "logged_in";
  }
  if (engine.signed_in === false) {
    return "not_logged_in";
  }
  return "unknown";
}

export interface MergeOptions {
  /** The server list is a failed or pending read: rows say so instead of "not installed". */
  serverStatus?: ServerListStatus;
  /** This computer's daemon could not be asked: server-only rows say so instead of "not installed". */
  localDown?: boolean;
}

/** Union of engine ids from the local detector and the host admin list. */
export function mergeCodingClis(
  local: readonly CliEngineDto[],
  server: readonly AdminCliEngine[] | null,
  { serverStatus = "ready", localDown = false }: MergeOptions = {}
): UnifiedCliRow[] {
  const serverKnown = serverStatus === "ready";
  const placeOf = (e: AdminCliEngine | null): ServerCliPlace => (serverKnown ? serverCliPlace(e) : serverStatus);
  const byId = new Map<string, UnifiedCliRow>();
  for (const e of local) {
    const id = e.detected.id;
    const { place, detail } = localCliPlace(e);
    byId.set(id, {
      id,
      name: e.detected.display_name,
      local: e,
      server: null,
      localPlace: place,
      localDetail: detail,
      serverPlace: placeOf(null)
    });
  }
  for (const e of serverKnown ? (server ?? []) : []) {
    const row = byId.get(e.id);
    const serverPlace = placeOf(e);
    if (row) {
      row.server = e;
      row.serverPlace = serverPlace;
      if (!row.name) {
        row.name = e.name;
      }
    } else {
      const { place, detail } = localCliPlace(null);
      byId.set(e.id, {
        id: e.id,
        name: e.name,
        local: null,
        server: e,
        localPlace: localDown ? "unreachable" : place,
        localDetail: localDown ? "na" : detail,
        serverPlace
      });
    }
  }
  const score = (r: UnifiedCliRow) =>
    (r.local?.detected.installed ? 2 : 0) + (r.server?.installed ? 1 : 0);
  return [...byId.values()].sort((a, b) => score(b) - score(a) || a.name.localeCompare(b.name));
}

/** Row can switch the local engine on only when it is ready to run. */
export function localCanToggle(row: UnifiedCliRow): boolean {
  return row.local !== null && canRunEngine(row.local) && cliStatus(row.local) === "ready";
}

const STATUS_I18N: Record<CliStatus, string> = {
  ready: "ready",
  sign_in: "signIn",
  outdated: "outdated",
  not_installed: "notInstalled",
  unsupported: "unsupported"
};

type Translate = (key: string, options?: Record<string, unknown>) => string;

/**
 * "This computer: on · Praxi host: signed in": one line per row, the places in the same order.
 * The reason a local CLI is off (needs sign-in, outdated) follows in brackets; "not installed"
 * already says it all.
 */
export function placeLabel(t: Translate, row: UnifiedCliRow, serverName: string, showServer: boolean): string {
  const reason =
    row.localPlace === "off" && row.localDetail !== "ready" && row.localDetail !== "na"
      ? ` (${t(`settingsModal.ai.detected.status.${STATUS_I18N[row.localDetail]}`)})`
      : "";
  const parts = [`${t("settingsUi.codingClis.thisComputer")} ${t(`settingsUi.codingClis.local.${row.localPlace}`)}${reason}`];
  if (showServer) {
    parts.push(`${t("settingsUi.codingClis.myServer", { name: serverName })} ${t(`settingsUi.codingClis.server.${row.serverPlace}`)}`);
  }
  return parts.join(" · ");
}

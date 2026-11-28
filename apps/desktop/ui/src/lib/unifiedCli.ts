import type { AdminCliEngine, CliEngineDto } from "../api/types.ts";
import { cliStatus, canRunEngine, type CliStatus } from "./cliCards.ts";

/** This computer: on / off / not installed (per product brief). */
export type LocalCliPlace =
  | "not_installed"
  | "on"
  | "off";

/** My server: logged in / not logged in / not installed. */
export type ServerCliPlace =
  | "not_installed"
  | "logged_in"
  | "not_logged_in"
  | "unknown";

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

/** Union of engine ids from the local detector and the host admin list. */
export function mergeCodingClis(
  local: readonly CliEngineDto[],
  server: readonly AdminCliEngine[] | null
): UnifiedCliRow[] {
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
      serverPlace: "not_installed"
    });
  }
  for (const e of server ?? []) {
    const row = byId.get(e.id);
    const serverPlace = serverCliPlace(e);
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
        localPlace: place,
        localDetail: detail,
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
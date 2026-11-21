import type { Connection } from "../api/connection";
import type { Health } from "../api/types";

/** What the app shows and offers about safe mode (tools, schedules, channels, apps off). */
export interface SafeModeView {
  /** The daemon says it runs in safe mode (the health route is the only source of truth). */
  active: boolean;
  /** Only a service this app started can be restarted by it, so only then can the owner leave. */
  canLeave: boolean;
  /** Same condition: the app can restart its own service in safe mode. */
  canEnter: boolean;
}

export function safeModeView(health: Health | null, conn: Pick<Connection, "started_by_app"> | null): SafeModeView {
  const ownService = Boolean(conn?.started_by_app);
  return {
    active: health?.safe_mode === true,
    canLeave: ownService,
    canEnter: ownService
  };
}

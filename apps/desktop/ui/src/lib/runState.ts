import type { ApiClient } from "../api/client.ts";

/** RunState strings returned by the daemon, distinct from tool-item "pending". */
export function isProcessingRunState(state: string): boolean {
  return state === "queued" || state === "running" || state === "waiting_approval";
}

/** A paused run can still be cancelled, but is not currently processing work. */
export function isCancellableRunState(state: string): boolean {
  return isProcessingRunState(state) || state === "paused";
}

/** Tray Stop All uses the current daemon snapshot, including queued and paused runs. */
export async function cancelAllRuns(client: Pick<ApiClient, "listRuns" | "cancelRun">): Promise<void> {
  const runs = await client.listRuns();
  await Promise.allSettled(
    runs.filter((run) => isCancellableRunState(run.state)).map((run) => client.cancelRun(run.id))
  );
}

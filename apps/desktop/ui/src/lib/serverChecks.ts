import { hostMismatch, type HostHealthLike, type HostProtocol } from "./hostState.ts";

/**
 * The live checks shown after a server was paired with its one-command code:
 * can this app reach it, does it run the same version, is there a screen to
 * watch, and can this app send it model keys. The checks talk to the server
 * through a small probe so tests can fake every step and every failure.
 */
export const PAIR_CHECKS = ["reach", "version", "screen", "models"] as const;
export type PairCheckId = (typeof PAIR_CHECKS)[number];
export type PairCheckState = "pending" | "running" | "pass" | "warn" | "fail";
export interface PairCheckRow {
  id: PairCheckId;
  state: PairCheckState;
}

export interface ServerProbe {
  /** Open the guarded channel; rejects when the server does not answer. */
  connect(): Promise<void>;
  /** Version and screen of the server as it reports them. */
  status(): Promise<{ build: HostHealthLike; hasDisplay: boolean }>;
  /** This app's own version. */
  localVersion(): Promise<HostProtocol>;
  /** True when this app may send model keys to the server. */
  canSendModels(): Promise<boolean>;
}

const initial = (): PairCheckRow[] => PAIR_CHECKS.map((id) => ({ id, state: "pending" }));

/**
 * Run the checks in order and report every change. A server that cannot be
 * reached ends the run (the other rows would only repeat the same failure);
 * the other rows are advice (`warn`), never a reason to throw the server away.
 */
export async function runPairChecks(
  probe: ServerProbe,
  onChange: (rows: PairCheckRow[]) => void
): Promise<PairCheckRow[]> {
  let rows = initial();
  const set = (id: PairCheckId, state: PairCheckState) => {
    rows = rows.map((r) => (r.id === id ? { ...r, state } : r));
    onChange(rows);
  };
  set("reach", "running");
  let status: Awaited<ReturnType<ServerProbe["status"]>>;
  try {
    await probe.connect();
    status = await probe.status();
  } catch {
    set("reach", "fail");
    return rows;
  }
  set("reach", "pass");
  set("version", "running");
  const local = await probe.localVersion().catch(() => null);
  set("version", local?.localVersion && !hostMismatch(status.build, local) ? "pass" : "warn");
  set("screen", status.hasDisplay ? "pass" : "warn");
  set("models", "running");
  let models = false;
  try {
    models = await probe.canSendModels();
  } catch {
    models = false;
  }
  set("models", models ? "pass" : "warn");
  return rows;
}

/** Everything the person needs is in place (warnings are advice only). */
export function checksDone(rows: PairCheckRow[]): boolean {
  return rows.every((r) => r.state === "pass" || r.state === "warn");
}

import type { ControlMode, ControlSnapshot } from "../overlay/events";

type Reporter = { putControlState(mode: ControlMode): Promise<unknown> };

/** Keep the physical PC registered while the main bridge follows a remote server. */
export function startLocalComputerPresence({
  active, local, read, subscribe, intervalMs
}: {
  active: Reporter | null;
  local: Reporter | null;
  read(): Promise<ControlSnapshot>;
  subscribe(report: (snapshot: ControlSnapshot) => void): () => void;
  intervalMs: number;
}): (() => void) | undefined {
  if (!local || local === active) return undefined;
  let live = true;
  let latest: ControlSnapshot | undefined;
  let pending = Promise.resolve();
  const send = (mode: ControlMode) => {
    pending = pending.then(async () => {
      if (live) await local.putControlState(mode);
    }).catch(() => {});
  };
  const report = (snapshot: ControlSnapshot) => {
    if (!live || (latest?.stateRev ?? 0) > (snapshot.stateRev ?? 0)) return;
    latest = snapshot;
    send(snapshot.mode);
  };
  const off = subscribe(report);
  void read().then((snapshot) => {
    if (!latest) report(snapshot);
  }).catch(() => {});
  const beat = setInterval(() => {
    if (latest) send(latest.mode);
  }, intervalMs);
  return () => {
    live = false;
    off();
    clearInterval(beat);
  };
}

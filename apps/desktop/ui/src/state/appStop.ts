import { useEffect, useState, useSyncExternalStore } from "react";
import { ackStop, getControlState, stopBotRun } from "../overlay/events.ts";
import {
  type LedgerStorage,
  RunLedger,
  STOP_STORAGE_KEY,
  type StopOrigin,
  type StopRecord
} from "./runLedger.ts";
import { describeStop, type StopStatus } from "./stopStatus.ts";
import { StopRuntime } from "./stopRuntime.ts";

/**
 * The main window's one Stop ledger (app lifetime, not per daemon client). Every
 * local Stop entry point calls `requestStop`; the widget and tray reach it through
 * native (`stopSeq`), never through their own logic.
 */

const SESSION_KEY = "praxi.webviewSession";

function webviewSession(): string {
  try {
    const existing = window.sessionStorage.getItem(SESSION_KEY);
    if (existing) {
      return existing;
    }
    const fresh = crypto.randomUUID();
    window.sessionStorage.setItem(SESSION_KEY, fresh);
    return fresh;
  } catch {
    return crypto.randomUUID();
  }
}

function localStore(): LedgerStorage | undefined {
  try {
    const store = window.localStorage;
    return {
      load: () => store.getItem(STOP_STORAGE_KEY),
      save: (value) => store.setItem(STOP_STORAGE_KEY, value)
    };
  } catch {
    return undefined;
  }
}

let instance: RunLedger | null = null;

export function appLedger(): RunLedger {
  if (!instance) {
    instance = new RunLedger({
      newId: () => crypto.randomUUID(),
      clock: {
        now: () => performance.now(),
        set: (fn, ms) => window.setTimeout(fn, ms),
        clear: (handle) => window.clearTimeout(handle as number)
      },
      native: {
        stopRun: (token) => stopBotRun(token),
        ackStop: (seq) => ackStop(seq)
      },
      session: webviewSession(),
      storage: localStore()
    });
  }
  return instance;
}

let runtime: StopRuntime | null = null;

/** The main window's host logic (ControlBridgeHost forwards to it). */
export function appRuntime(): StopRuntime {
  if (!runtime) {
    runtime = new StopRuntime({
      ledger: appLedger(),
      readNative: () => getControlState(),
      now: () => performance.now()
    });
  }
  return runtime;
}

/** One user Stop from a main-window entry point (hotkey, run banner, live viewer). */
export function requestStop(origin: Exclude<StopOrigin, "native" | "chat">): StopRecord {
  return appRuntime().requestStop(origin);
}

/** The chat's Stop / subagent cancel: one run, its own decision. */
export function cancelRun(runId: string, opts?: { force?: boolean }): StopRecord {
  return appRuntime().cancelOne(runId, opts);
}

/** Re-render on every ledger change. */
export function useLedgerVersion(): number {
  const ledger = appLedger();
  return useSyncExternalStore(
    (cb) => ledger.subscribe(cb),
    () => ledger.getVersion()
  );
}

/** The latest Stop as the user should see it (null when there is nothing to say). */
export function useStopStatus(): { status: StopStatus | null; dismiss: () => void } {
  useLedgerVersion();
  const ledger = appLedger();
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(new Set());
  const [, setTick] = useState(0);
  const status = describeStop({
    records: ledger.stopRecords(),
    runs: ledger.runEntries(),
    now: performance.now(),
    connected: ledger.client() !== null,
    dismissed
  });
  const visible = status !== null;
  useEffect(() => {
    if (!visible) {
      return undefined;
    }
    // Confirmations and notices time out; re-evaluate once a second while shown.
    const id = window.setInterval(() => setTick((t) => t + 1), 1000);
    return () => window.clearInterval(id);
  }, [visible]);
  const stopId = status?.stopId ?? null;
  return {
    status,
    dismiss: () => {
      if (stopId) {
        setDismissed((prev) => new Set([...prev, stopId]));
      }
    }
  };
}

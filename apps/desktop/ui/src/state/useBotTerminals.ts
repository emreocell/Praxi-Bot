import { useEffect, useState } from "react";
import type { ApiClient } from "../api/client";
import type { BotTerminal } from "../api/types";
import { TERMINAL_POLL_MS } from "../lib/liveTerminals";

/**
 * The terminals the bot runs on this computer, polled while `enabled` (the live view is open) and
 * the window is visible. Read-only: nothing here writes to a terminal. A host that does not know
 * the route (older version) or any failure simply shows none; the picture is unaffected.
 */
export function useBotTerminals(client: ApiClient | null, enabled: boolean): BotTerminal[] {
  const [terminals, setTerminals] = useState<BotTerminal[]>([]);

  useEffect(() => {
    if (!client || !enabled) {
      setTerminals([]);
      return undefined;
    }
    let stopped = false;
    let inFlight: AbortController | null = null;
    const poll = () => {
      if (inFlight || (typeof document !== "undefined" && document.hidden)) {
        return;
      }
      const ctl = new AbortController();
      inFlight = ctl;
      client
        .listBotTerminals(ctl.signal)
        .then((next) => {
          if (!stopped) {
            setTerminals((prev) => (sameTerminals(prev, next) ? prev : next));
          }
        })
        .catch(() => {
          if (!stopped) {
            setTerminals((prev) => (prev.length === 0 ? prev : []));
          }
        })
        .finally(() => {
          if (inFlight === ctl) {
            inFlight = null;
          }
        });
    };
    poll();
    const id = window.setInterval(poll, TERMINAL_POLL_MS);
    return () => {
      stopped = true;
      window.clearInterval(id);
      inFlight?.abort();
    };
  }, [client, enabled]);

  return terminals;
}

function sameTerminals(a: readonly BotTerminal[], b: readonly BotTerminal[]): boolean {
  return (
    a.length === b.length &&
    a.every((t, i) => {
      const o = b[i];
      return (
        !!o &&
        t.id === o.id &&
        t.state === o.state &&
        t.exit_code === o.exit_code &&
        t.screen === o.screen &&
        t.command === o.command
      );
    })
  );
}

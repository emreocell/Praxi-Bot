import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "../overlay/events";
import { isMockMode, MOCK_BASE_URL } from "./mock";

/** What the shell hands us after starting / finding the daemon. */
export interface Connection {
  base_url: string;
  token: string;
  started_by_app: boolean;
  fake_provider: boolean;
  data_dir: string;
}

/**
 * Ask the Tauri shell to find or start `bot serve` and return its URL + token.
 *
 * Outside Tauri (plain `vite` in a browser, dev only) the connection comes from
 * `?daemon=<url>&token=<token>[&fake=1]` so the UI can be exercised against a daemon
 * started by hand.
 */
export async function connectDaemon(restart = false): Promise<Connection> {
  if (isMockMode()) {
    return {
      base_url: MOCK_BASE_URL,
      token: "mock",
      started_by_app: false,
      fake_provider: true,
      data_dir: ""
    };
  }
  if (isTauriRuntime()) {
    return invoke<Connection>(restart ? "daemon_restart" : "daemon_connect");
  }
  if (import.meta.env.DEV) {
    const q = new URLSearchParams(window.location.search);
    const base = q.get("daemon");
    const token = q.get("token");
    if (base && token) {
      return {
        base_url: base,
        token,
        started_by_app: false,
        fake_provider: q.get("fake") === "1",
        data_dir: ""
      };
    }
  }
  throw new Error("daemon_unavailable_outside_tauri");
}

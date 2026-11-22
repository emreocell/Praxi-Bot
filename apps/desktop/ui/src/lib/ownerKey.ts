import { invoke } from "@tauri-apps/api/core";
import { MOCK_BASE_URL } from "../api/mockMode";

import { isLocalBaseUrl } from "./liveViewState";

let cached: Promise<string | undefined> | null = null;

/**
 * The owner key only the desktop app's own window can read (`owner_key` command): it proves a
 * request comes from the owner at the keyboard (sent messages, standing approvals, trusted
 * folders). `undefined` outside the app (browser preview, tests).
 */
export function ownerKey(baseUrl: string | undefined): Promise<string | undefined> {
  if (baseUrl === MOCK_BASE_URL) return Promise.resolve("mock");
  if (!baseUrl || !isLocalBaseUrl(baseUrl)) return Promise.resolve(undefined);
  cached ??= invoke<string>("owner_key").catch(() => {
    // The daemon may still be starting. A failed read must be retryable after
    // reconnecting, including when a voice call's preflight asks again.
    cached = null;
    return undefined;
  });
  return cached;
}

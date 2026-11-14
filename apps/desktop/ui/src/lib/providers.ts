import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "../overlay/events";
import { canManageProviders, PROVIDERS_LOCAL_ONLY, type ProviderManagementTarget } from "./providerConnection";

/**
 * Changing providers (adding a key to the OS keychain, choosing the default)
 * goes through the `bot` CLI the app ships with; *reading* them is the
 * daemon's `GET /v1/providers` (see `providerView.ts`), and
 * `POST /v1/providers/reload` makes the daemon pick up a change without a
 * restart.
 */

/** Provider id used by `bot serve --fake-provider` (demos and tests). */
export const FAKE_PROVIDER_ID = "fake";

export interface CliResult {
  code: number;
  stdout: string;
  stderr: string;
}

type Action = "list" | "add" | "test" | "default";

interface CliArgs {
  action: Action;
  id?: string;
  baseUrl?: string;
  model?: string;
  apiKey?: string;
}

async function providersCli(args: CliArgs, target: ProviderManagementTarget): Promise<CliResult> {
  if (!isTauriRuntime()) {
    throw new Error("providers_unavailable_outside_tauri");
  }
  if (!canManageProviders(target)) {
    throw new Error(PROVIDERS_LOCAL_ONLY);
  }
  return invoke<CliResult>("providers_cli", { ...args });
}

/** Adds (or updates) a provider; the key goes to the OS keychain via the CLI. */
export function addProvider(args: {
  id: string;
  baseUrl?: string;
  model?: string;
  apiKey?: string;
}, target: ProviderManagementTarget): Promise<CliResult> {
  return providersCli({ action: "add", ...args }, target);
}

export function testProvider(id: string, target: ProviderManagementTarget): Promise<CliResult> {
  return providersCli({ action: "test", id }, target);
}

export function setDefaultProvider(
  id: string,
  target: ProviderManagementTarget,
  model?: string
): Promise<CliResult> {
  return providersCli({ action: "default", id, model }, target);
}

export async function getAutostart(): Promise<boolean> {
  if (!isTauriRuntime()) {
    return false;
  }
  try {
    return await invoke<boolean>("get_autostart");
  } catch {
    return false;
  }
}

export async function setAutostart(enabled: boolean): Promise<boolean> {
  if (!isTauriRuntime()) {
    return enabled;
  }
  return invoke<boolean>("set_autostart", { enabled });
}

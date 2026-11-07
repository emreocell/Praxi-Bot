import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "../overlay/events";

/**
 * Providers shipped in `providers/catalog/` (the CLI embeds the same files).
 * The daemon API has no provider endpoint yet, so the list is static here.
 */
export const CATALOG_PROVIDERS = [
  "deepseek",
  "gemini-openai-compat",
  "lmstudio",
  "ollama",
  "openrouter"
] as const;

/** Provider id used by `bot serve --fake-provider` (demos and tests). */
export const FAKE_PROVIDER_ID = "fake";

export interface ConfiguredProvider {
  id: string;
  isDefault: boolean;
  model: string;
  keyName: string;
  enabled: boolean;
}

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

async function providersCli(args: CliArgs): Promise<CliResult> {
  if (!isTauriRuntime()) {
    throw new Error("providers_unavailable_outside_tauri");
  }
  return invoke<CliResult>("providers_cli", { ...args });
}

const LINE = /^([* ])\s+(\S+)\s+model=(\S+)\s+key=(\S+)\s*(\(disabled\))?\s*$/;

/** Parse the table printed by `bot providers list`. */
export function parseProviderList(stdout: string): ConfiguredProvider[] {
  const out: ConfiguredProvider[] = [];
  for (const line of stdout.split(/\r?\n/)) {
    const m = LINE.exec(line);
    if (m) {
      out.push({
        id: m[2],
        isDefault: m[1] === "*",
        model: m[3] === "-" ? "" : m[3],
        keyName: m[4] === "-" ? "" : m[4],
        enabled: m[5] === undefined
      });
    }
  }
  return out;
}

export async function listProviders(): Promise<ConfiguredProvider[]> {
  const r = await providersCli({ action: "list" });
  if (r.code !== 0) {
    throw new Error(r.stderr.trim() || r.stdout.trim());
  }
  return parseProviderList(r.stdout);
}

/** Adds (or updates) a provider; the key goes to the OS keychain via the CLI. */
export function addProvider(args: {
  id: string;
  baseUrl?: string;
  model?: string;
  apiKey?: string;
}): Promise<CliResult> {
  return providersCli({ action: "add", ...args });
}

export function testProvider(id: string): Promise<CliResult> {
  return providersCli({ action: "test", id });
}

export function setDefaultProvider(
  id: string,
  model?: string
): Promise<CliResult> {
  return providersCli({ action: "default", id, model });
}

import type { ProvidersDto } from "../api/types.ts";

type ProviderCatalogDto = ProvidersDto["catalog"][number];

/**
 * Provider guess from the shape of an API key.  Pure and local: the key is
 * only looked at, never sent anywhere or logged.  `sk-` alone is ambiguous
 * (OpenAI, DeepSeek, ...), so it is not guessed: the user picks the provider.
 */
const PREFIXES: ReadonlyArray<readonly [RegExp, string]> = [
  [/^sk-ant-/, "anthropic"],
  [/^sk-or-/, "openrouter"],
  [/^AIza/, "gemini"],
  [/^sk-proj-/, "openai"]
];

/** Catalog provider id a key most likely belongs to, or `null` when unknown. */
export function detectKeyProvider(key: string): string | null {
  const v = key.trim();
  for (const [re, id] of PREFIXES) {
    if (re.test(v)) {
      return id;
    }
  }
  return null;
}

export interface KeyTarget {
  id: string;
  keyEnv: string;
}

/**
 * Providers a key can be stored for: catalog entries that take a key from an
 * environment-style secret name.  Entries that share the secret of an earlier
 * one (the OpenAI-compatible Gemini endpoint) are folded into it.
 */
export function keyTargets(catalog: readonly ProviderCatalogDto[]): KeyTarget[] {
  const seen = new Set<string>();
  const out: KeyTarget[] = [];
  for (const c of catalog) {
    if (c.auth !== "api_key_env" || !c.key_env || seen.has(c.key_env)) {
      continue;
    }
    seen.add(c.key_env);
    out.push({ id: c.id, keyEnv: c.key_env });
  }
  return out;
}

/** The target to store under: the manual pick, else the detected provider if it is offered. */
export function resolveKeyTarget(
  targets: readonly KeyTarget[],
  detected: string | null,
  picked: string | null
): KeyTarget | null {
  const id = picked ?? detected;
  return targets.find((t) => t.id === id) ?? null;
}

/** Keys are long; a short paste is not a key yet. */
export function looksLikeKey(key: string): boolean {
  return key.trim().length > 7;
}

/** Remove an accidentally echoed secret from an error text before it is shown. */
export function scrubSecret(message: string, secret: string): string {
  const s = secret.trim();
  return s.length > 3 ? message.split(s).join("…") : message;
}

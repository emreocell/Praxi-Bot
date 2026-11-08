import type { ProvidersDto } from "../api/types.ts";
import { matchesQuery } from "./searchText.ts";

export interface ModelOption {
  /** `provider/model`, the form a bot profile stores. */
  ref: string;
  provider: string;
  model: string;
}

export interface ModelGroup {
  provider: string;
  models: ModelOption[];
}

/**
 * Models offered by the picker, grouped by provider.  Only enabled providers
 * are listed; a provider's default model and the `current` ref are included
 * even when the provider reports no listing.  The default provider comes first.
 */
export function modelGroups(
  dto: ProvidersDto | null,
  current?: string | null
): ModelGroup[] {
  const groups = new Map<string, Set<string>>();
  for (const p of dto?.configured ?? []) {
    if (!p.enabled) {
      continue;
    }
    const set = groups.get(p.id) ?? new Set<string>();
    if (p.default_model) {
      set.add(p.default_model);
    }
    for (const m of p.models) {
      set.add(m);
    }
    groups.set(p.id, set);
  }
  const cur = current?.trim();
  if (cur) {
    const i = cur.indexOf("/");
    const provider = i < 0 ? "" : cur.slice(0, i);
    const model = i < 0 ? cur : cur.slice(i + 1);
    if (provider) {
      const set = groups.get(provider) ?? new Set<string>();
      set.add(model);
      groups.set(provider, set);
    }
  }
  const defaultProvider = dto?.default_provider ?? null;
  return [...groups.entries()]
    .sort(([a], [b]) =>
      a === defaultProvider ? -1 : b === defaultProvider ? 1 : a.localeCompare(b)
    )
    .map(([provider, models]) => ({
      provider,
      models: [...models].sort().map((model) => ({
        ref: `${provider}/${model}`,
        provider,
        model
      }))
    }));
}

/** Keep the options whose provider or model matches `query`; drop empty groups. */
export function filterModelGroups(
  groups: ModelGroup[],
  query: string
): ModelGroup[] {
  return groups
    .map((g) => ({
      provider: g.provider,
      models: g.models.filter((m) => matchesQuery(query, m.provider, m.model))
    }))
    .filter((g) => g.models.length > 0);
}

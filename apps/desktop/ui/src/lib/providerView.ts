import type { ProvidersDto } from "../api/types.ts";

/** Providers shipped with the app, used only while the daemon cannot say. */
export const FALLBACK_CATALOG = [
  "deepseek",
  "gemini-openai-compat",
  "lmstudio",
  "ollama",
  "openrouter"
] as const;

export interface ProviderRow {
  id: string;
  isDefault: boolean;
  model: string;
  hasKey: boolean;
  enabled: boolean;
  /** Loaded in the running daemon (else a reload picks it up). */
  active: boolean;
  models: string[];
  modelsError: string | null;
}

export interface ProvidersView {
  configured: ProviderRow[];
  /** Catalog ids offered by "add provider". */
  catalog: string[];
  /** Every id a bot may name as its provider, sorted. */
  selectable: string[];
  /** The daemon's default provider. */
  defaultProvider: string | null;
}

/** Reshape `GET /v1/providers` for the UI (pure). */
export function providersView(dto: ProvidersDto | null): ProvidersView {
  if (!dto) {
    return {
      configured: [],
      catalog: [...FALLBACK_CATALOG],
      selectable: [...FALLBACK_CATALOG],
      defaultProvider: null
    };
  }
  const configured = dto.configured.map((p) => ({
    id: p.id,
    isDefault: p.is_default,
    model: p.default_model ?? "",
    hasKey: p.has_key,
    enabled: p.enabled,
    active: p.active,
    models: p.models,
    modelsError: p.models_error ?? null
  }));
  const catalog =
    dto.catalog.length > 0
      ? dto.catalog.map((c) => c.id)
      : [...FALLBACK_CATALOG];
  const selectable = [
    ...new Set([...dto.active, ...configured.map((p) => p.id), ...catalog])
  ].sort();
  return {
    configured,
    catalog,
    selectable,
    defaultProvider: dto.default_provider ?? null
  };
}

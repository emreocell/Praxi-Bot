import type { ProvidersDto } from "../api/types.ts";

/** Providers shipped with the app, used only while the daemon cannot say. */
export const FALLBACK_CATALOG = [
  "anthropic",
  "deepseek",
  "gemini",
  "gemini-openai-compat",
  "lmstudio",
  "ollama",
  "openrouter"
] as const;

export type OnboardingPresetGroup =
  | "gemini"
  | "anthropic"
  | "openai_compatible"
  | "ollama"
  | "fake";

export interface ProviderRoleDefaults {
  defaultModel: string;
  executor: string;
  planner: string;
  classifier: string;
  requiresKey: boolean;
  defaultBaseUrl: string;
}

/**
 * Recommended default model and role assignments (`executor`, `planner`,
 * `classifier`) for a provider during first-run onboarding.
 */
export function providerRoleDefaults(
  providerId: string,
  chosenModel?: string
): ProviderRoleDefaults {
  const trimmed = chosenModel?.trim() ?? "";
  switch (providerId) {
    case "gemini":
    case "gemini-openai-compat":
      return {
        defaultModel: "gemini-2.5-flash-lite",
        executor: trimmed || "gemini-2.5-flash-lite",
        planner: "gemini-3.1-pro-preview",
        classifier: "gemini-2.5-flash-lite",
        requiresKey: true,
        defaultBaseUrl: ""
      };
    case "anthropic":
      return {
        defaultModel: "claude-sonnet-5-5",
        executor: trimmed || "claude-sonnet-5-5",
        planner: "claude-opus-5-5",
        classifier: "claude-haiku-5-5",
        requiresKey: true,
        defaultBaseUrl: ""
      };
    case "ollama":
      return {
        defaultModel: "qwen3:8b",
        executor: trimmed || "qwen3:8b",
        planner: trimmed || "qwen3:8b",
        classifier: trimmed || "qwen3:8b",
        requiresKey: false,
        defaultBaseUrl: "http://localhost:11434/v1"
      };
    case "lmstudio":
      return {
        defaultModel: "local-model",
        executor: trimmed || "local-model",
        planner: trimmed || "local-model",
        classifier: trimmed || "local-model",
        requiresKey: false,
        defaultBaseUrl: "http://localhost:1234/v1"
      };
    case "deepseek":
      return {
        defaultModel: "deepseek-chat",
        executor: trimmed || "deepseek-chat",
        planner: "deepseek-reasoner",
        classifier: "deepseek-chat",
        requiresKey: true,
        defaultBaseUrl: "https://api.deepseek.com/v1"
      };
    case "fake":
      return {
        defaultModel: "echo",
        executor: trimmed || "echo",
        planner: trimmed || "echo",
        classifier: trimmed || "echo",
        requiresKey: false,
        defaultBaseUrl: ""
      };
    case "openrouter":
    default:
      return {
        defaultModel: "openai/gpt-4o-mini",
        executor: trimmed || "openai/gpt-4o-mini",
        planner: trimmed || "openai/gpt-4o",
        classifier: trimmed || "openai/gpt-4o-mini",
        requiresKey: true,
        defaultBaseUrl: "https://openrouter.ai/api/v1"
      };
  }
}

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

/** True when `id` is a CLI-backed provider (no API key or HTTP endpoint). */
export function isCliProvider(
  id: string,
  dto?: ProvidersDto | null
): boolean {
  if (id.startsWith("cli:")) {
    return true;
  }
  return dto?.catalog.some((c) => c.id === id && c.kind === "cli") ?? false;
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
  const nonCliCatalog = dto.catalog.filter((c) => c.kind !== "cli");
  const catalog =
    dto.catalog.length > 0
      ? nonCliCatalog.map((c) => c.id)
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

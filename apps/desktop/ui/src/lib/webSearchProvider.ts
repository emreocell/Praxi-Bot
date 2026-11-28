import type { WebSearchConfig, WebSearchConfigInput } from "../api/types";

/** The search providers the owner sets up in Settings: a key or an address, never scraping. */
export type ProviderId = "brave" | "tavily" | "searxng" | "firecrawl";

export const PROVIDERS: readonly ProviderId[] = ["brave", "tavily", "searxng", "firecrawl"];

/** Whether a provider is already set up (a key is stored, or an address is saved). */
export function providerConfigured(id: ProviderId, cfg: WebSearchConfig): boolean {
  if (id === "brave") {
    return cfg.brave_key_set;
  }
  if (id === "tavily") {
    return cfg.tavily_key_set;
  }
  if (id === "firecrawl") return Boolean(cfg.firecrawl_url || cfg.firecrawl_key_set);
  return Boolean(cfg.searxng_url);
}

/** The provider the page opens on: the first one that is set up, else Brave. */
export function initialProvider(cfg: WebSearchConfig): ProviderId {
  return PROVIDERS.find((id) => providerConfigured(id, cfg)) ?? "brave";
}

/**
 * Whether what was typed for `id` must be saved before testing: a key is always new when
 * something was typed (keys are write-only), an address only when it differs from the saved one.
 */
export function draftChanged(id: ProviderId, typed: string, cfg: WebSearchConfig): boolean {
  const value = typed.trim();
  if (value === "") {
    return false;
  }
  return id === "searxng" ? value !== (cfg.searxng_url ?? "") : true;
}

/** The `PUT /v1/web/search` body that saves what was typed for `id`. */
export function saveInput(id: ProviderId, typed: string): WebSearchConfigInput {
  const value = typed.trim();
  if (id === "brave") {
    return { brave_key: value };
  }
  if (id === "tavily") {
    return { tavily_key: value };
  }
  if (id === "firecrawl") return { firecrawl_key: value };
  return { searxng_url: value };
}

/** The body that removes provider `id` (an empty address clears SearXNG). */
export function clearInput(id: ProviderId): WebSearchConfigInput {
  if (id === "brave") {
    return { clear_brave_key: true };
  }
  if (id === "tavily") {
    return { clear_tavily_key: true };
  }
  if (id === "firecrawl") return { firecrawl_url: "", clear_firecrawl_key: true, firecrawl_scrape: false };
  return { searxng_url: "" };
}

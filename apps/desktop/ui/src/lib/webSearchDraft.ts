import type { WebSearchConfig, WebSearchConfigInput } from "../api/types";

export interface WebSearchDraft {
  searxngUrl: string;
  braveKey: string;
  tavilyKey: string;
}

/**
 * Apply a `PUT /v1/web/search` response to the in-progress credential draft.
 * Fields that were NOT part of the request keep their unsaved user edits so
 * reordering backends or toggling keyless switches never wipes a typed URL or key.
 */
export function applyWebSearchUpdate(
  draft: WebSearchDraft,
  sent: WebSearchConfigInput,
  updated: WebSearchConfig
): WebSearchDraft {
  return {
    searxngUrl:
      sent.searxng_url !== undefined ? (updated.searxng_url ?? "") : draft.searxngUrl,
    braveKey: sent.brave_key !== undefined ? "" : draft.braveKey,
    tavilyKey: sent.tavily_key !== undefined ? "" : draft.tavilyKey
  };
}

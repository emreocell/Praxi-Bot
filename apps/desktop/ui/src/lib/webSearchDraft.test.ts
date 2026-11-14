import assert from "node:assert/strict";
import { test } from "node:test";
import type { WebSearchConfig } from "../api/types.ts";
import { applyWebSearchUpdate, type WebSearchDraft } from "./webSearchDraft.ts";

const baseConfig: WebSearchConfig = {
  order: ["searxng", "brave", "tavily", "duckduckgo", "duckduckgo_lite", "bing"],
  active_backends: ["duckduckgo", "duckduckgo_lite", "bing"],
  searxng_url: null,
  brave_key_set: false,
  tavily_key_set: false,
  duckduckgo: true,
  duckduckgo_lite: true,
  bing: true
};

test("patch reorder or keyless toggle preserves unsaved searxngUrl and key inputs", () => {
  const draft: WebSearchDraft = {
    searxngUrl: "http://127.0.0.1:8080",
    braveKey: "BSA-in-progress",
    tavilyKey: "tvly-in-progress"
  };

  const afterToggle = applyWebSearchUpdate(draft, { bing: false }, { ...baseConfig, bing: false });
  assert.deepEqual(afterToggle, draft);

  const afterOrder = applyWebSearchUpdate(
    afterToggle,
    { order: ["bing", "duckduckgo", "duckduckgo_lite", "searxng", "brave", "tavily"] },
    baseConfig
  );
  assert.deepEqual(afterOrder, draft);
});

test("credential save updates only the fields included in the request", () => {
  const draft: WebSearchDraft = {
    searxngUrl: "http://127.0.0.1:8080",
    braveKey: "BSA-new",
    tavilyKey: "tvly-unsaved"
  };

  const afterSave = applyWebSearchUpdate(
    draft,
    { searxng_url: "http://127.0.0.1:8080", brave_key: "BSA-new" },
    { ...baseConfig, searxng_url: "http://127.0.0.1:8080", brave_key_set: true }
  );
  assert.equal(afterSave.searxngUrl, "http://127.0.0.1:8080");
  assert.equal(afterSave.braveKey, "");
  assert.equal(afterSave.tavilyKey, "tvly-unsaved");
});

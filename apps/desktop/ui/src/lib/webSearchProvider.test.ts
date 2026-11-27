import assert from "node:assert/strict";
import { test } from "node:test";
import type { WebSearchConfig } from "../api/types.ts";
import { MockApiClient } from "../api/mock.ts";
import {
  PROVIDERS,
  clearInput,
  draftChanged,
  initialProvider,
  providerConfigured,
  saveInput
} from "./webSearchProvider.ts";

function config(over: Partial<WebSearchConfig> = {}): WebSearchConfig {
  return {
    order: ["searxng", "brave", "tavily", "duckduckgo", "duckduckgo_lite", "bing"],
    active_backends: ["duckduckgo", "duckduckgo_lite", "bing"],
    searxng_url: null,
    brave_key_set: false,
    tavily_key_set: false,
    duckduckgo: true,
    duckduckgo_lite: true,
    bing: true,
    ...over
  };
}

test("a provider counts as set up when its key or address is stored", () => {
  assert.deepEqual(
    PROVIDERS.map((id) => providerConfigured(id, config())),
    [false, false, false]
  );
  assert.equal(providerConfigured("brave", config({ brave_key_set: true })), true);
  assert.equal(providerConfigured("tavily", config({ tavily_key_set: true })), true);
  assert.equal(
    providerConfigured("searxng", config({ searxng_url: "http://127.0.0.1:8080" })),
    true
  );
});

test("the page opens on the provider that is set up, else on Brave", () => {
  assert.equal(initialProvider(config()), "brave");
  assert.equal(initialProvider(config({ tavily_key_set: true })), "tavily");
  assert.equal(
    initialProvider(config({ searxng_url: "http://127.0.0.1:8080", tavily_key_set: true })),
    "tavily"
  );
  assert.equal(initialProvider(config({ searxng_url: "http://127.0.0.1:8080" })), "searxng");
});

test("typed text is saved before the test; an unchanged address and an empty field are not", () => {
  const set = config({ searxng_url: "http://127.0.0.1:8080", brave_key_set: true });
  assert.equal(draftChanged("brave", "", set), false);
  assert.equal(draftChanged("brave", "   ", set), false);
  // a stored key can only be replaced, so anything typed is a change
  assert.equal(draftChanged("brave", "BSA-new", set), true);
  assert.equal(draftChanged("searxng", "http://127.0.0.1:8080", set), false);
  assert.equal(draftChanged("searxng", " http://127.0.0.1:8080 ", set), false);
  assert.equal(draftChanged("searxng", "http://127.0.0.1:9090", set), true);
  assert.equal(draftChanged("searxng", "http://127.0.0.1:8080", config()), true);
});

test("each provider saves and clears through its own fields, and trims what was typed", () => {
  assert.deepEqual(saveInput("brave", "  BSA-1 "), { brave_key: "BSA-1" });
  assert.deepEqual(saveInput("tavily", "tvly-1"), { tavily_key: "tvly-1" });
  assert.deepEqual(saveInput("searxng", " http://127.0.0.1:8080 "), {
    searxng_url: "http://127.0.0.1:8080"
  });
  assert.deepEqual(clearInput("brave"), { clear_brave_key: true });
  assert.deepEqual(clearInput("tavily"), { clear_tavily_key: true });
  assert.deepEqual(clearInput("searxng"), { searxng_url: "" });
});

test("the mock follows the same flow: save a provider, test it, remove it", async () => {
  const mock = new MockApiClient();
  const saved = await mock.updateWebSearch(saveInput("tavily", "tvly-secret"));
  assert.equal(saved.tavily_key_set, true);
  assert.equal(providerConfigured("tavily", saved), true);
  const probe = await mock.testWebSearch({ backend: "tavily" });
  assert.equal(probe.ok, true);
  assert.equal(probe.backend, "tavily");
  assert.equal(probe.blocked ?? false, false);
  const cleared = await mock.updateWebSearch(clearInput("tavily"));
  assert.equal(providerConfigured("tavily", cleared), false);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProvidersDto } from "../api/types.ts";
import { filterModelGroups, modelGroups } from "./modelPicker.ts";
import { foldText, matchesQuery } from "./searchText.ts";

const dto = {
  active: [],
  catalog: [],
  default_provider: "gemini",
  configured: [
    { id: "anthropic", active: true, custom: false, default_model: "claude-sonnet-5-5", enabled: true, has_key: true, is_default: false, models: ["claude-opus-5-5"] },
    { id: "gemini", active: true, custom: false, default_model: "flash", enabled: true, has_key: true, is_default: true, models: [] },
    { id: "off", active: false, custom: false, default_model: "x", enabled: false, has_key: false, is_default: false, models: ["x"] }
  ]
} as ProvidersDto;

test("modelGroups groups by provider, default first, skips disabled", () => {
  const g = modelGroups(dto);
  assert.deepEqual(
    g.map((x) => x.provider),
    ["gemini", "anthropic"]
  );
  assert.deepEqual(
    g[1].models.map((m) => m.ref),
    ["anthropic/claude-opus-5-5", "anthropic/claude-sonnet-5-5"]
  );
});

test("modelGroups keeps the current model even when unlisted", () => {
  const g = modelGroups(dto, "ollama/qwen3:8b");
  assert.ok(g.some((x) => x.provider === "ollama" && x.models[0].model === "qwen3:8b"));
  assert.deepEqual(modelGroups(null), []);
});

test("filterModelGroups searches provider and model", () => {
  const g = modelGroups(dto);
  assert.deepEqual(
    filterModelGroups(g, "opus").map((x) => x.provider),
    ["anthropic"]
  );
  assert.equal(filterModelGroups(g, "gemini").length, 1);
  assert.deepEqual(filterModelGroups(g, "nomatch"), []);
});

test("search folding handles Turkish letters and diacritics", () => {
  assert.equal(foldText("İstanbul ÇIKIŞ ığü"), "istanbul cikis igu");
  assert.ok(matchesQuery("baglan", "Bağlandı"));
  assert.ok(matchesQuery("tema renk", "Tema", "Vurgu renk"));
  assert.ok(!matchesQuery("tema xyz", "Tema"));
  assert.ok(matchesQuery("", "anything"));
});

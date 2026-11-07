import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProvidersDto } from "../api/types.ts";
import { FALLBACK_CATALOG, providersView } from "./providerView.ts";

const dto: ProvidersDto = {
  default_provider: "second",
  active: ["fake", "second"],
  configured: [
    {
      id: "second",
      enabled: true,
      is_default: true,
      default_model: "m1",
      has_key: true,
      custom: false,
      active: true,
      models: ["m1", "m2"],
      models_error: null
    },
    {
      id: "ghost",
      enabled: false,
      is_default: false,
      default_model: null,
      has_key: false,
      custom: true,
      active: false,
      models: [],
      models_error: "timed out"
    }
  ],
  catalog: [
    {
      id: "aaa",
      kind: "openai_compatible",
      base_url: "https://x.test",
      auth: "none",
      key_env: null,
      tools: true,
      vision: false,
      configured: false
    }
  ]
};

test("the view merges active, configured and catalog providers", () => {
  const v = providersView(dto);
  assert.deepEqual(v.catalog, ["aaa"]);
  assert.deepEqual(v.selectable, ["aaa", "fake", "ghost", "second"]);
  assert.equal(v.defaultProvider, "second");
  assert.deepEqual(v.configured[0], {
    id: "second",
    isDefault: true,
    model: "m1",
    hasKey: true,
    enabled: true,
    active: true,
    models: ["m1", "m2"],
    modelsError: null
  });
  assert.equal(v.configured[1].model, "");
  assert.equal(v.configured[1].modelsError, "timed out");
});

test("without a daemon answer the shipped catalog is offered", () => {
  const v = providersView(null);
  assert.deepEqual(v.catalog, [...FALLBACK_CATALOG]);
  assert.equal(v.configured.length, 0);
  assert.equal(v.defaultProvider, null);
});

test("a daemon without catalog entries (fake provider) falls back too", () => {
  const v = providersView({ ...dto, catalog: [] });
  assert.deepEqual(v.catalog, [...FALLBACK_CATALOG]);
});

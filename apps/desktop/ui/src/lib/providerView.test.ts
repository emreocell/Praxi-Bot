import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProvidersDto } from "../api/types.ts";
import {
  FALLBACK_CATALOG,
  isCliProvider,
  providerRoleDefaults,
  providersView
} from "./providerView.ts";

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

test("providerRoleDefaults populates executor, planner, and classifier per preset", () => {
  const gemini = providerRoleDefaults("gemini");
  assert.equal(gemini.executor, "gemini-2.5-flash-lite");
  assert.equal(gemini.planner, "gemini-3.1-pro-preview");
  assert.equal(gemini.classifier, "gemini-2.5-flash-lite");
  assert.equal(gemini.requiresKey, true);

  const anthropic = providerRoleDefaults("anthropic", "claude-sonnet-5-5-custom");
  assert.equal(anthropic.executor, "claude-sonnet-5-5-custom");
  assert.equal(anthropic.planner, "claude-opus-5-5");
  assert.equal(anthropic.classifier, "claude-haiku-5-5");

  const ollama = providerRoleDefaults("ollama");
  assert.equal(ollama.requiresKey, false);
  assert.equal(ollama.defaultBaseUrl, "http://localhost:11434/v1");
});

test("providersView excludes cli catalog entries from add-provider catalog while keeping them selectable", () => {
  const withCli: ProvidersDto = {
    ...dto,
    active: ["cli:claude", "second"],
    configured: [
      ...dto.configured,
      {
        id: "cli:claude",
        enabled: true,
        is_default: false,
        default_model: null,
        has_key: false,
        custom: false,
        active: true,
        models: [],
        models_error: null
      }
    ],
    catalog: [
      ...dto.catalog,
      {
        id: "cli:claude",
        kind: "cli",
        base_url: "",
        auth: "none",
        key_env: null,
        tools: true,
        vision: false,
        configured: true
      }
    ]
  };
  const v = providersView(withCli);
  assert.deepEqual(v.catalog, ["aaa"]);
  assert.ok(v.selectable.includes("cli:claude"));
  assert.equal(isCliProvider("cli:claude", withCli), true);
  assert.equal(isCliProvider("openai", withCli), false);
  assert.ok(v.configured.some((r) => r.id === "cli:claude"));
});

import assert from "node:assert/strict";
import { test } from "node:test";
import type { ProvidersDto } from "../api/types.ts";
import { sourceLabel } from "./modelBrand.ts";
import { isPlaceholderModel } from "./placeholderModel.ts";
import { engineRows, filterModelGroups, hasConnectedModel, isCliDefault, isNoiseModel, modelGroups, parseModel, pickerSections, recommendedOptions } from "./modelPicker.ts";
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

test("modelGroups wires daemon display names and recommendation order without changing ids", () => {
  const named = structuredClone(dto);
  named.configured[0].model_details = [
    { id: "claude-sonnet-5-5", display_name: "Claude Sonnet 5.5", kind: "chat", recommended: true, recommended_rank: 1, deprecated: false, note: null },
    { id: "claude-opus-5-5", display_name: "Claude Opus 5.5", kind: "chat", recommended: true, recommended_rank: 2, deprecated: false, note: null }
  ];
  const groups = modelGroups(named, "anthropic/custom-id");
  const models = groups.find((g) => g.provider === "anthropic")?.models;
  assert.deepEqual(models?.map((m) => m.ref), [
    "anthropic/claude-sonnet-5-5", "anthropic/claude-opus-5-5", "anthropic/custom-id"
  ]);
  assert.equal(models?.[0].displayName, "Claude Sonnet 5.5");
  assert.equal(models?.[2].displayName, "Custom Id");
  assert.equal(models?.[2].recommended, false);
  assert.equal(filterModelGroups(groups, "Sonnet 5.5")[0].models[0].ref, "anthropic/claude-sonnet-5-5");
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

test("metadata-aware filtering does not reinsert a retired or non-chat default", () => {
  const filtered = structuredClone(dto);
  filtered.configured[1].default_model = "gemini-2.0-flash";
  filtered.configured[1].models = ["gemini-4-carbon"];
  filtered.configured[1].model_details = [
    { id: "gemini-4-carbon", display_name: "Gemini 4 Carbon", kind: "chat", recommended: true, recommended_rank: 1, deprecated: false, note: null }
  ];
  const ids = modelGroups(filtered).find((g) => g.provider === "gemini")?.models.map((m) => m.model);
  assert.deepEqual(ids, ["gemini-4-carbon"]);
  // An existing bot's explicit current id remains editable and executable.
  const current = modelGroups(filtered, "gemini/gemini-2.0-flash")
    .find((g) => g.provider === "gemini")?.models.map((m) => m.model);
  assert.ok(current?.includes("gemini-2.0-flash"));
});

test("search folding handles Turkish letters and diacritics", () => {
  assert.equal(foldText("İstanbul ÇIKIŞ ığü"), "istanbul cikis igu");
  assert.ok(matchesQuery("baglan", "Bağlandı"));
  assert.ok(matchesQuery("tema renk", "Tema", "Vurgu renk"));
  assert.ok(!matchesQuery("tema xyz", "Tema"));
  assert.ok(matchesQuery("", "anything"));
});

test("modelGroups includes enabled CLI providers with single default model and excludeCli filters them out", () => {
  const withCli = {
    active: ["cli:claude", "gemini"],
    default_provider: "gemini",
    configured: [
      {
        id: "gemini",
        active: true,
        custom: false,
        default_model: "flash",
        enabled: true,
        has_key: true,
        is_default: true,
        models: []
      },
      {
        id: "cli:claude",
        active: true,
        custom: false,
        default_model: null,
        enabled: true,
        has_key: false,
        is_default: false,
        models: []
      }
    ],
    catalog: [
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
  } as unknown as ProvidersDto;

  const execGroups = modelGroups(withCli);
  const cliGroup = execGroups.find((g) => g.provider === "cli:claude");
  assert.ok(cliGroup, "cli:claude should be present in executor groups");
  assert.deepEqual(
    cliGroup.models.map((m) => m.ref),
    ["cli:claude/default"]
  );

  const classifierGroups = modelGroups(withCli, null, { includeCli: false });
  assert.equal(
    classifierGroups.some((g) => g.provider === "cli:claude"),
    false
  );
});

test("recommendations: newest per family replaces older, free-first for aggregators, no CLI default row", () => {
  const det = (id: string, extra: Record<string, unknown> = {}) =>
    ({ id, display_name: id, kind: "chat", recommended: false, recommended_rank: null, deprecated: false, note: null, ...extra }) as never;
  const provider = (id: string, ids: string[], free: string[] = []) =>
    ({ id, active: true, custom: false, default_model: null, enabled: true, has_key: true, is_default: false,
      models: ids, model_details: ids.map((i) => det(i, free.includes(i) ? { note: "free" } : {})) }) as never;
  const claude = ["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-4-1-20250805", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-3-5-haiku-20241022"];
  const d = {
    active: [], catalog: [], default_provider: "anthropic",
    configured: [
      provider("anthropic", claude),
      provider("cli:claude", ["default", "haiku", "sonnet", "opus", "fable"]),
      provider("cli:opencode", ["default", "google/gemini-2.5-pro", "opencode/grok-code", "opencode/big-pickle"], ["opencode/grok-code", "opencode/big-pickle"]),
      provider("openrouter", ["anthropic/claude-sonnet-5.5", "meta/llama-4:free", "qwen/qwen3-235b:free", "qwen/qwen3-30b:free", ...Array.from({ length: 50 }, (_, i) => `v/m-${i}`)]),
      provider("gemini", ["gemini-2.5-pro", "gemini-3.5-flash", "gemini-3.5-flash-lite", "gemini-2.5-flash-lite"])
    ]
  } as unknown as ProvidersDto;
  const groups = modelGroups(d);
  const sec = pickerSections(groups);
  const ids = (g: { recommended: { option: { model: string } }[] }) => g.recommended.map((r) => r.option.model);
  const api = (id: string) => sec.api.find((g) => g.provider === id)!;
  const cli = (id: string) => sec.cli.find((g) => g.provider === id)!;

  // (a) newest of each family, strongest tier first; a newer 5.x replaces the older one.
  assert.deepEqual(ids(api("anthropic")), ["claude-fable-5-2", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"]);
  assert.equal(api("anthropic").recommended[0].hint, "new");
  assert.deepEqual(ids(cli("cli:claude")), ["fable", "opus", "sonnet", "haiku"]);
  // No "the CLI decides" row anywhere (it stays selectable only as a stored value).
  for (const g of [...sec.cli, ...sec.api]) {
    assert.ok(!g.recommended.some((r) => isCliDefault(r.option)) && !g.rest.some(isCliDefault));
  }
  // (b) free models first for multi-vendor engines and aggregators.
  assert.deepEqual(ids(cli("cli:opencode")).slice(0, 2).sort(), ["opencode/big-pickle", "opencode/grok-code"]);
  assert.ok(api("openrouter").recommended.slice(0, 3).every((r) => r.hint === "free"));
  assert.ok(api("openrouter").rest.length > 40);
  // (c) Gemini: high-quota Flash-Lite / Flash first, Pro last.
  assert.deepEqual(ids(api("gemini")), ["gemini-3.5-flash-lite", "gemini-3.5-flash", "gemini-2.5-pro"]);
  assert.equal(api("gemini").recommended[0].hint, "quota");
  // Top strip: one per source, CLI first, no duplicate family.
  const strip = recommendedOptions(groups);
  assert.equal(strip.length, 4);
  assert.deepEqual(strip.slice(0, 2).map((r) => r.option.ref), ["cli:claude/fable", "cli:opencode/opencode/big-pickle"]);
  // A stored CLI default stays selectable and is labelled by the engine.
  assert.equal(modelGroups(d, "cli:claude/default").find((g) => g.provider === "cli:claude")!.models.some(isCliDefault), true);
  assert.equal(parseModel("claude-3-5-sonnet-20241022").family, parseModel("claude-sonnet-5-5").family);
  assert.equal(sourceLabel("cli:opencode"), "OpenCode CLI");
  assert.ok(isNoiseModel(modelGroups(d, null).find((g) => g.provider === "anthropic")!.models.find((m) => m.model === "claude-3-5-haiku-20241022")!));
});

test("engineRows: installed-not-enabled connect first, uninstalled install last, enabled hidden", () => {
  const e = (id: string, installed: boolean, install: boolean, runnable = true) =>
    ({ detected: { id, display_name: id, installed, runnable }, install: install ? { command: "npm" } : null }) as never;
  const rows = engineRows([e("opencode", false, true), e("claude", true, true), e("codex", true, true), e("chatgpt-plan", false, false), e("cursor", true, true, false)], new Set(["claude"]));
  assert.deepEqual(rows.map((r) => `${r.action}:${r.id}`), ["connect:codex", "install:opencode"]);
});

test("model names fall back to a prettified id and a CLI model shows as a name", () => {
  const g = modelGroups(dto, "cli:opencode/google/gemini-2.5-pro");
  const m = g.find((x) => x.provider === "cli:opencode")!.models[0];
  assert.notEqual(m.displayName, m.model);
});

test("a CLI without any listed model is never a group: it is reported apart", () => {
  const d = {
    active: [],
    catalog: [],
    default_provider: "gemini",
    configured: [
      { id: "gemini", active: true, custom: false, default_model: "flash", enabled: true, has_key: true, is_default: true, models: [] },
      { id: "cli:claude", active: true, custom: false, default_model: null, enabled: true, has_key: false, is_default: false, models: ["opus", "sonnet"] },
      { id: "cli:codex", active: true, custom: false, default_model: null, enabled: true, has_key: false, is_default: false, models: [] },
      { id: "cli:agy", active: true, custom: false, default_model: null, enabled: true, has_key: false, is_default: false, models: [] }
    ]
  } as unknown as ProvidersDto;
  const sec = pickerSections(modelGroups(d));
  assert.deepEqual(sec.cli.map((g) => g.provider), ["cli:claude"]);
  assert.deepEqual(sec.emptyCli.sort(), ["cli:agy", "cli:codex"]);
  for (const g of [...sec.cli, ...sec.api]) {
    assert.ok(g.cli ? g.total > 0 : true, `${g.provider} is rendered with 0 models`);
  }
  // The current choice keeps its group visible.
  assert.deepEqual(pickerSections(modelGroups(d, "cli:codex/default"), "cli:codex/default").cli.map((g) => g.provider), ["cli:claude", "cli:codex"]);
});

test("the validation-only placeholder never reaches the picker, search or Recommended; alone it means no model yet", () => {
  const placeholder = {
    id: "validation-only", active: true, custom: false, default_model: "validation-placeholder",
    enabled: true, has_key: false, is_default: true, models: ["validation-placeholder"]
  };
  const only = { active: [], catalog: [], default_provider: "validation-only", configured: [placeholder] } as unknown as ProvidersDto;
  assert.deepEqual(modelGroups(only, "validation-only/validation-placeholder"), []);
  assert.equal(hasConnectedModel(only), false);
  const mixed = { ...dto, configured: [placeholder, ...dto.configured] } as unknown as ProvidersDto;
  const groups = modelGroups(mixed, "validation-only/validation-placeholder");
  assert.ok(groups.every((g) => g.provider !== "validation-only"));
  assert.equal(filterModelGroups(groups, "validation").length, 0);
  assert.ok(recommendedOptions(groups).every((r) => !isPlaceholderModel(r.option.ref)));
  assert.equal(hasConnectedModel(mixed), true);
});

import assert from "node:assert/strict";
import { test } from "node:test";
import type { ModelGroup, ModelOption } from "./modelPicker.ts";
import { parseRecentModels, recentOptions, RECENT_MODELS_MAX, withRecent } from "./recentModels.ts";

const opt = (provider: string, model: string): ModelOption => ({
  ref: `${provider}/${model}`, provider, model, displayName: model, recommended: false,
  recommendedRank: null, kind: null, deprecated: false, note: null
});

test("withRecent puts the pick first, drops the duplicate and keeps three", () => {
  assert.deepEqual(withRecent([], "a/x"), ["a/x"]);
  assert.deepEqual(withRecent(["a/x", "b/y"], "b/y"), ["b/y", "a/x"]);
  assert.deepEqual(withRecent(["a/1", "a/2", "a/3"], "a/4"), ["a/4", "a/1", "a/2"]);
  assert.equal(withRecent(["a/1", "a/2", "a/3"], "a/4").length, RECENT_MODELS_MAX);
  assert.deepEqual(withRecent(["a/1"], "  "), ["a/1"]);
});

test("parseRecentModels reads only a JSON list of provider/model refs", () => {
  assert.deepEqual(parseRecentModels('["a/x","b/y"]'), ["a/x", "b/y"]);
  assert.deepEqual(parseRecentModels('["a/x", 3, "nope"]'), ["a/x"]);
  assert.deepEqual(parseRecentModels("{"), []);
  assert.deepEqual(parseRecentModels(null), []);
  assert.deepEqual(parseRecentModels('{"a":1}'), []);
});

test("recentOptions keeps the recent order and skips models no longer offered", () => {
  const groups: ModelGroup[] = [
    { provider: "anthropic", models: [opt("anthropic", "claude-opus-5-5"), opt("anthropic", "claude-sonnet-5-5")] },
    { provider: "cli:codex", models: [opt("cli:codex", "gpt-6.1-sol")] }
  ];
  const got = recentOptions(groups, ["cli:codex/gpt-6.1-sol", "gone/model", "anthropic/claude-sonnet-5-5"]);
  assert.deepEqual(got.map((m) => m.ref), ["cli:codex/gpt-6.1-sol", "anthropic/claude-sonnet-5-5"]);
});

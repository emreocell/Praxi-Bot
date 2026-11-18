import assert from "node:assert/strict";
import { test } from "node:test";
import { modelDisplayName, modelLine } from "./modelName.ts";

test("model ids become human names", () => {
  const cases: Array<[string, string]> = [
    ["gemini-3.5-flash-lite", "Gemini 3.5 Flash Lite"],
    ["gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview"],
    ["claude-sonnet-5-5", "Claude Sonnet 5.5"],
    ["claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet"],
    ["claude-opus-4-1-2025-08-05", "Claude Opus 4.1"],
    ["claude-haiku-5-5-latest", "Claude Haiku 5.5"],
    ["gpt-5.1-mini", "GPT-5.1 Mini"],
    ["gpt-4o", "GPT-4o"],
    ["gpt-4-1106-preview", "GPT-4 1106 Preview"],
    ["o3-mini", "o3 Mini"],
    ["llama3.2:3b", "Llama 3.2 (3B)"],
    ["meta-llama/llama-3.3-70b-instruct", "Llama 3.3 70B Instruct"],
    ["deepseek-chat", "DeepSeek Chat"],
    ["my_custom_model", "My Custom Model"]
  ];
  for (const [id, name] of cases) {
    assert.equal(modelDisplayName(id), name, id);
  }
  assert.equal(modelDisplayName("  "), "");
});

test("a display name never shows the raw separator-style id", () => {
  for (const id of ["gemini-3.5-flash-lite", "claude-sonnet-5-5", "gpt-5.1-mini"]) {
    assert.notEqual(modelDisplayName(id), id);
    assert.ok(!/[a-z]-[a-z]/.test(modelDisplayName(id)), id);
  }
});

test("modelLine splits provider and model and names CLI defaults after the CLI", () => {
  assert.deepEqual(modelLine("gemini/gemini-3.5-flash-lite"), {
    name: "Gemini 3.5 Flash Lite",
    provider: "Google",
    brand: "googlegemini",
    isDefault: false
  });
  assert.deepEqual(modelLine("claude-code/default"), {
    name: "Claude Code CLI",
    provider: "Anthropic",
    brand: "claude",
    isDefault: true
  });
  // A ref without a provider is still named.
  assert.equal(modelLine("claude-sonnet-5-5").name, "Claude Sonnet 5.5");
  assert.equal(modelLine("claude-sonnet-5-5").provider, "");
  assert.deepEqual(modelLine(""), { name: "", provider: "", brand: "", isDefault: false });
  assert.deepEqual(modelLine(undefined), { name: "", provider: "", brand: "", isDefault: false });
});

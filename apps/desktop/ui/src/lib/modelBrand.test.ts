import assert from "node:assert/strict";
import { test } from "node:test";
import { productBrandKey, providerBrand } from "./modelBrand.ts";

test("providers map to their company, CLI variants carry a note", () => {
  assert.deepEqual(providerBrand("anthropic"), { company: "anthropic", label: "Anthropic" });
  assert.equal(providerBrand("claude-code").company, "anthropic");
  assert.equal(providerBrand("claude-code").note, "Claude Code CLI");
  assert.equal(providerBrand("gemini").label, "Google");
  assert.deepEqual(providerBrand("cli:gemini-cli"), {
    company: "googlegemini",
    label: "Google",
    note: "Gemini CLI"
  });
});

test("unknown providers keep their id as name and as logo key", () => {
  assert.deepEqual(providerBrand("deepseek"), { company: "deepseek", label: "Deepseek" });
  assert.equal(providerBrand("cli:cursor").note, "CLI");
});

test("rows show the product logo, not the company's", () => {
  assert.equal(productBrandKey("anthropic", "claude-sonnet-5-5"), "claude");
  assert.equal(productBrandKey("claude-code", "default"), "anthropic");
  assert.equal(productBrandKey("openai", "gpt-4.1"), "openai");
  assert.equal(productBrandKey("openai", "o3"), "openai");
  assert.equal(productBrandKey("gemini", "gemini-2.5-flash"), "googlegemini");
  assert.equal(productBrandKey("ollama", "llama3.2"), "ollama");
  assert.equal(productBrandKey("openrouter", "claude-3.7"), "claude");
});

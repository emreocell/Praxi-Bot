import assert from "node:assert/strict";
import { test } from "node:test";
import { productBrandKey, providerBrand, sourceLabel } from "./modelBrand.ts";

test("providers map to their company, CLI variants carry a note", () => {
  assert.deepEqual(providerBrand("anthropic"), { company: "anthropic", label: "Anthropic" });
  assert.equal(providerBrand("claude-code").company, "claude");
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
  assert.equal(productBrandKey("claude-code", "default"), "claude");
  assert.equal(productBrandKey("cli:claude", "sonnet"), "claude");
  // Claude Code wears the Claude starburst; only the Anthropic API keeps the Anthropic "A".
  assert.equal(providerBrand("cli:claude").company, "claude");
  assert.equal(providerBrand("anthropic").company, "anthropic");
  assert.equal(providerBrand("cli:agy").company, "antigravity");
  assert.equal(providerBrand("cli:agy").label, "Antigravity");
  assert.equal(providerBrand("cli:qwen").label, "Qwen Code");
  assert.equal(providerBrand("cli:qwen").company, "qwen");
  assert.equal(providerBrand("cli:kiro").label, "Kiro");
  assert.equal(sourceLabel("cli:kiro"), "Kiro CLI");
  assert.equal(providerBrand("cli:kiro").company, "kiro");
  assert.equal(providerBrand("cli:qoder").label, "Qoder");
  assert.equal(sourceLabel("cli:qoder"), "Qoder CLI");
  assert.equal(providerBrand("cli:copilot").label, "GitHub Copilot");
  assert.equal(sourceLabel("cli:copilot"), "GitHub Copilot CLI");
  assert.equal(providerBrand("cli:copilot").company, "githubcopilot");
  assert.equal(providerBrand("cli:kimi").label, "Kimi Code");
  assert.equal(sourceLabel("cli:kimi"), "Kimi Code CLI");
  assert.equal(providerBrand("cli:kimi").company, "kimi");
  assert.equal(providerBrand("cli:vibe").label, "Mistral Vibe");
  assert.equal(sourceLabel("cli:vibe"), "Mistral Vibe CLI");
  assert.equal(providerBrand("cli:vibe").company, "mistralai");
  assert.equal(providerBrand("cli:goose").label, "Goose");
  assert.equal(sourceLabel("cli:goose"), "Goose CLI");
  assert.equal(providerBrand("cli:goose").company, "goose");
  // Data-driven engines (`providers/cli/`) are named like any other.
  assert.equal(providerBrand("cli:cline").label, "Cline");
  assert.equal(providerBrand("cli:auggie").label, "Augment Code");
  assert.equal(providerBrand("cli:droid").label, "Factory Droid");
  assert.equal(providerBrand("cli:grok").label, "Grok Build");
  assert.equal(providerBrand("cli:junie").label, "Junie");
  assert.equal(providerBrand("cli:openhands").label, "OpenHands");
  assert.equal(productBrandKey("openai", "gpt-4.1"), "openai");
  assert.equal(productBrandKey("openai", "o3"), "openai");
  assert.equal(productBrandKey("gemini", "gemini-2.5-flash"), "googlegemini");
  assert.equal(productBrandKey("ollama", "llama3.2"), "ollama");
  assert.equal(productBrandKey("openrouter", "claude-3.7"), "claude");
});

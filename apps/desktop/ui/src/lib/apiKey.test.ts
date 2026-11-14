import assert from "node:assert/strict";
import { test } from "node:test";
import {
  detectKeyProvider,
  keyTargets,
  looksLikeKey,
  resolveKeyTarget,
  scrubSecret
} from "./apiKey.ts";

test("detectKeyProvider reads the provider off the key prefix", () => {
  assert.equal(detectKeyProvider("sk-ant-api03-abc"), "anthropic");
  assert.equal(detectKeyProvider("  sk-or-v1-abc "), "openrouter");
  assert.equal(detectKeyProvider("AIzaSyAbc"), "gemini");
  assert.equal(detectKeyProvider("sk-proj-abc"), "openai");
  assert.equal(detectKeyProvider("sk-abc"), null); // ambiguous: DeepSeek and others use sk- too
  assert.equal(detectKeyProvider("ghp_whatever"), null);
  assert.equal(detectKeyProvider(""), null);
});

const catalog = [
  { id: "anthropic", auth: "api_key_env", key_env: "ANTHROPIC_API_KEY" },
  { id: "gemini", auth: "api_key_env", key_env: "GEMINI_API_KEY" },
  { id: "gemini-openai-compat", auth: "api_key_env", key_env: "GEMINI_API_KEY" },
  { id: "ollama", auth: "none", key_env: null },
  { id: "deepseek", auth: "api_key_env", key_env: "DEEPSEEK_API_KEY" }
] as unknown as Parameters<typeof keyTargets>[0];

test("keyTargets offers key providers once per secret name", () => {
  assert.deepEqual(
    keyTargets(catalog).map((t) => t.id),
    ["anthropic", "gemini", "deepseek"]
  );
});

test("resolveKeyTarget prefers the manual pick and ignores providers not on offer", () => {
  const targets = keyTargets(catalog);
  assert.equal(resolveKeyTarget(targets, "anthropic", null)?.keyEnv, "ANTHROPIC_API_KEY");
  assert.equal(resolveKeyTarget(targets, "anthropic", "deepseek")?.id, "deepseek");
  // `openai` is detected but this daemon's catalog does not list it: the user must choose.
  assert.equal(resolveKeyTarget(targets, "openai", null), null);
  assert.equal(resolveKeyTarget(targets, null, null), null);
});

test("looksLikeKey and scrubSecret", () => {
  assert.equal(looksLikeKey("abc"), false);
  assert.equal(looksLikeKey("sk-ant-1234"), true);
  assert.equal(scrubSecret("bad key sk-secret-123 rejected", "sk-secret-123"), "bad key … rejected");
  assert.equal(scrubSecret("short", "ab"), "short");
});

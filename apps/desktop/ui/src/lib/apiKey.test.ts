import assert from "node:assert/strict";
import { test } from "node:test";
import {
  detectKeyProvider,
  keyTargets,
  looksLikeKey,
  resolveKeyTarget,
  saveProviderKey,
  scrubSecret
} from "./apiKey.ts";

const anthropicTarget = { id: "anthropic", keyEnv: "ANTHROPIC_API_KEY" };
const ownedConnection = { base_url: "http://127.0.0.1:47821", started_by_app: true };

function providerFixture() {
  const state = {
    configured: new Map<string, string>(),
    looseSecrets: new Map<string, string>(),
    active: new Set<string>(),
    order: [] as string[]
  };
  const client = {
    baseUrl: ownedConnection.base_url,
    setSecret: async (name: string, value: string) => { state.looseSecrets.set(name, value); },
    reloadProviders: async () => {
      state.order.push("reload");
      state.active = new Set(state.configured.keys());
      return { active: [...state.active] };
    }
  };
  const configure = async ({ id, apiKey }: { id: string; apiKey: string }) => {
    state.order.push("configure");
    state.configured.set(id, apiKey);
    return { code: 0, stdout: "added", stderr: "" };
  };
  return { state, client, configure };
}

test("the API key flow configures a new provider before reloading it", async () => {
  const { state, client, configure } = providerFixture();
  await saveProviderKey(anthropicTarget, "test-key-first", client, configure, ownedConnection);
  assert.equal(state.configured.get("anthropic"), "test-key-first");
  assert.ok(state.active.has("anthropic"));
  assert.deepEqual(state.order, ["configure", "reload"]);
  assert.equal(state.looseSecrets.size, 0);
});

test("the API key flow rotates the key referenced by an existing provider", async () => {
  const { state, client, configure } = providerFixture();
  state.configured.set("anthropic", "test-key-old");
  await saveProviderKey(anthropicTarget, "test-key-new", client, configure, ownedConnection);
  assert.equal(state.configured.get("anthropic"), "test-key-new");
});

test("failed provider configuration is reported without reloading or loose secret writes", async () => {
  const { state, client } = providerFixture();
  await assert.rejects(
    saveProviderKey(anthropicTarget, "test-key", client, async () => ({
      code: 1, stdout: "", stderr: "cannot write providers.toml"
    }), ownedConnection),
    /cannot write providers.toml/
  );
  assert.deepEqual(state.order, []);
  assert.equal(state.looseSecrets.size, 0);
});

test("failed provider reload is not reported as successful activation", async () => {
  const { client, configure } = providerFixture();
  await assert.rejects(saveProviderKey(anthropicTarget, "test-key", {
    ...client,
    reloadProviders: async () => { throw new Error("reload failed"); }
  }, configure, ownedConnection), /reload failed/);
});

for (const [name, connection, baseUrl] of [
  ["remote daemon", { base_url: "https://host.example", started_by_app: false }, "https://host.example"],
  ["loopback proxy or externally started daemon", { ...ownedConnection, started_by_app: false }, ownedConnection.base_url],
  ["mismatched active daemon", ownedConnection, "http://127.0.0.1:47822"],
  ["remote URL even with stale ownership", { base_url: "https://host.example", started_by_app: true }, "https://host.example"],
  ["missing connection", null, ownedConnection.base_url]
] as const) {
  test(`the API key flow rejects a ${name} before changing local config`, async () => {
    const { state, client, configure } = providerFixture();
    await assert.rejects(
      saveProviderKey(anthropicTarget, "test-key", { ...client, baseUrl }, configure, connection),
      /providers_require_app_daemon/
    );
    assert.equal(state.configured.size, 0);
    assert.equal(state.looseSecrets.size, 0);
    assert.deepEqual(state.order, []);
  });
}

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

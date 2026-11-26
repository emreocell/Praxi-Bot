import assert from "node:assert/strict";
import test from "node:test";
import { signedMaterial, webhookAddress } from "./incomingWebhooks.ts";

test("the address stays on the daemon and the signature binds the timestamp to the body", () => {
  assert.equal(
    webhookAddress("http://127.0.0.1:9/", "abc"),
    "http://127.0.0.1:9/v1/incoming-webhooks/abc"
  );
  assert.equal(signedMaterial("100", "call:danger_run {}"), "100.call:danger_run {}");
});

test("rotating the secret posts to the rotate route and the mock returns a new secret once", async () => {
  const { ApiClient } = await import("../api/client.ts");
  const { MockApiClient } = await import("../api/mock.ts");
  const real = globalThis.fetch;
  const seen: Array<{ method: string; url: string }> = [];
  globalThis.fetch = (async (url: URL | string, init?: RequestInit) => {
    seen.push({ method: init?.method ?? "GET", url: String(url) });
    return new Response(JSON.stringify({ webhook: { id: "h1" }, secret: "s".repeat(64) }), {
      status: 200,
      headers: { "content-type": "application/json" }
    });
  }) as typeof fetch;
  try {
    const client = new ApiClient({ base_url: "http://127.0.0.1:9", token: "token" });
    const out = await client.rotateIncomingWebhookSecret("b 1", "h1");
    assert.equal(out.secret.length, 64);
    assert.equal(seen[0]?.method, "POST");
    assert.ok(seen[0]?.url.endsWith("/bots/b%201/incoming-webhooks/h1/rotate-secret"), seen[0]?.url);
  } finally {
    globalThis.fetch = real;
  }
  const mock = new MockApiClient();
  const bot = (await mock.listBots())[0];
  assert.ok(bot);
  const made = await mock.createIncomingWebhook(bot.id, { name: "n" });
  const next = await mock.rotateIncomingWebhookSecret(bot.id, made.webhook.id);
  assert.notEqual(next.secret, made.secret);
});

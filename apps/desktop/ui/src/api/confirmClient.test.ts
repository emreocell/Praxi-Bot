import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { ApiClient } from "./client.ts";

const realFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = realFetch; });

test("confirming an automation names the revision the owner looked at", async () => {
  const calls: { method: string; path: string; body: unknown }[] = [];
  globalThis.fetch = (async (url: URL | string, input?: RequestInit) => {
    calls.push({ method: input?.method ?? "GET", path: new URL(String(url)).pathname, body: input?.body ? JSON.parse(String(input.body)) : undefined });
    return new Response("{}", { status: 200 });
  }) as typeof fetch;
  const client = new ApiClient({ base_url: "http://127.0.0.1:9", token: "test" });
  await client.confirmSchedule("sch-1", 4);
  await client.confirmTrigger("bot-1", "trg-1", 7);
  assert.deepEqual(calls, [
    { method: "POST", path: "/v1/schedules/sch-1/confirm", body: { revision: 4 } },
    { method: "POST", path: "/v1/bots/bot-1/triggers/trg-1/confirm", body: { revision: 7 } }
  ]);
});

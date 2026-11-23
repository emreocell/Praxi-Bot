import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { hostRefusalKey, onboardingAllowed } from "./hostMode.ts";
import { HOST_STATE_HOLD_MS, settleHostState } from "./hostState.ts";

const flat = (file: string): Record<string, unknown> => {
  const out: Record<string, unknown> = {};
  const walk = (o: Record<string, unknown>, prefix: string) => {
    for (const [k, v] of Object.entries(o)) {
      if (v && typeof v === "object") walk(v as Record<string, unknown>, `${prefix}${k}.`);
      else out[`${prefix}${k}`] = v;
    }
  };
  walk(JSON.parse(readFileSync(new URL(`../i18n/locales/${file}.json`, import.meta.url), "utf8")), "");
  return out;
};

test("first-run wizard is not shown when a host is selected", () => {
  assert.equal(onboardingAllowed("local"), true);
  assert.equal(onboardingAllowed("ca99609c0000000000000000000000000000000000000000000000000000abcd"), false);
});

test("every host refusal maps to a translated sentence, never the raw daemon text", () => {
  const daemon = [
    "the execution policy is managed on the host itself",
    "secrets are managed on the host itself",
    "tool servers and skills are managed on the host itself",
    "account credentials are managed on the host itself",
    "host transfer and channel pairing are done on the host itself",
    "pairing and host lifecycle are managed on the host itself",
    "permissions and tools of a bot on a host are managed on the host itself",
    "knowledge settings are managed by the local owner",
    "Composio connections are managed by the owner's desktop only",
    "only the local user answers overlay questions",
    "schedule triggers and prechecks are local-owner only",
    "peer role does not allow this operation",
    "host-local provider management is not allowed for peers",
    "connection tests spend this host's stored keys and are an owner action"
  ];
  const tr = flat("tr");
  const en = flat("en");
  for (const message of daemon) {
    const key = hostRefusalKey(403, message);
    assert.ok(key && key !== "generic", message);
    assert.ok(typeof tr[`hostMode.refusal.${key}`] === "string", `tr ${key}`);
    assert.ok(typeof en[`hostMode.refusal.${key}`] === "string", `en ${key}`);
  }
  assert.equal(hostRefusalKey(403, "something new"), "generic");
});

test("a good host connection survives a missed poll instead of flickering to connecting", () => {
  let mem = settleHostState(null, { kind: "connected" }, 0);
  mem = settleHostState(mem, { kind: "connecting" }, 3_000);
  assert.equal(mem.shown.kind, "connected");
  mem = settleHostState(mem, { kind: "connected" }, 6_000);
  assert.equal(mem.worseSince, null);
  mem = settleHostState(mem, { kind: "connecting" }, 7_000);
  mem = settleHostState(mem, { kind: "connecting" }, 7_000 + HOST_STATE_HOLD_MS);
  assert.equal(mem.shown.kind, "offline");
  mem = settleHostState(mem, { kind: "connecting" }, 30_000);
  assert.equal(mem.shown.kind, "offline");
  mem = settleHostState(mem, { kind: "connected" }, 31_000);
  assert.equal(mem.shown.kind, "connected");
});

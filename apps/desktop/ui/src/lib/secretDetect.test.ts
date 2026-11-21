import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  detectSecrets,
  isValidSecretName,
  planStore,
  removeSecrets,
  suggestedName
} from "./secretDetect.ts";

// Fake values assembled at run time so no secret-shaped literal sits in the source.
const alnum = (n: number) => "aB3dE5gH7jK9mN1pQ2rS4tU6vW8xY0z".repeat(3).slice(0, n);
const CANARY = "sk-" + alnum(30);
const GH = "ghp_" + alnum(36);

test("known provider shapes are found, with the kind and span but never the value", () => {
  const text = `use ${CANARY} and ${GH} please`;
  const found = detectSecrets(text);
  assert.deepEqual(found.map((c) => c.kind), ["openai_style_key", "github_token"]);
  assert.equal(text.slice(found[0].start, found[0].end), CANARY);
  assert.ok(!JSON.stringify(found).includes(CANARY));
});

test("prose, hashes, uuids and look-alike words are not reported", () => {
  for (const t of [
    "sk-learn-is-a-fine-library",
    "task-" + alnum(30),
    "3f2b8c1e-7a4d-4e9b-9c11-0a5d6e7f8091",
    "9fceb02d0ae598e95dc970b74767f19372d61af8",
    "Ajzashkjdgasda",
    "password: kedi",
    "[secret: pasted/openai-style-key]"
  ]) {
    assert.deepEqual(detectSecrets(t), [], t);
  }
});

test("storing swaps the value for a placeholder: the text keeps the name only", () => {
  const text = `key ${CANARY} end`;
  const plan = planStore(text, detectSecrets(text), "pasted/openai");
  assert.deepEqual(plan.entries, [{ name: "pasted/openai", value: CANARY }]);
  assert.equal(plan.text, "key [secret: pasted/openai] end");
  assert.ok(!plan.text.includes(CANARY));
  assert.deepEqual(detectSecrets(plan.text), [], "a placeholder is never a secret");
});

test("removing drops every span and leaves the rest", () => {
  const text = `a ${CANARY} b ${GH} c`;
  assert.equal(removeSecrets(text, detectSecrets(text)), "a  b  c");
});

test("vault names follow the daemon rule", () => {
  assert.ok(isValidSecretName(suggestedName("openai_style_key")));
  for (const bad of ["", "a b", "x__y", "composio/x", "../x", "/x", "a".repeat(65)]) {
    assert.ok(!isValidSecretName(bad), bad);
  }
});

// ---- leak canary (#421 slice 4): a secret typed in the composer reaches no store of the app ----

const SRC = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

test("the composer never persists the draft: no storage, console or telemetry call touches it", () => {
  const chat = SRC("../views/ChatView.tsx");
  // The draft lives in React state only; ChatView has no browser-storage write at all.
  assert.ok(!/localStorage|sessionStorage|indexedDB/.test(chat));
  // Nothing in the secret UI or detector logs, stores or sends the text it handles.
  for (const f of [
    "../components/PastedSecretWarning.tsx",
    "../components/SecretRequestCard.tsx",
    "./secretDetect.ts",
    "./secretRequestForm.ts"
  ]) {
    const src = SRC(f);
    assert.ok(!/console\.|localStorage|sessionStorage|indexedDB|sendBeacon|telemetry/.test(src), f);
  }
});

test("the only calls that carry a typed value are the vault and the secret-request routes", () => {
  const chat = SRC("../views/ChatView.tsx");
  assert.equal((chat.match(/createFillSecret\(/g) ?? []).length, 1);
  // The warning keeps kinds (strings), never offsets or text, in state.
  assert.ok(/setSecretKinds\(found\.map\(\(c\) => c\.kind\)\)/.test(chat));
  const card = SRC("../components/SecretRequestCard.tsx");
  // The value inputs are uncontrolled: no `value=` prop and no state setter receives them.
  assert.ok(!/<input[^>]*\bvalue=/.test(card));
  assert.ok(!/useState<[^>]*(password|secret|value)/i.test(card));
  assert.ok(/type="password"/.test(card) && /autoComplete: "off"/.test(card));
});

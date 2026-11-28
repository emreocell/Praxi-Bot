import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  detectSecrets,
  isValidSecretName,
  kindsToWarn,
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

// The Rust detector's corpus (crates/secret-store/tests): the same rows must give the same answer here.
interface Row { text: string; value: string; kind?: string }
interface Stress { name: string; unit: string; times: number; found?: number }
interface Shape { kind: string; prefix: string; body: string }
const CORPUS = JSON.parse(
  readFileSync(new URL("../../../../../crates/secret-store/tests/fixtures/detect-corpus.json", import.meta.url), "utf8")
) as { shapes: Shape[]; labelled: Row[]; cards: Row[]; negatives: string[]; stress: Stress[] };

test("every provider shape is found with its kind and exact span, in every template (shared corpus)", () => {
  assert.ok(CORPUS.shapes.length >= 17);
  for (const { kind, prefix, body } of CORPUS.shapes) {
    const value = prefix + body;
    for (const template of ["{}", "here is my key: {} thanks", "export FOO={}\n", '("{}")', "anahtarım {} bu"]) {
      const text = template.replace("{}", () => value);
      const found = detectSecrets(text);
      assert.equal(found.length, 1, `${kind} in ${JSON.stringify(template)}: ${JSON.stringify(found)}`);
      assert.equal(found[0].kind, kind, template);
      assert.equal(text.slice(found[0].start, found[0].end), value, `${kind} span in ${JSON.stringify(template)}`);
    }
  }
});

test("labelled values are found with the kind and exact span (shared corpus)", () => {
  assert.ok(CORPUS.labelled.length >= 20);
  for (const { text, value, kind } of CORPUS.labelled) {
    const found = detectSecrets(text);
    assert.equal(found.length, 1, text);
    assert.equal(found[0].kind, kind, text);
    assert.equal(text.slice(found[0].start, found[0].end), value, text);
  }
});

test("card numbers are found with the exact span (shared corpus)", () => {
  assert.ok(CORPUS.cards.length >= 20);
  for (const { text, value } of CORPUS.cards) {
    const found = detectSecrets(text);
    assert.equal(found.length, 1, text);
    assert.equal(found[0].kind, "payment_card", text);
    assert.equal(text.slice(found[0].start, found[0].end), value, text);
  }
});

test("the negative corpus has zero false positives, bare and inside a longer unicode message", () => {
  assert.ok(CORPUS.negatives.length >= 90);
  for (const text of CORPUS.negatives) {
    assert.deepEqual(detectSecrets(text), [], text);
    const wrapped = `Şu mesajı gönder 🙂 «${text}» çok teşekkürler\nsonra devam`;
    assert.deepEqual(detectSecrets(wrapped), [], wrapped);
  }
});

test("spans index the original string when emoji and Turkish letters come first", () => {
  const text = "🙂 şifre: Tr0ub4dor&3 ve 🙂 kart 4111 1111 1111 1111";
  const found = detectSecrets(text);
  assert.deepEqual(found.map((c) => text.slice(c.start, c.end)), ["Tr0ub4dor&3", "4111 1111 1111 1111"]);
});

test("a known shape wins over a label on the same bytes, several secrets come back in order", () => {
  const text = `api key: ${CANARY} and sifre: Tr0ub4dor&3`;
  const found = detectSecrets(text);
  assert.deepEqual(found.map((c) => c.kind), ["openai_style_key", "labelled_password"]);
});

test("storing a labelled password keeps the label and swaps only the value", () => {
  const text = "my password is Tr0ub4dor&3, thanks";
  const plan = planStore(text, detectSecrets(text), "pasted/pw");
  assert.equal(plan.text, "my password is [secret: pasted/pw], thanks");
  assert.deepEqual(plan.entries, [{ name: "pasted/pw", value: "Tr0ub4dor&3" }]);
  assert.deepEqual(detectSecrets(plan.text), []);
});

test("huge input stays fast", () => {
  const big = "ahahahah token: ".repeat(50_000);
  const t0 = Date.now();
  assert.deepEqual(detectSecrets(big), []);
  assert.ok(Date.now() - t0 < 5000);
});

test("long pathological inputs from the shared corpus finish fast with the expected count", () => {
  assert.ok(CORPUS.stress.length >= 10);
  for (const { name, unit, times, found } of CORPUS.stress) {
    const text = unit.repeat(times);
    assert.ok(text.length > 100_000, `${name} is a long input`);
    const t0 = Date.now();
    const got = detectSecrets(text);
    // A quadratic scan needs minutes on these; a linear one well under a second.
    assert.ok(Date.now() - t0 < 5000, `${name} took ${Date.now() - t0} ms`);
    if (found !== undefined) assert.equal(got.length, found, name);
  }
});

// The window edge (`SCAN_WINDOW` = 64 KiB of UTF-16 units) must be invisible: same answer wherever a secret sits.
const WINDOW = 64 * 1024;

test("a secret across a scan-window edge is found whole, with exact offsets", () => {
  for (const lead of [WINDOW - 9, WINDOW - 1, WINDOW, WINDOW + 5, 3 * WINDOW - 7]) {
    const text = `é${"ab ".repeat(Math.floor(lead / 3))}${CANARY} end`;
    const found = detectSecrets(text);
    assert.equal(found.length, 1, `lead ${lead}`);
    assert.equal(text.slice(found[0].start, found[0].end), CANARY, `lead ${lead}`);
  }
  // The edge falls between the two halves of an emoji (a surrogate pair): nothing is split or lost.
  for (const pad of ["", "x"]) {
    const text = `${pad}${"🙂".repeat(WINDOW / 2 + 3)}x ${GH} y`;
    const found = detectSecrets(text);
    assert.equal(found.length, 1);
    assert.equal(text.slice(found[0].start, found[0].end), GH);
  }
});

test("a label before a window edge still labels its value after it", () => {
  const text = `${"ab ".repeat(WINDOW / 3 - 3)}password: Tr0ub4dor&3 x`;
  const found = detectSecrets(text);
  assert.deepEqual(found.map((c) => text.slice(c.start, c.end)), ["Tr0ub4dor&3"]);
});

test("secrets deep inside and at the end of a long text are all found", () => {
  const words = "alpha beta gamma ".repeat(30_000);
  const text = `${words}${CANARY} ${words}password: Tr0ub4dor&3 and ${GH}`;
  const found = detectSecrets(text);
  assert.deepEqual(found.map((c) => c.kind), ["openai_style_key", "labelled_password", "github_token"]);
  assert.equal(removeSecrets(text, found).length, text.length - CANARY.length - "Tr0ub4dor&3".length - GH.length);
});

test("a card number glued to a long prefix still needs a card word within the last few", () => {
  const prefix = "x ".repeat(500_000);
  assert.deepEqual(detectSecrets(`${prefix}card 4111111111111111`).map((c) => c.kind), ["payment_card"]);
  assert.deepEqual(detectSecrets(`${prefix}4111111111111111`), []);
  // The word is just past the 32 code points that count: not a label any more.
  assert.deepEqual(detectSecrets(`card ${"é".repeat(40)} 4111111111111111`), []);
  assert.deepEqual(detectSecrets(`🙂 kart 🙂 4111111111111111`).map((c) => c.kind), ["payment_card"]);
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
  assert.ok(/setSecretKinds\(found\)/.test(chat));
  const card = SRC("../components/SecretRequestCard.tsx");
  // The value inputs are uncontrolled: no `value=` prop and no state setter receives them.
  assert.ok(!/<input[^>]*\bvalue=/.test(card));
  assert.ok(!/useState<[^>]*(password|secret|value)/i.test(card));
  assert.ok(/type="password"/.test(card) && /autoComplete: "off"/.test(card));
});

// ---- "Send anyway" is a single-use allowance (review of #495) ----

test("Send anyway allows one call; the next draft is checked again even if the first call returned early", () => {
  const first = `key ${CANARY}`;
  const second = `other ${GH}`;
  assert.deepEqual(kindsToWarn(first, true), []);
  // The first call bailed out early (busy): nothing carried over, so a changed draft still blocks.
  assert.deepEqual(kindsToWarn(second, false), ["github_token"]);
  assert.deepEqual(kindsToWarn(first, false), ["openai_style_key"]);
});

test("ChatView keeps no standing send-anyway flag, checks slash text and voice, and guards onStore", () => {
  const chat = SRC("../views/ChatView.tsx");
  assert.ok(!/sendAnyway/.test(chat));
  assert.ok(/const send = async \(allowSecrets = false\)/.test(chat));
  assert.ok(/void send\(true\)/.test(chat));
  // allowSecrets is read only in the gate, never assigned to a ref or state.
  assert.ok(!/allowSecrets\s*=[^=]|allowSecrets\.current|setAllowSecrets/.test(chat.replace("allowSecrets = false", "")));
  // Slash text reaches the bot and the history: no startsWith("/") exemption in front of the gate.
  assert.ok(!/startsWith\("\/"\)\s*&&\s*!allowSecrets/.test(chat));
  // Voice text is checked, and "store" never overwrites an edited draft.
  assert.ok(/sendText: async \(spoken\) => \{\s*[^}]*detectSecrets\(spoken\)/.test(chat));
  assert.ok(/draftRef\.current === analysed/.test(chat));
});

test("the secret card refuses to wipe or claim success without a connection, and shows no host sentence", () => {
  const dec = SRC("../components/PendingDecisions.tsx");
  // Two secret-card, three hand-over (CAPTCHA / code) and two MCP form handlers share the guard.
  assert.equal((dec.match(/throw new NoClientError\(\)/g) ?? []).length, 7);
  const card = SRC("../components/SecretRequestCard.tsx");
  assert.ok(/secretRequest\.hostRefused/.test(card) && /secretRequest\.notConnected/.test(card));
});

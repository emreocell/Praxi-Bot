import assert from "node:assert/strict";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import type { OpenApiOperation } from "../api/types.ts";
import { authFromForm, enabledSet, filterOperations, isOn, toggleOperation, validSpecUrl } from "./openapiApps.ts";

const op = (name: string, enabled: boolean, bots: string[] | null = null, extra: Partial<OpenApiOperation> = {}): OpenApiOperation => ({
  name,
  method: "GET",
  path: `/${name}`,
  summary: `Summary of ${name}`,
  tags: [],
  effect: "read",
  always_ask: false,
  enabled,
  bots,
  ...extra,
});
const BOTS = ["a", "b", "c"];

test("isOn: an operation open to every bot is on for each bot, a list only for its members", () => {
  assert.equal(isOn(op("x", false), { kind: "all" }), false);
  assert.equal(isOn(op("x", true, null), { kind: "all" }), true);
  assert.equal(isOn(op("x", true, null), { kind: "bot", id: "a" }), true);
  assert.equal(isOn(op("x", true, ["a"]), { kind: "all" }), false);
  assert.equal(isOn(op("x", true, ["a"]), { kind: "bot", id: "a" }), true);
  assert.equal(isOn(op("x", true, ["a"]), { kind: "bot", id: "b" }), false);
});

test("turning an operation on for one bot starts a list; for all bots it opens it to everyone", () => {
  const ops = [op("x", false), op("y", true, ["a"])];
  assert.deepEqual(toggleOperation(ops, "x", { kind: "bot", id: "b" }, true, BOTS), [
    { name: "x", bots: ["b"] },
    { name: "y", bots: ["a"] },
  ]);
  assert.deepEqual(toggleOperation(ops, "y", { kind: "bot", id: "b" }, true, BOTS), [{ name: "y", bots: ["a", "b"] }]);
  assert.deepEqual(toggleOperation(ops, "x", { kind: "all" }, true, BOTS), [
    { name: "x", bots: null },
    { name: "y", bots: ["a"] },
  ]);
});

test("turning one bot off keeps the others, and never widens to everyone", () => {
  const ops = [op("x", true, ["a", "b"]), op("y", true, null), op("z", true, ["a"])];
  assert.deepEqual(toggleOperation(ops, "x", { kind: "bot", id: "a" }, false, BOTS), [
    { name: "x", bots: ["b"] },
    { name: "y", bots: null },
    { name: "z", bots: ["a"] },
  ]);
  // Open to every bot: turning one off lists all the others.
  assert.deepEqual(toggleOperation(ops, "y", { kind: "bot", id: "b" }, false, BOTS).find((e) => e.name === "y"), { name: "y", bots: ["a", "c"] });
  // The last bot: the operation is switched off, not left on with an empty list or opened to all.
  assert.deepEqual(toggleOperation(ops, "z", { kind: "bot", id: "a" }, false, BOTS).map((e) => e.name), ["x", "y"]);
  // "All bots" off switches it off for everyone.
  assert.deepEqual(toggleOperation(ops, "y", { kind: "all" }, false, BOTS).map((e) => e.name), ["x", "z"]);
});

test("an operation already open to every bot stays open when one bot is turned on", () => {
  const ops = [op("y", true, null)];
  assert.deepEqual(toggleOperation(ops, "y", { kind: "bot", id: "a" }, true, BOTS), [{ name: "y", bots: null }]);
});

test("enabledSet lists only enabled operations in order", () => {
  assert.deepEqual(enabledSet([op("a", true, ["x"]), op("b", false), op("c", true)]), [
    { name: "a", bots: ["x"] },
    { name: "c", bots: null },
  ]);
});

test("filterOperations searches name, path, summary and tags", () => {
  const ops = [op("listPets", false, null, { tags: ["pets"] }), op("createInvoice", false, null, { path: "/billing/invoices" })];
  assert.equal(filterOperations(ops, "").length, 2);
  assert.deepEqual(filterOperations(ops, "PETS").map((o) => o.name), ["listPets"]);
  assert.deepEqual(filterOperations(ops, "billing").map((o) => o.name), ["createInvoice"]);
  assert.equal(filterOperations(ops, "nothing").length, 0);
});

test("authFromForm checks what each kind needs and sends only what applies", () => {
  const f = { kind: "none" as const, name: "", username: "", secret: "" };
  assert.deepEqual(authFromForm(f), { ok: true, auth: { kind: "none" } });
  assert.deepEqual(authFromForm({ ...f, kind: "bearer" }), { ok: false, error: "secret" });
  assert.deepEqual(authFromForm({ ...f, kind: "bearer", secret: "t", name: "ignored" }), { ok: true, auth: { kind: "bearer", secret: "t" } });
  assert.deepEqual(authFromForm({ ...f, kind: "api_key_header", secret: "k" }), { ok: false, error: "name" });
  assert.deepEqual(authFromForm({ ...f, kind: "api_key_query", name: " api_key ", secret: "k" }), {
    ok: true,
    auth: { kind: "api_key_query", name: "api_key", secret: "k" },
  });
  assert.deepEqual(authFromForm({ ...f, kind: "basic", secret: "p" }), { ok: false, error: "username" });
  assert.deepEqual(authFromForm({ ...f, kind: "basic", username: "me", secret: "p" }), { ok: true, auth: { kind: "basic", username: "me", secret: "p" } });
  // An app that already has a secret may leave it empty (the daemon keeps it).
  assert.deepEqual(authFromForm({ ...f, kind: "bearer" }, true), { ok: true, auth: { kind: "bearer" } });
});

test("validSpecUrl accepts http(s) addresses only", () => {
  assert.equal(validSpecUrl("https://api.example.com/openapi.json"), true);
  assert.equal(validSpecUrl(" http://x.test/a "), true);
  for (const bad of ["", "ftp://x.test/a", "file:///etc/passwd", "api.example.com", "javascript:alert(1)"]) assert.equal(validSpecUrl(bad), false, bad);
});

test("the mock daemon connects an API with every operation off, then applies a bot's selection", async () => {
  const client = new MockApiClient();
  assert.deepEqual(await client.openapiApps(), []);
  const app = await client.openapiAddApp({ name: "Demo Shop", spec_url: "https://shop.example.test/openapi.json", auth: { kind: "bearer", secret: "s" } });
  assert.equal(app.id, "demo_shop");
  assert.equal(app.enabled_count, 0);
  assert.ok(app.operations.every((o) => !o.enabled));
  assert.ok(app.operations.some((o) => o.always_ask));
  assert.equal(app.auth.secret_set, true);
  assert.ok(!JSON.stringify(app).includes('"secret":'));
  const next = toggleOperation(app.operations, app.operations[0].name, { kind: "bot", id: "bot-1" }, true, ["bot-1"]);
  const updated = await client.openapiUpdateApp(app.id, { operations: next });
  assert.equal(updated.enabled_count, 1);
  assert.deepEqual(updated.operations[0].bots, ["bot-1"]);
  await client.openapiRemoveApp(app.id);
  assert.deepEqual(await client.openapiApps(), []);
});

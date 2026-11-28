import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { addressIsValid, checkoutFacts, pinIsValid, setupProblem } from "./phoneApproval.ts";

test("a PIN is 6 to 12 digits", () => {
  assert.ok(pinIsValid("123456") && pinIsValid("123456789012"));
  for (const bad of ["12345", "1234567890123", "12345a", " 123456", ""]) {
    assert.equal(pinIsValid(bad), false, bad);
  }
});

test("the address is an https origin, http only on this computer", () => {
  assert.ok(addressIsValid("https://pay.example.org"));
  assert.ok(addressIsValid("https://pay.example.org:8443/"));
  assert.ok(addressIsValid("http://127.0.0.1:8080"));
  for (const bad of [
    "http://pay.example.org",
    "https://pay.example.org/sub",
    "https://u:p@pay.example.org",
    "https://pay.example.org/?x=1",
    "pay.example.org",
    "javascript:alert(1)"
  ]) {
    assert.equal(addressIsValid(bad), false, bad);
  }
});

test("the first setup needs a PIN typed twice; later an empty PIN keeps the old one", () => {
  const a = "https://pay.example.org";
  assert.equal(setupProblem(a, "", "", false), "phoneApproval.badPin");
  assert.equal(setupProblem(a, "", "", true), null);
  assert.equal(setupProblem(a, "246810", "246811", false), "phoneApproval.pinMismatch");
  assert.equal(setupProblem(a, "246810", "246810", false), null);
  assert.equal(setupProblem("http://x.org", "246810", "246810", true), "phoneApproval.badAddress");
});

test("the settings card never shows or keeps the PIN after saving", () => {
  const src = readFileSync(new URL("../views/PhoneApprovalSettings.tsx", import.meta.url), "utf8");
  assert.match(src, /type="password"/);
  assert.match(src, /autoComplete="new-password"/);
  // After a save or a failed save the typed PIN is cleared.
  assert.match(src, /setPin\(""\);\s*setRepeat\(""\);/);
  // Every daemon call goes through the client; the PIN is never logged.
  assert.doesNotMatch(src, /console\./);
});

test("a money card's checkout facts are read defensively", () => {
  assert.deepEqual(
    checkoutFacts({ money_movement: true, checkout: { domain: "shop.example", merchant: "Shop", amount: "€12" } }),
    { domain: "shop.example", merchant: "Shop", amount: "€12" }
  );
  assert.deepEqual(checkoutFacts({ money_movement: true, checkout: { domain: "shop.example", amount: 5 } }), {
    domain: "shop.example",
    merchant: null,
    amount: null
  });
  assert.equal(checkoutFacts({ checkout: { domain: "shop.example" } }), null, "not a money card");
  assert.equal(checkoutFacts({ money_movement: true, checkout: {} }), null);
  assert.equal(checkoutFacts(null), null);
  assert.equal(checkoutFacts({ money_movement: true, checkout: { merchant: "x".repeat(500) } })?.merchant?.length, 120);
});

import assert from "node:assert/strict";
import test from "node:test";
import { freshId, knownState, meterFraction, meterTone, providerId, shortCount } from "./embeddings.ts";

test("usage bars are empty without a published limit and never overflow", () => {
  assert.equal(meterFraction(5, null), 0);
  assert.equal(meterFraction(5, 0), 0);
  assert.equal(meterFraction(50, 100), 0.5);
  assert.equal(meterFraction(500, 100), 1);
  assert.equal(meterTone(0.5), "ok");
  assert.equal(meterTone(0.85), "warn");
  assert.equal(meterTone(1), "full");
});

test("unknown daemon states are shown as a rejected request, not as a blank", () => {
  assert.equal(knownState("rate_limited"), "rate_limited");
  assert.equal(knownState("from_the_future"), "rejected");
});

test("counts and ids are short and safe", () => {
  assert.equal(shortCount(250), "250");
  assert.equal(shortCount(30000), "30K");
  assert.equal(shortCount(1500), "1.5K");
  assert.equal(providerId("My Ollama (home)!"), "my-ollama-home");
  assert.equal(providerId("!!!"), "provider");
  assert.equal(freshId("gemini", ["gemini", "gemini-2"]), "gemini-3");
});

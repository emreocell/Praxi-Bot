import assert from "node:assert/strict";
import { test } from "node:test";
import { appInert, createInertTracker } from "./inertStack.ts";

test("inert is on while at least one token is held, whatever the release order", () => {
  const calls: boolean[] = [];
  const t = createInertTracker((v) => calls.push(v));
  t.acquire(1);
  t.acquire(2);
  assert.equal(t.holders, 2);
  t.release(1);
  assert.equal(t.holders, 1);
  assert.equal(calls.at(-1), true);
  t.release(2);
  assert.equal(t.holders, 0);
  assert.equal(calls.at(-1), false);
});

test("acquiring a token twice counts once, and releasing an unknown token is harmless", () => {
  const calls: boolean[] = [];
  const t = createInertTracker((v) => calls.push(v));
  t.acquire(1);
  t.acquire(1);
  assert.equal(t.holders, 1);
  t.release(99);
  assert.equal(t.holders, 1);
  assert.equal(calls.at(-1), true);
  t.release(1);
  assert.equal(calls.at(-1), false);
});

test("the shared tracker is a no-op without a document", () => {
  assert.doesNotThrow(() => {
    appInert.acquire(1);
    appInert.release(1);
  });
  assert.equal(appInert.holders, 0);
});

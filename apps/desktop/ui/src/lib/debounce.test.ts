import assert from "node:assert/strict";
import { mock, test } from "node:test";
import { createDebounced } from "./debounce.ts";

test("only the last call of a burst runs, after the delay", () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  const seen: number[] = [];
  const d = createDebounced((n: number) => seen.push(n), 250);
  d(1);
  mock.timers.tick(200);
  d(2);
  mock.timers.tick(200);
  d(3);
  assert.deepEqual(seen, []);
  mock.timers.tick(250);
  assert.deepEqual(seen, [3]);
  mock.timers.reset();
});

test("cancel drops a pending call; flush runs it immediately and only once", () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  const seen: string[] = [];
  const d = createDebounced((s: string) => seen.push(s), 250);
  d("a");
  d.cancel();
  mock.timers.tick(500);
  assert.deepEqual(seen, []);
  d("b");
  d.flush();
  assert.deepEqual(seen, ["b"]);
  mock.timers.tick(500);
  d.flush();
  assert.deepEqual(seen, ["b"]);
  mock.timers.reset();
});

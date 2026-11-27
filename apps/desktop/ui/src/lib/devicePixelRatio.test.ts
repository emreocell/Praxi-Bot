import assert from "node:assert/strict";
import { test } from "node:test";
import { currentRatio, watchDevicePixelRatio, type RatioWindow } from "./devicePixelRatio.ts";

/** A window whose ratio can change; `matchMedia` fires `change` once when its query stops matching. */
function fakeWindow(start: number) {
  const win: RatioWindow & { listeners: Map<string, Set<() => void>>; queries: string[] } = {
    devicePixelRatio: start,
    listeners: new Map(),
    queries: [],
    matchMedia(query: string) {
      win.queries.push(query);
      return {
        addEventListener: (_type, fn) => {
          const set = win.listeners.get(query) ?? new Set();
          set.add(fn);
          win.listeners.set(query, set);
        },
        removeEventListener: (_type, fn) => win.listeners.get(query)?.delete(fn)
      };
    }
  };
  const setRatio = (next: number) => {
    const query = `(resolution: ${win.devicePixelRatio}dppx)`;
    win.devicePixelRatio = next;
    for (const fn of [...(win.listeners.get(query) ?? [])]) fn();
  };
  return { win, setRatio };
}

test("the ratio follows moves between screens of different scale and re-arms each time", () => {
  const { win, setRatio } = fakeWindow(1);
  const seen: number[] = [];
  const stop = watchDevicePixelRatio(win, (r) => seen.push(r));
  setRatio(1.5);
  setRatio(2);
  setRatio(1);
  assert.deepEqual(seen, [1.5, 2, 1]);
  assert.deepEqual(win.queries, [
    "(resolution: 1dppx)",
    "(resolution: 1.5dppx)",
    "(resolution: 2dppx)",
    "(resolution: 1dppx)"
  ]);
  stop();
});

test("stopping removes the listener, so a later change is not reported", () => {
  const { win, setRatio } = fakeWindow(2);
  const seen: number[] = [];
  const stop = watchDevicePixelRatio(win, (r) => seen.push(r));
  stop();
  setRatio(1);
  assert.deepEqual(seen, []);
  assert.equal(win.listeners.get("(resolution: 2dppx)")?.size, 0);
});

test("a window without matchMedia is never watched and a bad ratio counts as 1", () => {
  const seen: number[] = [];
  const stop = watchDevicePixelRatio({ devicePixelRatio: 2 }, (r) => seen.push(r));
  stop();
  assert.deepEqual(seen, []);
  assert.equal(currentRatio({ devicePixelRatio: 0 }), 1);
  assert.equal(currentRatio({ devicePixelRatio: Number.NaN }), 1);
  assert.equal(currentRatio({ devicePixelRatio: 1.25 }), 1.25);
});

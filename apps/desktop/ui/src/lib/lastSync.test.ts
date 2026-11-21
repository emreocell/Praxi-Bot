import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";
import { lastSyncParts, readLastSync, writeLastSync } from "./lastSync.ts";

let previousWindow: PropertyDescriptor | undefined;
let store: Map<string, string>;

function installStorage(localStorage: Pick<Storage, "getItem" | "setItem">) {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage }
  });
}

beforeEach(() => {
  previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  store = new Map<string, string>();
  installStorage({
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => { store.set(key, value); }
  });
});

afterEach(() => {
  if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

test("writeLastSync round-trips milliseconds and keeps servers independent", () => {
  const ms = new Date(2026, 0, 15, 14, 30).getTime();
  writeLastSync("server-a", ms);
  writeLastSync("server-b", ms + 1000);
  assert.equal(store.get("praxi.lastSync.server-a"), String(ms));
  assert.equal(readLastSync("server-a"), ms);
  assert.equal(readLastSync("server-b"), ms + 1000);
  assert.equal(readLastSync("server-c"), null);

  writeLastSync("server-a", ms + 2000);
  assert.equal(readLastSync("server-a"), ms + 2000);
  assert.equal(readLastSync("server-b"), ms + 1000);
});

test("readLastSync returns null for a missing value", () => {
  assert.equal(readLastSync("server-a"), null);
});

for (const value of ["", " ", "not-a-number", "123ms", "NaN", "Infinity", "-Infinity", "0", "-1"]) {
  test(`readLastSync returns null for ${JSON.stringify(value)}`, () => {
    store.set("praxi.lastSync.server-a", value);
    assert.equal(readLastSync("server-a"), null);
  });
}

test("readLastSync returns null when getItem throws", () => {
  installStorage({
    getItem: () => { throw new Error("Storage is blocked"); },
    setItem: () => {}
  });
  assert.equal(readLastSync("server-a"), null);
});

test("writeLastSync does not throw when setItem throws", () => {
  let attempted = false;
  installStorage({
    getItem: () => null,
    setItem: () => {
      attempted = true;
      throw new Error("Storage quota exceeded");
    }
  });
  assert.doesNotThrow(() => writeLastSync("server-a", 1234));
  assert.equal(attempted, true);
});

test("storage helpers tolerate a missing window", () => {
  Reflect.deleteProperty(globalThis, "window");
  assert.equal(readLastSync("server-a"), null);
  assert.doesNotThrow(() => writeLastSync("server-a", 1234));
});

test("storage helpers tolerate a throwing localStorage getter", () => {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      get localStorage() { throw new Error("Storage access denied"); }
    }
  });
  assert.equal(readLastSync("server-a"), null);
  assert.doesNotThrow(() => writeLastSync("server-a", 1234));
});

for (const lang of ["tr", "en"]) {
  test(`lastSyncParts hides the date on the same local calendar day (${lang})`, () => {
    const synced = new Date(2026, 0, 15, 0, 5);
    const now = new Date(2026, 0, 15, 23, 55);
    const parts = lastSyncParts(synced.getTime(), now.getTime(), lang);
    assert.equal(parts.day, null);
    assert.equal(parts.time, lang === "tr" ? "00:05" : "12:05 AM");
  });

  test(`lastSyncParts shows the date across local midnight (${lang})`, () => {
    const synced = new Date(2026, 0, 15, 23, 55);
    const now = new Date(2026, 0, 16, 0, 5);
    const parts = lastSyncParts(synced.getTime(), now.getTime(), lang);
    assert.equal(parts.day, lang === "tr" ? "15 Oca" : "Jan 15");
    assert.equal(parts.time, lang === "tr" ? "23:55" : "11:55 PM");
  });

  test(`lastSyncParts distinguishes matching day numbers in other months and years (${lang})`, () => {
    const synced = new Date(2026, 0, 15, 14, 30);
    for (const now of [new Date(2026, 1, 15, 14, 30), new Date(2027, 0, 15, 14, 30)]) {
      const parts = lastSyncParts(synced.getTime(), now.getTime(), lang);
      assert.equal(parts.day, lang === "tr" ? "15 Oca" : "Jan 15");
      assert.equal(parts.time, lang === "tr" ? "14:30" : "02:30 PM");
    }
  });
}

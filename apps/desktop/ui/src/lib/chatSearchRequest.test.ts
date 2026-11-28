import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import {
  requestSearchAllChats,
  SEARCH_ALL_CHATS_EVENT,
  searchPhrase,
  takeSearchRequest
} from "./chatSearchRequest.ts";

beforeEach(() => {
  const listeners = new Map<string, Set<() => void>>();
  (globalThis as unknown as { window: unknown }).window = {
    dispatchEvent: (e: Event) => {
      for (const fn of listeners.get(e.type) ?? []) fn();
      return true;
    },
    addEventListener: (type: string, fn: () => void) => {
      let set = listeners.get(type);
      if (!set) {
        set = new Set();
        listeners.set(type, set);
      }
      set.add(fn);
    },
    removeEventListener: (type: string, fn: () => void) => {
      listeners.get(type)?.delete(fn);
    }
  };
});

test("searchPhrase normalises whitespace and caps length", () => {
  assert.equal(searchPhrase("  a\n  b\tc "), "a b c");
  assert.equal(searchPhrase("x".repeat(500)).length, 200);
  assert.equal(searchPhrase("   "), "");
});

test("requestSearchAllChats queues one request and fires the event", () => {
  let fired = 0;
  window.addEventListener(SEARCH_ALL_CHATS_EVENT, () => {
    fired += 1;
  });
  requestSearchAllChats("  find invoices  ");
  assert.equal(fired, 1);
  assert.deepEqual(takeSearchRequest(), { query: "find invoices", n: 1 });
  assert.equal(takeSearchRequest(), null);
  requestSearchAllChats("");
  assert.equal(takeSearchRequest(), null);
});
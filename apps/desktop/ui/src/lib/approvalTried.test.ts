import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import {
  approvalDecided,
  markApprovalDecided,
  onApprovalDecided
} from "./approvalTried.ts";

const KEY = "praxi.approvalDecided";
const EVENT = "praxi:approval-decided";

beforeEach(() => {
  const mem = new Map<string, string>();
  const listeners = new Map<string, Set<() => void>>();
  (globalThis as unknown as { window: unknown }).window = {
    localStorage: {
      getItem: (k: string) => mem.get(k) ?? null,
      setItem: (k: string, v: string) => void mem.set(k, v)
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
    },
    dispatchEvent: (e: Event) => {
      for (const fn of listeners.get(e.type) ?? []) fn();
      return true;
    }
  };
});

test("approvalDecided reads the local checklist mark", () => {
  assert.equal(approvalDecided(), false);
  window.localStorage.setItem(KEY, "1");
  assert.equal(approvalDecided(), true);
});

test("markApprovalDecided persists the mark and notifies listeners", () => {
  let heard = 0;
  const off = onApprovalDecided(() => {
    heard += 1;
  });
  markApprovalDecided();
  assert.equal(approvalDecided(), true);
  assert.equal(heard, 1);
  off();
  markApprovalDecided();
  assert.equal(heard, 1);
});

test("storage failures are tolerated", () => {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    get() {
      throw new Error("blocked");
    }
  });
  assert.equal(approvalDecided(), false);
  assert.doesNotThrow(() => markApprovalDecided());
  let fired = false;
  const off = onApprovalDecided(() => {
    fired = true;
  });
  window.dispatchEvent(new Event(EVENT));
  assert.equal(fired, true);
  off();
});
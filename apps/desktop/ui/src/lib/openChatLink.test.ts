import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import { OPEN_CHAT_EVENT, requestOpenChat, type OpenChatTarget } from "./openChatLink.ts";

beforeEach(() => {
  const listeners = new Map<string, Set<(e: Event) => void>>();
  (globalThis as unknown as { window: unknown }).window = {
    dispatchEvent(e: CustomEvent) {
      for (const fn of listeners.get(e.type) ?? []) fn(e);
      return true;
    },
    addEventListener(type: string, fn: (e: Event) => void) {
      let set = listeners.get(type);
      if (!set) {
        set = new Set();
        listeners.set(type, set);
      }
      set.add(fn);
    },
    removeEventListener(type: string, fn: (e: Event) => void) {
      listeners.get(type)?.delete(fn);
    }
  };
});

test("requestOpenChat dispatches a detail-bearing custom event", () => {
  let seen: OpenChatTarget | null = null;
  const handler = (e: Event) => {
    seen = (e as CustomEvent<OpenChatTarget>).detail;
  };
  window.addEventListener(OPEN_CHAT_EVENT, handler);
  requestOpenChat("chat-abc");
  window.removeEventListener(OPEN_CHAT_EVENT, handler);
  assert.deepEqual(seen, { chatId: "chat-abc" });
});
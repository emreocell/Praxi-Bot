import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { shieldFromCapture } from "./captureShield.ts";

const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

afterEach(() => {
  if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

test("shieldFromCapture is a safe no-op outside the Tauri desktop shell", () => {
  Reflect.deleteProperty(globalThis, "window");
  const release = shieldFromCapture();
  release();
  release();
});

test("shieldFromCapture reference-counts overlapping shields and ignores duplicate releases", () => {
  const calls: Array<{ cmd: string; value: boolean }> = [];
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      __TAURI_INTERNALS__: {
        metadata: { currentWindow: { label: "main" } },
        invoke: async (cmd: string, args: { value?: boolean }) => {
          calls.push({ cmd, value: Boolean(args?.value) });
        }
      }
    }
  });

  const releaseFirst = shieldFromCapture();
  const releaseSecond = shieldFromCapture();
  assert.deepEqual(calls, [{ cmd: "plugin:window|set_content_protected", value: true }]);

  // Releasing one (and repeating it) keeps capture protection on until the last closes.
  releaseFirst();
  releaseFirst();
  assert.equal(calls.length, 1);

  releaseSecond();
  releaseSecond();
  assert.deepEqual(calls, [
    { cmd: "plugin:window|set_content_protected", value: true },
    { cmd: "plugin:window|set_content_protected", value: false }
  ]);
});

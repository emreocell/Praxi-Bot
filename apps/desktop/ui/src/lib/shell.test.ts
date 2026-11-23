import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, afterEach, before, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";
import type { NativeNotification } from "./shell.ts";

let server: ViteDevServer;
let shell: typeof import("./shell.ts");

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null },
    ssr: { noExternal: ["ui"] }
  });
  shell = (await server.ssrLoadModule("/src/lib/shell.ts")) as typeof import("./shell.ts");
});

after(async () => {
  await server?.close();
});

const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");

function installWindow(value: Record<string, unknown> = {}) {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value
  });
}

function installDocument(hasFocus: () => boolean) {
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: { hasFocus }
  });
}

afterEach(() => {
  if (previousWindow) {
    Object.defineProperty(globalThis, "window", previousWindow);
  } else {
    Reflect.deleteProperty(globalThis, "window");
  }
  if (previousDocument) {
    Object.defineProperty(globalThis, "document", previousDocument);
  } else {
    Reflect.deleteProperty(globalThis, "document");
  }
});

test("showNotification records notifications in order on window.__praxiNotifications", async () => {
  const fakeWindow: { __praxiNotifications?: NativeNotification[] } = {};
  installWindow(fakeWindow);

  const first: NativeNotification = { title: "First", body: "One" };
  const second: NativeNotification = { title: "Second", body: "Two", chatId: "chat-1" };

  await shell.showNotification(first);
  await shell.showNotification(second);

  assert.deepEqual(fakeWindow.__praxiNotifications, [first, second]);
});

test("isAppFocused returns document.hasFocus() and false when document is missing", async () => {
  installWindow();

  installDocument(() => true);
  assert.equal(await shell.isAppFocused(), true);

  installDocument(() => false);
  assert.equal(await shell.isAppFocused(), false);

  Reflect.deleteProperty(globalThis, "document");
  assert.equal(await shell.isAppFocused(), false);
});

test("setNeedsCount and setTrayLabels resolve without error in browser fallback", async () => {
  installWindow();

  await shell.setNeedsCount(3);
  await shell.setTrayLabels({
    toggle: "Toggle",
    needs: "Needs",
    pauseAll: "Pause all",
    stopAll: "Stop All",
    quit: "Quit",
    tooltip: "Praxi",
    tooltipNeeds: "Praxi (needs)",
    stillRunningTitle: "Still running",
    stillRunningBody: "In the tray"
  });
  await shell.setKeepInTray(true, false);
});

test("onShellEvent returns a no-op unsubscribe function and never invokes handler", () => {
  installWindow();

  let called = false;
  const unsubscribe = shell.onShellEvent("shell:tray_open_needs", () => {
    called = true;
  });

  assert.equal(typeof unsubscribe, "function");
  unsubscribe();
  assert.equal(called, false);
});

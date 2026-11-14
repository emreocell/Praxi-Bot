import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { getCurrentWindow, type Window } from "@tauri-apps/api/window";

interface Capability {
  identifier: string;
  windows?: string[];
  webviews?: string[];
  local?: boolean;
  remote?: unknown;
  permissions: (string | { identifier: string })[];
}

const config = JSON.parse(readFileSync(new URL("../../../tauri.conf.json", import.meta.url), "utf8")) as {
  app: { security: { capabilities: string[] } };
};
const directory = new URL("../../../capabilities/", import.meta.url);
const capabilities = readdirSync(directory)
  .filter((name) => name.endsWith(".json"))
  .map((name) => JSON.parse(readFileSync(new URL(name, directory), "utf8")) as Capability)
  .filter((capability) => config.app.security.capabilities.includes(capability.identifier));
const identifiers = (capability: Capability) => capability.permissions.map(
  (permission) => typeof permission === "string" ? permission : permission.identifier
);

// These methods explicitly require individual grants in @tauri-apps/api/window;
// core:window:default only grants read operations and internal-toggle-maximize.
const actions: [string, (window: Window) => Promise<void>][] = [
  ["set-decorations", (window) => window.setDecorations(false)],
  ["minimize", (window) => window.minimize()],
  ["toggle-maximize", (window) => window.toggleMaximize()],
  ["close", (window) => window.close()]
];

for (const [action, perform] of actions) {
  test(`the main window can invoke the ${action} command used by its chrome`, async () => {
    const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
    Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
    try {
      mockWindows("main");
      mockIPC((command, payload) => {
        assert.equal(command, `plugin:window|${action.replaceAll("-", "_")}`);
        assert.equal((payload as { label: string }).label, "main");
        assert.ok(capabilities.some((capability) =>
          capability.windows?.includes("main") &&
          identifiers(capability).includes(`core:window:allow-${action}`)
        ), `${command} is not enabled by the shipped capabilities`);
      });
      await perform(getCurrentWindow());
    } finally {
      clearMocks();
      if (previous) Object.defineProperty(globalThis, "window", previous);
      else Reflect.deleteProperty(globalThis, "window");
    }
  });
}

test("window chrome grants stay local to the main window", () => {
  const grants = new Set(actions.map(([action]) => `core:window:allow-${action}`));
  for (const capability of capabilities.filter((c) => identifiers(c).some((p) => grants.has(p)))) {
    assert.deepEqual(capability.windows, ["main"]);
    assert.equal(capability.webviews, undefined);
    assert.equal(capability.remote, undefined);
    assert.notEqual(capability.local, false);
  }
});

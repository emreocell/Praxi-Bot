import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";
import type { ComputerTarget } from "./settingsPrefs.ts";
import type { AppSettings } from "../store/settingsStore.ts";

let server: ViteDevServer;
let ComputerTargetBlock: ComponentType<{ value: ComputerTarget; onChange: (value: ComputerTarget) => void }>;
let ModelPicker: ComponentType<{ groups: []; value: string; onPick: () => void; thinking: boolean }>;
let settings: { loadSettingsSync: () => AppSettings | null; loadSettings: () => Promise<AppSettings> };

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null },
    ssr: { noExternal: ["ui"] },
    plugins: [{
      name: "inline-model-popover-for-ssr-test",
      enforce: "pre",
      resolveId(source, importer) {
        // Expose the actual picker's content without a browser portal or layout.
        if (source === "./Popover" && importer?.endsWith("/components/ModelPicker.tsx")) {
          return "\0inline-popover-for-test";
        }
      },
      load(id) {
        if (id === "\0inline-popover-for-test") {
          return "export function Popover({children}) { return children; }";
        }
      }
    }]
  });
  await server.ssrLoadModule("/src/i18n/index.ts");
  ({ ComputerTargetBlock } = await server.ssrLoadModule("/src/views/settings/ComputerCards.tsx"));
  ({ ModelPicker } = await server.ssrLoadModule("/src/components/ModelPicker.tsx"));
  settings = await server.ssrLoadModule("/src/store/settingsStore.ts") as typeof settings;
});
after(async () => { await server?.close(); });

async function withWindow(stored: unknown, search: string, run: () => void | Promise<void>) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { location: { search }, localStorage: { getItem: () => JSON.stringify(stored) } }
  });
  try {
    await run();
  } finally {
    if (previous) Object.defineProperty(globalThis, "window", previous);
    else Reflect.deleteProperty(globalThis, "window");
  }
}

for (const target of ["browser", "remote", "off"] as const) {
  test(`the unavailable ${target} computer mode is disabled and cannot appear active`, () => {
    const html = renderToStaticMarkup(createElement(ComputerTargetBlock, { value: target, onChange: () => {} }));
    const button = html.match(new RegExp(`<button\\b[^>]*data-testid="computer-target-${target}"[^>]*>`))?.[0];
    assert.ok(button);
    assert.match(button, /\bdisabled=""/);
    assert.match(button, /aria-checked="false"/);
    assert.match(html, /<button\b[^>]*aria-checked="true"[^>]*data-testid="computer-target-this-pc"/);
  });

  test(`stored ${target} mode does not survive either browser settings load path`, async () => {
    await withWindow({ computerTarget: target }, "", async () => {
      assert.equal(settings.loadSettingsSync()?.computerTarget, "this-pc");
      assert.equal((await settings.loadSettings()).computerTarget, "this-pc");
    });
  });
}

test("real daemon model pickers do not offer an effort setting the API cannot apply", async () => {
  await withWindow(null, "", () => {
    const html = renderToStaticMarkup(createElement(ModelPicker, { groups: [], value: "", onPick: () => {}, thinking: true }));
    assert.doesNotMatch(html, /thinking-level-label/);
  });
});

test("the opt-in model design preview retains its thinking control", async () => {
  await withWindow(null, "?mock=1&screen=models", () => {
    const html = renderToStaticMarkup(createElement(ModelPicker, { groups: [], value: "", onPick: () => {}, thinking: true }));
    assert.match(html, /thinking-level-label/);
  });
});

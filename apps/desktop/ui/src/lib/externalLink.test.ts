import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, afterEach, before, beforeEach, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";

type OpenExternal = (href: string) => boolean;
type OpenComposioLink = (href: string, dashboard?: boolean) => Promise<boolean>;

let openExternal: OpenExternal;
let openComposioLink: OpenComposioLink;
let server: ViteDevServer;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui"] }
  });
  ({ openExternal, openComposioLink } = await server.ssrLoadModule("/src/lib/externalLink.ts"));
});

after(async () => {
  await server?.close();
});

const OPEN_ARGS = ["_blank", "noopener,noreferrer"];
let calls: unknown[][];
const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

beforeEach(() => {
  calls = [];
});

afterEach(() => {
  if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

function installWindow(extra: Record<string, unknown> = {}) {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      ...extra,
      open: (...args: unknown[]) => {
        calls.push(args);
      }
    }
  });
}

for (const url of ["https://example.com", "http://example.com", "mailto:someone@example.com"]) {
  test(`openExternal opens ${url}`, () => {
    installWindow();
    assert.equal(openExternal(url), true);
    assert.deepEqual(calls, [[url, ...OPEN_ARGS]]);
  });
}

for (const url of [
  "javascript:alert(1)",
  "data:text/html,x",
  "file:///etc/passwd",
  "",
  `https://example.com/${"a".repeat(2048)}`,
  "java\tscript:alert(1)",
  "java\nscript:alert(1)"
]) {
  test(`openExternal refuses ${JSON.stringify(url).slice(0, 48)}`, () => {
    installWindow();
    assert.equal(openExternal(url), false);
    assert.deepEqual(calls, []);
  });
}

test("openComposioLink opens an allowed connect URL in a plain browser", async () => {
  installWindow();
  const url = "https://connect.composio.dev/link/lk_1?x=1";
  assert.equal(await openComposioLink(url), true);
  assert.deepEqual(calls, [[url, ...OPEN_ARGS]]);
});

test("openComposioLink opens an allowed dashboard URL in a plain browser", async () => {
  installWindow();
  const url = "https://platform.composio.dev/settings";
  assert.equal(await openComposioLink(url, true), true);
  assert.deepEqual(calls, [[url, ...OPEN_ARGS]]);
});

test("openComposioLink refuses other URLs", async () => {
  installWindow();
  for (const url of [
    "https://example.com",
    "http://connect.composio.dev/link/x",
    "https://connect.composio.dev/link?x=1",
    "javascript:alert(1)"
  ]) {
    assert.equal(await openComposioLink(url), false, url);
  }
  assert.deepEqual(calls, []);
});

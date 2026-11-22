import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, beforeEach, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";

// ownerKey reads the key through the Tauri `owner_key` command and caches it per module, so every
// test loads a fresh copy of the module graph with a fake `@tauri-apps/api/core`.
let server: ViteDevServer;
let ownerKey: (baseUrl: string | undefined) => Promise<string | undefined>;
let markRemoteBaseUrl: (baseUrl: string) => () => void;
let MOCK_BASE_URL: string;
let native: { fail: boolean; calls: unknown[] };

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] },
    plugins: [
      {
        name: "fake-owner-key",
        enforce: "pre",
        resolveId(source) {
          if (source === "@tauri-apps/api/core") return "\0fake-owner-key";
        },
        load(id) {
          if (id === "\0fake-owner-key")
            return `export const state = { fail: false, calls: [] };
export async function invoke(name, args) { state.calls.push([name, args]); if (state.fail) throw Error("no window"); return "owner-secret"; }`;
        }
      }
    ]
  });
});

beforeEach(async () => {
  server.moduleGraph.invalidateAll();
  ({ ownerKey } = await server.ssrLoadModule("/src/lib/ownerKey.ts"));
  ({ markRemoteBaseUrl } = await server.ssrLoadModule("/src/lib/liveViewState.ts"));
  ({ MOCK_BASE_URL } = await server.ssrLoadModule("/src/api/mockMode.ts"));
  native = (await server.ssrLoadModule("@tauri-apps/api/core")).state;
});

after(async () => {
  await server?.close();
});

test("mock mode gets a fixed key without asking the app", async () => {
  assert.equal(await ownerKey(MOCK_BASE_URL), "mock");
  assert.deepEqual(native.calls, []);
});

test("no key without a daemon or for a daemon on another computer", async () => {
  assert.equal(await ownerKey(undefined), undefined);
  assert.equal(await ownerKey(""), undefined);
  assert.equal(await ownerKey("https://host.example.com:47821"), undefined);
  assert.equal(await ownerKey("http://192.168.1.20:47821"), undefined);
  assert.deepEqual(native.calls, []);
});

test("no key for a loopback proxy to a paired host", async () => {
  const unmark = markRemoteBaseUrl("http://127.0.0.1:50123/");
  assert.equal(await ownerKey("http://127.0.0.1:50123"), undefined);
  assert.deepEqual(native.calls, []);
  unmark();
  assert.equal(await ownerKey("http://127.0.0.1:50123"), "owner-secret");
});

test("the local daemon gets the key from the app, read once and reused", async () => {
  assert.equal(await ownerKey("http://127.0.0.1:47821"), "owner-secret");
  assert.equal(await ownerKey("http://localhost:47821/"), "owner-secret");
  assert.equal(await ownerKey("http://[::1]:47821"), "owner-secret");
  assert.deepEqual(native.calls, [["owner_key", undefined]]);
});

test("a failed read gives no key instead of an error", async () => {
  native.fail = true;
  assert.equal(await ownerKey("http://127.0.0.1:47821"), undefined);
  assert.equal(native.calls.length, 1);
  native.fail = false;
  assert.equal(await ownerKey("http://127.0.0.1:47821"), "owner-secret");
  assert.equal(native.calls.length, 2);
});

import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { createServer, type ViteDevServer } from "vite";

type Target = {
  connection: { base_url: string; started_by_app: boolean } | null;
  baseUrl: string | undefined;
};
type Result = { code: number; stdout: string; stderr: string };
type Providers = {
  addProvider: (args: { id: string; apiKey: string }, target: Target) => Promise<Result>;
  testProvider: (id: string, target: Target) => Promise<Result>;
  setDefaultProvider: (id: string, target: Target, model?: string) => Promise<Result>;
};

let server: ViteDevServer;
let providers: Providers;
before(async () => {
  server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null }
  });
  providers = await server.ssrLoadModule("/src/lib/providers.ts") as Providers;
});
after(async () => { await server?.close(); });

const local = { base_url: "http://127.0.0.1:47821", started_by_app: true };
const actions = [
  ["add", () => ({ id: "anthropic", apiKey: "test-key" }), (target: Target) =>
    providers.addProvider({ id: "anthropic", apiKey: "test-key" }, target)],
  ["test", () => ({ id: "anthropic" }), (target: Target) =>
    providers.testProvider("anthropic", target)],
  ["default", () => ({ id: "anthropic", model: "test-model" }), (target: Target) =>
    providers.setDefaultProvider("anthropic", target, "test-model")]
] as const;

async function withNativeBridge(run: (calls: { command: string; payload: unknown }[]) => Promise<void>) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
  const calls: { command: string; payload: unknown }[] = [];
  mockIPC((command, payload) => {
    calls.push({ command, payload });
    return { code: 0, stdout: "ok", stderr: "" };
  });
  try {
    await run(calls);
  } finally {
    clearMocks();
    if (previous) Object.defineProperty(globalThis, "window", previous);
    else Reflect.deleteProperty(globalThis, "window");
  }
}

for (const [action, payload, perform] of actions) {
  test(`${action} invokes the provider bridge once for the app's active local daemon`, async () => {
    await withNativeBridge(async (calls) => {
      assert.equal((await perform({ connection: local, baseUrl: local.base_url })).code, 0);
      assert.deepEqual(calls, [{ command: "providers_cli", payload: { action, ...payload() } }]);
    });
  });

  for (const [name, target] of [
    ["remote daemon", { connection: { base_url: "http://remote.example:47821", started_by_app: false }, baseUrl: "http://remote.example:47821" }],
    ["loopback proxy or attached daemon", { connection: { ...local, started_by_app: false }, baseUrl: local.base_url }],
    ["different active client", { connection: local, baseUrl: "http://127.0.0.1:47822" }],
    ["remote URL with stale ownership", { connection: { base_url: "http://remote.example:47821", started_by_app: true }, baseUrl: "http://remote.example:47821" }],
    ["offline connection", { connection: null, baseUrl: undefined }]
  ] as const) {
    test(`${action} cannot invoke the local provider bridge for a ${name}`, async () => {
      await withNativeBridge(async (calls) => {
        await assert.rejects(perform(target), /providers_require_app_daemon/);
        assert.deepEqual(calls, [], "must not mutate/test local providers or start a daemon");
      });
    });
  }
}

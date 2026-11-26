import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";
import type { RelayApi } from "./mcpSignIn.ts";

let server: ViteDevServer;
let openSignInLink: (href: string) => Promise<boolean>;
let oauthRelay: () => RelayApi | null;
let native: { fail: boolean; calls: unknown[]; answer: unknown };
before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] },
    plugins: [
      {
        name: "fake-native",
        enforce: "pre",
        resolveId(source) {
          if (source === "@tauri-apps/api/core") return "\0fake-native";
        },
        load(id) {
          if (id === "\0fake-native") {
            return `export const state = { fail: false, calls: [], answer: undefined };
              export async function invoke(name, args) { state.calls.push([name, args]); if (state.fail) throw Error("OS failed"); return state.answer; }`;
          }
        }
      }
    ]
  });
  ({ openSignInLink, oauthRelay } = await server.ssrLoadModule("/src/lib/externalLink.ts"));
  native = (await server.ssrLoadModule("@tauri-apps/api/core")).state;
});
after(async () => {
  await server?.close();
});

test("sign-in pages open with the app's own opener, https only, and a failure is reported", async () => {
  assert.equal(await openSignInLink("http://accounts.example/auth"), false);
  assert.equal(await openSignInLink("javascript:alert(1)"), false);
  assert.equal(native.calls.length, 0);
  assert.equal(await openSignInLink("https://accounts.google.com/o/oauth2/v2/auth?state=x"), true);
  assert.deepEqual(native.calls[0], ["open_sign_in_link", { href: "https://accounts.google.com/o/oauth2/v2/auth?state=x" }]);
  native.fail = true;
  assert.equal(await openSignInLink("https://accounts.google.com/"), false);
  native.fail = false;
});

test("the redirect listener is the app's three commands", async () => {
  const relay = oauthRelay();
  assert.ok(relay);
  native.calls.length = 0;
  native.answer = { id: 3, port: 50000 };
  assert.deepEqual(await relay.listen(), { id: 3, port: 50000 });
  native.answer = "http://127.0.0.1:50000/callback?code=c&state=s";
  assert.equal(await relay.wait(3, "s"), "http://127.0.0.1:50000/callback?code=c&state=s");
  await relay.cancel(3);
  assert.deepEqual(native.calls, [
    ["oauth_relay_listen", undefined],
    ["oauth_relay_wait", { id: 3, state: "s" }],
    ["oauth_relay_cancel", { id: 3 }]
  ]);
});

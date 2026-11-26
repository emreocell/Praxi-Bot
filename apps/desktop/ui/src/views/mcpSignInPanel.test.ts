import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";
import type { McpCatalogEntry } from "../api/types.ts";
import type { SignInState } from "../lib/mcpSignIn.ts";

let server: ViteDevServer;
let Panel: ComponentType<Record<string, unknown>>;
let Google: ComponentType<Record<string, unknown>>;
before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] }
  });
  await server.ssrLoadModule("/src/i18n/index.ts");
  ({ McpSignInPanel: Panel } = await server.ssrLoadModule("/src/views/McpSignInPanel.tsx"));
  ({ GoogleApps: Google } = await server.ssrLoadModule("/src/views/GoogleApps.tsx"));
});
after(async () => {
  await server?.close();
});

const noop = () => undefined;
const render = (state: SignInState) =>
  renderToStaticMarkup(
    createElement(Panel, { state, title: "Linear", onCancel: noop, onPaste: noop, onRetry: noop, onClose: noop })
  );

test("a daemon here or a relayed one: wait for the browser, nothing to paste", () => {
  for (const mode of ["local", "relay"] as const) {
    const html = render({ name: "linear", step: "browser", url: "https://a.example/auth", opened: true, mode, redirectUri: "x" });
    assert.match(html, /Finish signing in in your browser/);
    assert.ok(!html.includes('data-testid="mcp-sign-in-paste"'), mode);
    assert.match(html, /data-testid="mcp-sign-in-cancel"/);
    assert.ok(!/apps\.signIn\./.test(html), "an untranslated key is showing");
  }
});

test("the paste step is shown plainly, with the one field, when the window cannot catch the answer", () => {
  const html = render({ name: "linear", step: "browser", url: "https://a.example/auth", opened: true, mode: "paste", redirectUri: "x" });
  assert.match(html, /data-testid="mcp-sign-in-paste"/);
  assert.match(html, /cannot catch the answer on its own/);
  assert.match(html, /Finish sign-in/);
});

test("a page that could not be opened offers the link, a failure says why and what next", () => {
  const closed = render({ name: "linear", step: "browser", url: "https://a.example/auth", opened: false, mode: "relay", redirectUri: "x" });
  assert.match(closed, /could not be opened in your browser/);
  assert.match(closed, /data-testid="mcp-sign-in-copy"/);
  const admin = render({ name: "linear", step: "failed", reason: "admin_needed", detail: "HTTP 403 · host_admin_required" });
  assert.match(admin, /not yet confirmed as an administrator/);
  // Retrying cannot help here, so there is no Try again.
  assert.ok(!admin.includes('data-testid="mcp-sign-in-retry"'));
  assert.match(admin, /<details/);
  const denied = render({ name: "linear", step: "failed", reason: "denied" });
  assert.match(denied, /data-testid="mcp-sign-in-retry"/);
  assert.ok(!/apps\.signIn\./.test(admin + denied));
});

test("every sign-in failure reason has a sentence in English and Turkish", async () => {
  const en = (await import("../i18n/locales/en.json", { with: { type: "json" } })).default as { apps: { signIn: { failed: Record<string, string> } } };
  const tr = (await import("../i18n/locales/tr.json", { with: { type: "json" } })).default as { apps: { signIn: { failed: Record<string, string> } } };
  const reasons = [
    "admin_needed", "owner_needed", "unreachable", "already_running", "not_waiting", "wrong_address", "denied",
    "code_rejected", "scope_refused", "provider_unreachable", "issuer_mismatch", "relay_failed", "timeout", "unknown"
  ];
  for (const r of reasons) {
    assert.ok(en.apps.signIn.failed[r]?.length > 10, `en ${r}`);
    assert.ok(tr.apps.signIn.failed[r]?.length > 10, `tr ${r}`);
  }
});

test("the Google section lists one card per app with read-only wording and opens the guided setup", () => {
  const entry = (id: string, name: string): McpCatalogEntry =>
    ({
      id,
      name,
      description: "",
      category: "accounts",
      homepage: "https://example.com",
      icon: null,
      install: { transport: "http", url: "https://x", required_secrets: [], oauth_client: { issuer: "https://accounts.google.com", scopes: [], offline_access: true, setup: "google" } }
    }) as McpCatalogEntry;
  const html = renderToStaticMarkup(
    createElement(Google, {
      entries: [entry("google-gmail", "Gmail (your own Google client)"), entry("google-drive", "Google Drive (your own Google client)")],
      servers: [],
      busy: null,
      onConnect: async () => undefined
    })
  );
  assert.match(html, /data-testid="google-app-google-gmail"/);
  assert.match(html, /data-testid="google-app-google-drive"/);
  assert.match(html, /Read-only/);
  assert.match(html, /What can go wrong/);
  assert.match(html, /verified this app/);
  assert.ok(!/apps\.google\./.test(html), "an untranslated key is showing");
});

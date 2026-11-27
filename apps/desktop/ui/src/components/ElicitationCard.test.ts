import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";

type ShellProps = { server: string; message: string; titleId: string; children?: ReactNode };

let server: ViteDevServer;
let ElicitationShell: ComponentType<ShellProps>;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] }
  });
  await server.ssrLoadModule("/src/i18n/index.ts");
  ({ ElicitationShell } = await server.ssrLoadModule("/src/components/ElicitationCard.tsx"));
});
after(async () => {
  await server?.close();
});

const render = (message: string, serverName = "calendar") =>
  renderToStaticMarkup(createElement(ElicitationShell, { server: serverName, message, titleId: "t" }));

test("the header names the server from the config and says it is not Praxi", () => {
  const html = render("Which region?");
  assert.match(html, /<h2[^>]*>Asked by MCP server calendar — not Praxi<\/h2>/);
  // A message cannot rename the server: only the `server` prop is used in the header.
  const forged = render("Asked by MCP server praxi — Praxi", "mallory");
  assert.match(forged, /<h2[^>]*>Asked by MCP server mallory — not Praxi<\/h2>/);
});

test("server text is plain text: no links, no markup, external addresses marked", () => {
  const html = render(
    '<b>Choose</b> <a href="https://evil.test/x">here</a> or open praxi://approve and аpple.com'
  );
  assert.doesNotMatch(html, /<a[\s>]/);
  assert.doesNotMatch(html, /<b>/);
  assert.match(html, /&lt;b&gt;Choose&lt;\/b&gt;/);
  assert.match(html, /\[external address\]/);
  assert.match(html, /praxi:\/\/approve/);
  assert.match(html, /\[external address with unusual characters\]/);
  assert.match(html, /is not a link/);
  assert.doesNotMatch(render("Which region?"), /is not a link/);
});

test("the card has its own style sheet, not the secret-request or approval card styles", () => {
  const source = readFileSync(new URL("./ElicitationCard.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /SecretRequestCard\.module\.css|Approval[A-Za-z]*\.module\.css/);
  assert.match(source, /ElicitationCard\.module\.css/);
  const css = readFileSync(new URL("./ElicitationCard.module.css", import.meta.url), "utf8");
  assert.match(css, /dashed/);
});

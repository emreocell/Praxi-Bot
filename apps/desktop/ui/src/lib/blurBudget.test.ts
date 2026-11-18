import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Performance contract: backdrop-filter is expensive in WebView2, so every blur in the
 * UI is listed here.  A new one has to be added on purpose, with a reason.  The shell has one blur
 * per panel (sidebar, center pane XOR composer, right panel); floating surfaces live in body portals;
 * nothing inside a panel, a list or a scrolling child blurs.
 */
const ROOTS = [new URL("../", import.meta.url), new URL("../../../../../packages/ui/src/", import.meta.url)].map((u) => fileURLToPath(u));
const REPO = fileURLToPath(new URL("../../../../../", import.meta.url));

const ALLOWED: Record<string, string> = {
  // Shell panels: one layer each.
  "packages/ui/src/components/Sidebar.module.css|.sidebar": "left rail",
  "packages/ui/src/components/Sidebar.module.css|:global(:root[data-float=\"on\"]) .mainPane": "center pane (none while the chat area is clear)",
  "packages/ui/src/components/Sidebar.module.css|.panel[data-open=\"true\"]": "right panel, only while open",
  // The composer replaces the center pane's blur when the chat area is clear (center-filter is `none` then).
  "apps/desktop/ui/src/views/ChatView.module.css|:global(:root[data-chat=\"clear\"]) .box": "composer over a clear chat area",
  // Floating surfaces rendered in body portals, over everything.
  "apps/desktop/ui/src/components/Modal.module.css|:global(:root[data-style=\"glass\"]) .dialog,\n:global(:root[data-style=\"tinted\"]) .dialog": "dialog (portal)",
  "apps/desktop/ui/src/components/Modal.module.css|.backdrop": "3 px veil behind a dialog (transient)",
  "apps/desktop/ui/src/components/Popover.module.css|:global(:root[data-style=\"glass\"]) .popover,\n:global(:root[data-style=\"tinted\"]) .popover": "popover (portal)",
  "apps/desktop/ui/src/components/CommandPalette.module.css|:global(:root[data-style=\"glass\"]) .dialog,\n:global(:root[data-style=\"tinted\"]) .dialog": "command palette (portal)",
  "apps/desktop/ui/src/components/Toast.module.css|:global(:root[data-style=\"glass\"]) .toast:not(.danger),\n:global(:root[data-style=\"tinted\"]) .toast:not(.danger)": "toast (fixed, top layer)",
  // Owned by PR #94 (live view): left as is.
  "apps/desktop/ui/src/views/LiveViewer.module.css|:global(:root[data-style=\"glass\"]) .backdrop": "live viewer backdrop (#94)",
  "apps/desktop/ui/src/views/LiveViewer.module.css|:global(:root[data-style=\"glass\"]) .bottomDock": "live viewer dock (#94)"
};

function cssFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (name === "node_modules") return [];
    return statSync(p).isDirectory() ? cssFiles(p) : p.endsWith(".css") ? [p] : [];
  });
}

test("every backdrop-filter in the UI is on the allowlist (one blur layer per shell panel)", () => {
  const found: string[] = [];
  for (const file of ROOTS.flatMap(cssFiles)) {
    const text = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const key = relative(REPO, file).replaceAll("\\", "/");
    for (const m of text.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      if (/backdrop-filter\s*:\s*(?!none)/.test(m[2])) found.push(`${key}|${m[1].trim().replace(/\s*\n\s*/g, "\n")}`);
    }
  }
  const unexpected = found.filter((f) => !(f in ALLOWED));
  assert.deepEqual(unexpected, [], "a new backdrop-filter: add it to the allowlist with a reason, or remove it");
  const stale = Object.keys(ALLOWED).filter((k) => !found.includes(k));
  assert.deepEqual(stale, [], "allowlist entries without a matching rule");
});

test("the inline `/` and `@` menu in the center pane has no blur (it sits under the pane's own layer)", () => {
  const chat = readFileSync(new URL("../views/ChatView.module.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const rules = [...chat.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter((m) => /(^|[\s,])\.menu\b(?!\w)/.test(m[1]) && !/\.menu\w/.test(m[1]));
  assert.ok(rules.length > 0);
  for (const r of rules) assert.doesNotMatch(r[2], /backdrop-filter\s*:\s*(?!none)/, r[1]);
  assert.ok(rules.some((r) => /background-color:\s*var\(--color-surface-elevated-opaque\)/.test(r[2])), "opaque fill");
  assert.ok(!rules.some((r) => /--float-fill/.test(r[2])), "never the translucent float fill");
});

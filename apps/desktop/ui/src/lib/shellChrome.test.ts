import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

/* The shell chrome contract (design wave 2), checked on the shipped sources: no DOM in node --test. */
const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const app = read("../App.tsx");
const css = read("../App.module.css");
const appsPillCss = read("../components/AppsPill.module.css");
const appsPillTsx = read("../components/AppsPill.tsx");
const sidebarCss = read("../../../../../packages/ui/src/components/Sidebar.module.css");
const block = (source: string, selector: string) => {
  const start = source.indexOf(`${selector} {`);
  return start < 0 ? "" : source.slice(start, source.indexOf("}", start));
};

test("the title bar has exactly one place picker: the left 'This computer' pill is gone", () => {
  assert.equal((app.match(/<ComputerPicker\b/g) ?? []).length, 1);
  assert.doesNotMatch(app, /runtimeLabel|runtimeName|account\.localComputer/);
  assert.doesNotMatch(css, /\.runtimeLabel|\.runtimeName|\.liveDot/);
  assert.match(app, /className=\{styles\.titleLead\} id=\{TITLEBAR_LEAD_ID\}/);
});

test("the title bar keeps the height the overlay panel is offset by (Sidebar.module.css)", () => {
  assert.match(block(css, ".titleBar"), /min-height:\s*calc\(var\(--space-12\) \+ var\(--space-1\)\)/);
  assert.match(sidebarCss, /top:\s*calc\(var\(--titlebar-h, var\(--space-0\)\) \+ var\(--space-12\) \+ var\(--space-1\)\)/);
});

test("rail rows: selected is the accent state layer with a straight marker, not a curved inset edge", () => {
  assert.match(css, /\.rail \.railItem\[aria-current="page"\][^{]*\{[^}]*background-color:\s*var\(--state-selected\)/);
  assert.match(css, /\.rail \.railItem\[aria-current="page"\]::before/);
  assert.doesNotMatch(css, /box-shadow:\s*inset var\(--size-border-thick\) 0 0/);
  assert.match(block(css, ".rail .railItem"), /border-radius:\s*var\(--radius-md\)/);
});

test("Apps sits immediately to the right of the profile row at every rail width (icon rail: stacked above it)", () => {
  // One row, profile first: the account trigger takes the room, the Apps button keeps its own size.
  const footer = app.slice(app.indexOf("className={styles.footRow}"), app.indexOf("</SidebarFooter>"));
  assert.ok(footer.indexOf("<AccountMenu") >= 0 && footer.indexOf("<AppsPill") > footer.indexOf("<AccountMenu"), "AccountMenu, then AppsPill");
  assert.match(block(css, ".footRow"), /flex-direction:\s*row/);
  assert.match(block(css, ".footRow"), /align-items:\s*center/);
  assert.doesNotMatch(block(css, ".footRow"), /flex-wrap:\s*wrap/);
  assert.match(block(appsPillCss, ".pill"), /flex:\s*none/);
  // Only the icon rail is too narrow for two controls side by side: Apps goes above the avatar there.
  assert.match(block(css, ".railMini .footRow"), /flex-direction:\s*column-reverse/);
});

test("the Apps button previews Discord, Gmail and GitHub on the rail", () => {
  assert.match(appsPillTsx, /"discord",\s*"gmail",\s*"github"/);
  assert.match(appsPillTsx, /monochrome/);
});

test("the Apps button: grid icon on a narrow rail, three preview logos from medium, label from large", () => {
  assert.match(appsPillCss, /\.label \{[^}]*display:\s*none/);
  assert.match(appsPillCss, /\[data-rail-tier="large"\]\) \.label,\s*:global\(\[data-rail-tier="wide"\]\) \.label \{\s*display:\s*inline/);
  assert.match(appsPillCss, /\[data-rail-tier="medium"\]\) \.stack,\s*\n:global\(\[data-rail-tier="large"\]\) \.stack/);
  assert.match(appsPillCss, /\[data-rail-tier="medium"\]\) \.stack ~ \.icon,\s*\n:global\(\[data-rail-tier="large"\]\) \.stack ~ \.icon/);
  assert.match(block(appsPillCss, ".pill"), /white-space:\s*nowrap/);
});

test("the wordmark steps aside on a narrow rail", () => {
  assert.match(css, /\[data-rail-tier="narrow"\]\) \.brandName \{\s*display:\s*none/);
});

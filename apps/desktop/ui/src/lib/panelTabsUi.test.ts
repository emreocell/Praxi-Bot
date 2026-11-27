import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

/* Owner UI feedback on the rail, the profile button and the bot panel's Tools and Library tabs,
   checked on the shipped sources (no DOM in node --test). */
const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const app = read("../App.tsx");
const appCss = read("../App.module.css");
const accountTsx = read("../components/AccountMenu.tsx");
const accountCss = read("../components/AccountMenu.module.css");
const browser = read("../components/BotBrowserProfile.tsx");
const mail = read("../components/BotPanelMail.tsx");
const library = read("../components/BotPanelLibrary.tsx");
const botPanel = read("../components/BotPanel.tsx");
const rows = read("../components/PanelRows.tsx");
const block = (source: string, selector: string) => {
  const start = source.indexOf(`${selector} {`);
  return start < 0 ? "" : source.slice(start, source.indexOf("}", start));
};
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const lookup = (tree: Record<string, unknown>, key: string): unknown =>
  key.split(".").reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], tree);

test("the collapsed rail scrolls without drawing a scrollbar", () => {
  assert.match(app, /<SidebarSection className=\{styles\.railScroll\}>/);
  assert.match(block(appCss, ".railMini .railScroll"), /scrollbar-width:\s*none/);
  assert.match(appCss, /\.railMini \.railScroll::-webkit-scrollbar \{\s*display:\s*none/);
  // Hiding the bar must not hide the content: the section keeps its own overflow-y (Sidebar.module.css).
  assert.doesNotMatch(block(appCss, ".railMini .railScroll"), /overflow:\s*hidden/);
});

test("the selected bot shows no unread dot: its selected background already says it", () => {
  assert.match(app, /unreadBots\.has\(bot\.id\) && routeBotId !== bot\.id/);
});

test("the profile button is as wide as the name, has no up/down chevron, and its menu is narrower", () => {
  assert.doesNotMatch(accountTsx, /ChevronsUpDown|chevron/i);
  assert.doesNotMatch(accountCss, /\.chevron/);
  assert.match(block(accountCss, ".trigger"), /width:\s*fit-content/);
  assert.match(block(accountCss, ".trigger"), /max-width:\s*100%/);
  assert.doesNotMatch(block(accountCss, ".name"), /flex:\s*1/);
  assert.match(block(appCss, ".footRow > :first-child"), /flex:\s*0 1 auto/);
  // Apps stays at the rail's edge even though the profile no longer fills the row.
  assert.match(block(appCss, ".footRow > :last-child"), /margin-left:\s*auto/);
  const width = Number(/const MENU_WIDTH = (\d+)/.exec(accountTsx)?.[1]);
  assert.ok(width > 0 && width < 272, `menu width ${width} is narrower than the old 272`);
  assert.match(accountTsx, /width=\{MENU_WIDTH\}/);
});

test("the light-model hint is the small icon and popover; no permanent box is left in the panel", () => {
  assert.match(botPanel, /<ModelHint onChangeModel=\{onOpenSettings\} \/>/);
  assert.doesNotMatch(botPanel, /liteHint|liteFullHint/);
  assert.doesNotMatch(read("../components/BotPanel.module.css"), /\.liteHint/);
});

test("Tools: grouped cards, a state, one primary action each, sites one per line", () => {
  assert.match(browser, /<PanelGroup\b/);
  assert.match(mail, /<PanelGroup\b/);
  // One primary button per card: sign in (browser) and connect (mail, only while there is no mailbox).
  assert.equal([...browser.matchAll(/variant="primary"/g)].length, 1);
  assert.equal([...mail.matchAll(/variant="primary"/g)].length, 1);
  assert.match(browser, /<StatusChip\b/);
  assert.match(browser, /<PanelRows\b/);
  // The long explanations are folded, not shown on open.
  assert.match(browser, /<details className=\{styles\.about\}>/);
  assert.match(mail, /<details className=\{styles\.more\}>/);
  assert.doesNotMatch(browser, /<p className=\{styles\.warning\}>/);
  // The tab order and the panel's tab ids are unchanged.
  assert.match(botPanel, /\["activity", "automation", "library", "tools"\] as const/);
});

test("Library: two groups, no intro paragraph, rows go through the shared list", () => {
  assert.match(library, /testId="library-group-knows"/);
  assert.match(library, /testId="library-group-keeps"/);
  assert.doesNotMatch(library, /styles\.intro/);
  assert.equal([...library.matchAll(/<PanelRows\b/g)].length, 2, "documents and notes; skills are in BotPanelSkills");
  assert.match(library, /aria-describedby=\{`\$\{id\}-line`\}/, "the shelf description stays for assistive technology");
});

test("the shared list is accessible: named list, labelled search, polite result count, toggle state", () => {
  assert.match(rows, /<ul className=\{styles\.rows\} aria-label=\{label\}>/);
  assert.match(rows, /aria-label=\{searchLabel\}/);
  assert.match(rows, /role="status"/);
  assert.match(rows, /aria-expanded=\{all\}/);
  assert.match(rows, /aria-label=\{row\.action\.label\}/);
  // A state is words plus a dot, never colour alone.
  assert.match(rows, /<span className=\{styles\.dot\} aria-hidden="true" \/>\s*\{children\}/);
});

test("every new panel text exists in English and Turkish", () => {
  const sources = [browser, mail, library, rows, read("../components/BotPanelSkills.tsx")].join("\n");
  const keys = new Set([...sources.matchAll(/\bt\("(panelTabs\.[a-zA-Z.]+)"/g)].map((m) => m[1]));
  assert.ok(keys.size >= 12, "the new tab code uses the panelTabs texts");
  for (const lang of ["en", "tr"]) {
    const tree = locale(lang);
    for (const key of keys) {
      const value = lookup(tree, key) ?? lookup(tree, `${key}_other`);
      assert.equal(typeof value, "string", `${lang}: ${key}`);
    }
  }
  const fallback = JSON.parse(read("../i18n/scaffold-locales.json")) as { englishFallbackPrefixes: string[] };
  assert.ok(fallback.englishFallbackPrefixes.includes("panelTabs."), "other languages fall back to English for the new texts");
});

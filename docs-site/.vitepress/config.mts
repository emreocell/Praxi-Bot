import { defineConfig } from "vitepress";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, posix, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// The documentation site is built from the Markdown in ../docs. Nothing is copied:
// the same files are read on GitHub and on the website, so the docs link checker
// (scripts/check_markdown_links.py) covers both.
const here = dirname(fileURLToPath(import.meta.url));
const docsDir = resolve(here, "../../docs");
const REPO = "https://github.com/emreocell/Praxi-Bot";
type Lang = "en" | "tr" | "de";

const read = (file: string) => readFileSync(join(docsDir, file), "utf8");

function title(file: string): string {
  const m = /^#\s+(.+)$/m.exec(read(file));
  return (m ? m[1] : file).replace(/`/g, "").trim();
}

const pageLink = (file: string) => "/" + file.replace(/(^|\/)index\.md$/, "$1").replace(/\.md$/, "");

function mdFiles(dir: string): string[] {
  return readdirSync(join(docsDir, dir))
    .filter((f) => f.endsWith(".md") && f !== "index.md")
    .sort();
}

// Sidebar groups are the "## sections" of a guide index; each relative .md link in a
// section becomes one entry, in index order. Adding a page to the index adds it here.
function guideSidebar(lang: Lang) {
  const dir = lang === "en" ? "guide" : `guide/${lang}`;
  const index = read(`${dir}/index.md`);
  const groups: { text: string; collapsed: boolean; items: { text: string; link: string }[] }[] = [];
  const seen = new Set<string>();
  let current: (typeof groups)[number] | null = null;
  let inFence = false;
  for (const line of index.split(/\r?\n/)) {
    if (/^\s*(```|~~~)/.test(line)) inFence = !inFence;
    if (inFence) continue;
    const h = /^##\s+(.+)$/.exec(line);
    if (h) {
      current = { text: h[1].replace(/`/g, "").trim(), collapsed: true, items: [] };
      groups.push(current);
      continue;
    }
    if (!current) continue;
    for (const m of line.matchAll(/\[([^\]]+)\]\(([^)#\s]+\.md)\)/g)) {
      const target = posix.normalize(posix.join(dir, m[2]));
      if (!target.startsWith("guide/") || seen.has(target)) continue;
      seen.add(target);
      current.items.push({ text: m[1].replace(/`/g, ""), link: pageLink(target) });
    }
  }
  const useful = groups.filter((g) => g.items.length > 0);
  // The first group (the beginner pages) stays open so newcomers see it at once.
  if (useful.length) useful[0].collapsed = false;
  return [{ text: title(`${dir}/index.md`), link: pageLink(`${dir}/index.md`) }, ...useful];
}

const translated = Object.fromEntries(
  (["tr", "de"] as const).map((l) => [l, mdFiles(`guide/${l}`).map((f) => f.replace(/\.md$/, ""))]),
);

// docs/contributing/README.md is the folder landing page (see `rewrites` below).
const contributing = [
  { text: title("contributing/README.md"), link: "/contributing/" },
  ...mdFiles("contributing")
    .filter((f) => f !== "README.md")
    .map((f) => ({ text: title(`contributing/${f}`), link: pageLink(`contributing/${f}`) })),
];
const design = mdFiles("design");
const isAdr = (f: string) => f.endsWith("-adr.md");
const designSidebar = [
  {
    text: "Design notes",
    items: design.filter((f) => !isAdr(f)).map((f) => ({ text: title(`design/${f}`), link: pageLink(`design/${f}`) })),
  },
  {
    text: "Decisions (ADRs)",
    items: design.filter(isAdr).map((f) => ({ text: title(`design/${f}`), link: pageLink(`design/${f}`) })),
  },
];

// Links that leave docs/ (README, crates, scripts ...) or point at non-page files cannot be
// pages on the site: send them to the file on GitHub instead of a dead link.
function githubLinks(md: import("markdown-it")) {
  md.core.ruler.push("praxi_github_links", (state) => {
    const rel: string | undefined = state.env?.relativePath;
    if (!rel) return;
    for (const block of state.tokens) {
      for (const tok of block.children ?? []) {
        if (tok.type !== "link_open") continue;
        const href = tok.attrGet("href");
        if (!href || /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href) || href.startsWith("#")) continue;
        const [path, hash] = href.split("#");
        if (!path) continue;
        const target = posix.normalize(posix.join("docs", posix.dirname(rel), decodeURI(path)));
        const outside = target.startsWith("..") || !target.startsWith("docs/");
        const notAPage = !/\.(md|png|jpe?g|gif|svg|webp)$/i.test(target);
        if (outside || notAPage) {
          tok.attrSet("href", `${REPO}/blob/main/${target}${hash ? "#" + hash : ""}`);
        }
      }
    }
  });
}


// Prose in the docs writes placeholders such as <bot-id> and {{ }} that GitHub shows as text but
// Vue would try to compile as components or expressions. Keep a short allow-list of real HTML
// and turn everything else into literal text, so the Markdown stays unchanged.
const SAFE_TAGS = new Set([
  "a", "b", "br", "code", "details", "div", "em", "i", "img", "kbd", "p", "picture", "source",
  "span", "strong", "sub", "summary", "sup",
]);
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
function literalPlaceholders(md: import("markdown-it")) {
  md.core.ruler.push("praxi_literal_placeholders", (state) => {
    const unsafe = (html: string) => {
      const tags = [...html.matchAll(/<\/?([a-zA-Z][\w-]*)/g)].map((m) => m[1].toLowerCase());
      return tags.some((t) => !SAFE_TAGS.has(t));
    };
    for (const block of state.tokens) {
      if (block.type === "html_block" && unsafe(block.content)) {
        block.content = `<p>${esc(block.content.trim())}</p>
`;
      }
      for (const tok of block.children ?? []) {
        if (tok.type === "html_inline" && unsafe(tok.content)) tok.content = esc(tok.content);
        else if (tok.type === "text" && /[{}]/.test(tok.content)) {
          tok.type = "html_inline";
          tok.content = esc(tok.content).replace(/\{/g, "&#123;").replace(/\}/g, "&#125;");
        }
      }
    }
  });
}

export default defineConfig({
  title: "Praxi",
  description: "Documentation for Praxi, the self-hosted AI agent that asks before it acts.",
  lang: "en",
  srcDir: "../docs",
  // GitHub project pages live under /<repo>/. Override for a custom domain or a local preview.
  base: process.env.DOCS_BASE ?? "/Praxi-Bot/",
  cleanUrls: true,
  rewrites: { "contributing/README.md": "contributing/index.md" },
  // Dead links fail the build. The Markdown link checker is the first gate; this catches
  // anything it does not model (for example a page that is missing from the site).
  ignoreDeadLinks: false,
  head: [
    ["meta", { name: "theme-color", content: "#111217" }],
    ["link", { rel: "icon", href: "data:," }],
  ],
  vite: {
    // The Markdown lives outside this folder, so Node would not find `vue` from there.
    resolve: {
      alias: [{ find: /^vue\/server-renderer$/, replacement: join(here, "../node_modules/vue/server-renderer/index.mjs") },
        { find: /^vue$/, replacement: join(here, "../node_modules/vue/dist/vue.runtime.esm-bundler.js") }],
    },
  },
  markdown: {
    config: (md) => {
      githubLinks(md);
      literalPlaceholders(md);
    },
  },
  themeConfig: {
    siteTitle: "Praxi docs",
    translated,
    nav: [
      { text: "Start here", link: "/guide/", activeMatch: "^/guide/(?!tr/|de/)" },
      { text: "Contributors", link: "/contributing/", activeMatch: "^/contributing/" },
      { text: "Design & decisions", link: "/design/permissions-ux", activeMatch: "^/design/" },
    ],
    sidebar: {
      "/guide/tr/": guideSidebar("tr"),
      "/guide/de/": guideSidebar("de"),
      "/guide/": guideSidebar("en"),
      "/contributing/": contributing,
      "/design/": designSidebar,
    },
    search: {
      provider: "local",
      options: { detailedView: true },
    },
    editLink: {
      pattern: `${REPO}/edit/main/docs/:path`,
      text: "Edit this page on GitHub",
    },
    outline: { level: [2, 3] },
    socialLinks: [{ icon: "github", link: REPO }],
    footer: {
      message: "Praxi is open source. These pages are plain Markdown in the repository.",
    },
  },
});

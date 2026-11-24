// Builds the static site into site/dist: pages, CSS (with the app's Obsidian
// tokens read from packages/ui/tokens.css), images copied from the repo, JS.
// No dependencies. Run: node site/scripts/build.mjs
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { renderPage } from "../src/render.mjs";
import { SITE } from "../src/content.mjs";
import { ogSvg } from "./og.mjs";
import { themeTokens } from "./build-tokens.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const site = join(here, "..");
const repo = join(site, "..");
const dist = process.env.SITE_DIST || join(site, "dist");

rmSync(dist, { recursive: true, force: true });
mkdirSync(join(dist, "img"), { recursive: true });
mkdirSync(join(dist, "tr"), { recursive: true });

writeFileSync(join(dist, "index.html"), renderPage("en"));
writeFileSync(join(dist, "tr", "index.html"), renderPage("tr"));
writeFileSync(join(dist, "style.css"), `${themeTokens()}\n${readFileSync(join(site, "src/style.css"), "utf8")}`);
// Ship .js (some static servers send .mjs as text/plain, which breaks module scripts).
writeFileSync(join(dist, "app.js"), readFileSync(join(site, "src/app.js"), "utf8").replace("./release.mjs", "./release.js"));
copyFileSync(join(site, "src/release.mjs"), join(dist, "release.js"));

const screens = join(repo, "docs/assets/readme/screens");
for (const f of ["approval-card.webp", "screen-permissions.webp", "screen-schedules.webp", "screen-live-view.webp"]) {
  cpSync(join(screens, f), join(dist, "img", f));
}
cpSync(join(repo, "packages/ui/src/assets/renders/ghost.webp"), join(dist, "img/ghost.webp"));
cpSync(join(repo, "packages/ui/src/assets/chars/ghost-sm.webp"), join(dist, "img/ghost-sm.webp"));

writeFileSync(join(dist, "og.svg"), ogSvg(themeTokens()));
if (!existsSync(join(site, "og.png"))) throw new Error("site/og.png missing: run node site/scripts/render-og.mjs");
cpSync(join(site, "og.png"), join(dist, "og.png"));

writeFileSync(join(dist, "robots.txt"), `User-agent: *\nAllow: /\nSitemap: ${SITE}sitemap.xml\n`);
writeFileSync(
  join(dist, "sitemap.xml"),
  `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n  <url><loc>${SITE}</loc></url>\n  <url><loc>${SITE}tr/</loc></url>\n</urlset>\n`,
);
writeFileSync(join(dist, ".nojekyll"), "");
console.log("site built in", dist);

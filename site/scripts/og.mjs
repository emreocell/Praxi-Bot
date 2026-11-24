// Social preview (Open Graph) image source, drawn only from the app's theme
// tokens and the ghost mascot. site/og.png is rendered from this by
// render-og.mjs; build.mjs also ships the SVG.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

export function token(tokens, name) {
  const m = tokens.match(new RegExp(`--${name}:\\s*([^;]+);`));
  if (!m) throw new Error(`token --${name} not found`);
  return m[1].trim();
}

export function ogSvg(tokens) {
  const bg = token(tokens, "color-background");
  const surface = token(tokens, "color-surface-elevated");
  const text = token(tokens, "color-text");
  const muted = token(tokens, "color-text-muted");
  const accent = token(tokens, "color-accent");
  const ghost = readFileSync(join(repo, "packages/ui/src/assets/renders/ghost.webp")).toString("base64");
  return `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1200" height="630" viewBox="0 0 1200 630" role="img" aria-label="Praxi-Bot: a local-first AI agent you control">
  <defs>
    <radialGradient id="glow" cx="78%" cy="45%" r="55%"><stop offset="0" stop-color="${accent}" stop-opacity="0.28"/><stop offset="1" stop-color="${accent}" stop-opacity="0"/></radialGradient>
  </defs>
  <rect width="1200" height="630" fill="${bg}"/>
  <rect width="1200" height="630" fill="url(#glow)"/>
  <rect x="48" y="48" width="1104" height="534" rx="28" fill="${surface}" fill-opacity="0.55" stroke="${text}" stroke-opacity="0.12"/>
  <image href="data:image/webp;base64,${ghost}" x="830" y="150" width="310" height="310"/>
  <text x="96" y="210" font-family="Segoe UI, system-ui, sans-serif" font-size="30" font-weight="600" fill="${accent}">Praxi-Bot · alpha</text>
  <text x="96" y="300" font-family="Segoe UI, system-ui, sans-serif" font-size="62" font-weight="700" fill="${text}">A local-first AI agent</text>
  <text x="96" y="372" font-family="Segoe UI, system-ui, sans-serif" font-size="62" font-weight="700" fill="${text}">you control.</text>
  <text x="96" y="450" font-family="Segoe UI, system-ui, sans-serif" font-size="28" fill="${muted}">Your computer or your own server. It asks before it acts.</text>
  <text x="96" y="492" font-family="Segoe UI, system-ui, sans-serif" font-size="28" fill="${muted}">Open source · no tracking</text>
</svg>
`;
}

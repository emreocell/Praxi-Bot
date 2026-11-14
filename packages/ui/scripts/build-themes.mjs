import { readFileSync, writeFileSync } from 'node:fs';
import { URL } from 'node:url';
import { buildTokens, colorSchemeFor, fallbackAccent, deriveTheme, paletteById } from '../src/themeModel.ts';

// The derived model (src/themeModel.ts) is the source of truth.  tokens.css only
// carries the default theme (Obsidian) so the first paint is correct before JS
// runs; `applyTheme` sets every token inline afterwards.
const root = new URL('../', import.meta.url);
const theme = paletteById('obsidian');
const accent = fallbackAccent(deriveTheme(theme));
const tokens = buildTokens(theme, accent, 'solid', false);
const css = `:root {\n  color-scheme: ${colorSchemeFor(theme)};\n${Object.entries(tokens).map(([k, v]) => `  ${k}: ${v};`).join('\n')}\n}`;
const path = new URL('tokens.css', root);
const base = readFileSync(path, 'utf8').split('/* Generated themes')[0].trimEnd();
writeFileSync(path, `${base}\n\n/* Generated default theme — pnpm --dir packages/ui generate:themes */\n${css}\n`);

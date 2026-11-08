import { readFileSync, writeFileSync } from 'node:fs';
import { URL } from 'node:url';

// JSON is the source of truth (ADR 0022). Commit the generated CSS for Vite/Tauri.
const root = new URL('../', import.meta.url);
const names = ['dark', 'cobalt', 'raven', 'light'];
const css = names.map((file, i) => {
  const theme = JSON.parse(readFileSync(new URL(`themes/${file}.json`, root), 'utf8'));
  const selectors = `${i === 0 ? ':root,\n' : ''}:root[data-theme="${theme.name}"]`;
  const preview = `[data-theme-preview="${theme.name}"]`;
  return `${selectors},\n${preview} {\n  color-scheme: ${theme.mode};\n${Object.entries(theme.tokens).map(([k,v]) => `  ${k}: ${v};`).join('\n')}\n}`;
}).join('\n\n');
const path = new URL('tokens.css', root);
const base = readFileSync(path, 'utf8').split('/* Generated themes')[0].trimEnd();
writeFileSync(path, `${base}\n\n/* Generated themes — pnpm --dir packages/ui generate:themes */\n${css}\n`);

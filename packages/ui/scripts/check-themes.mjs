import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { URL } from 'node:url';
import process from 'node:process';

const root = new URL('../', import.meta.url);
const luminance = hex => {
  const c = hex.slice(1).match(/../g).map(v => parseInt(v, 16) / 255).map(v => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4);
  return c[0] * .2126 + c[1] * .7152 + c[2] * .0722;
};
const ratio = (a, b) => {
  const [low, high] = [luminance(a), luminance(b)].sort((x, y) => x - y);
  return (high + .05) / (low + .05);
};
const mix = (a, b, weight) => '#' + [1, 3, 5].map(i => Math.round(parseInt(a.slice(i, i + 2), 16) * weight + parseInt(b.slice(i, i + 2), 16) * (1 - weight)).toString(16).padStart(2, '0')).join('');
let tokenKeys;
const css = readFileSync(new URL('tokens.css', root), 'utf8');
for (const file of ['dark', 'cobalt', 'raven', 'light']) {
  const { name, tokens: t } = JSON.parse(readFileSync(new URL(`themes/${file}.json`, root), 'utf8'));
  const keys = Object.keys(t).sort();
  if (tokenKeys) assert.deepEqual(keys, tokenKeys, `${name}: complete token contract`);
  tokenKeys = keys;
  assert.ok(css.includes(`:root[data-theme="${name}"]`));
  const check = (foreground, background, minimum, context) => {
    const r = ratio(foreground, background);
    assert.ok(r >= minimum, `${name}: ${context} ${r.toFixed(2)} < ${minimum}`);
    return r;
  };
  let lowest = Infinity;
  for (const surface of ['background', 'surface', 'surface-elevated', 'surface-active']) {
    for (const text of ['text', 'text-secondary', 'text-muted', 'accent-text', 'success', 'warning', 'danger']) {
      lowest = Math.min(lowest, check(t[`--color-${text}`], t[`--color-${surface}`], 4.5, `${text}/${surface}`));
    }
    check(t['--color-focus'], t[`--color-${surface}`], 3, `focus/${surface}`);
    check(t['--color-border-strong'], t[`--color-${surface}`], 3, `control border/${surface}`);
  }
  check(t['--color-accent-contrast'], t['--color-accent'], 4.5, 'accent button');
  const hover = t['--color-accent-hover'].startsWith('#') ? t['--color-accent-hover'] : mix(t['--color-accent'], '#ffffff', .88);
  check(t['--color-accent-contrast'], hover, 4.5, 'accent button hover');
  check(t['--color-danger-contrast'], t['--color-danger'], 4.5, 'danger button');
  for (const status of ['success', 'warning', 'danger']) {
    check(t[`--color-${status}`], mix(t[`--color-${status}`], t['--color-surface'], .16), 4.5, `${status} badge`);
  }
  for (const token of ['text', 'keyword', 'string', 'number', 'comment']) {
    check(t[`--color-code-${token}`], t['--color-code-bg'], 4.5, `code ${token}`);
  }
  // A selector must contain every value in its source JSON (no stale generated CSS).
  const block = css.split(`:root[data-theme="${name}"]`)[1].split('}')[0];
  for (const [k, v] of Object.entries(t)) assert.ok(block.includes(`${k}: ${v};`), `${name}: stale ${k}`);
  process.stdout.write(`${name}: ${keys.length} tokens; minimum text contrast ${lowest.toFixed(2)}:1; states/code/focus AA passed\n`);
}

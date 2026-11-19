#!/usr/bin/env node
// Generates the README artwork into docs/assets/readme/. Deterministic and offline.
//   node scripts/readme-assets/build.mjs            build everything
//   node scripts/readme-assets/build.mjs hero       build only files whose name contains "hero"
import { mkdirSync, writeFileSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { themes } from './lib/core.mjs';
import { hero } from './scenes/hero.mjs';
import { featuresScene } from './scenes/features.mjs';
import { architectureScene } from './scenes/architecture.mjs';
import { stepsScene, statsScene, dividerScene } from './scenes/strips.mjs';

const out = fileURLToPath(new URL('../../docs/assets/readme/', import.meta.url));
const only = process.argv[2];
const files = {};
for (const name of ['dark', 'light']) {
  const T = themes[name];
  files['hero-' + name + '.svg'] = () => hero(T, false);
  files['hero-' + name + '-mobile.svg'] = () => hero(T, true);
  files['features-' + name + '.svg'] = () => featuresScene(T, false);
  files['features-' + name + '-mobile.svg'] = () => featuresScene(T, true);
  files['architecture-' + name + '.svg'] = () => architectureScene(T, false);
  files['architecture-' + name + '-mobile.svg'] = () => architectureScene(T, true);
  files['steps-' + name + '.svg'] = () => stepsScene(T, false);
  files['steps-' + name + '-mobile.svg'] = () => stepsScene(T, true);
  files['stats-' + name + '.svg'] = () => statsScene(T, false);
  files['stats-' + name + '-mobile.svg'] = () => statsScene(T, true);
  files['divider-' + name + '.svg'] = () => dividerScene(T);
}
mkdirSync(out, { recursive: true });
for (const [name, render] of Object.entries(files)) {
  if (only && !name.includes(only)) continue;
  const svg = render();
  const target = join(out, name);
  writeFileSync(target + '.tmp', svg);
  renameSync(target + '.tmp', target);
  console.log(name.padEnd(34), (Buffer.byteLength(svg) / 1024).toFixed(1) + ' KB');
}

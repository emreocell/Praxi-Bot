import { spawnSync } from 'node:child_process';
import { mkdirSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import process from 'node:process';

const destination = resolve(process.argv[2] ?? '../../../../design/v4');
const prefix = process.argv[3] ?? '';
const chrome = 'C:/Program Files/Google/Chrome/Application/chrome.exe';
mkdirSync(destination, { recursive: true });
const screens = ['obsidian', 'clay'].flatMap(theme =>
  ['chat', 'empty', 'settings', 'apps', 'models'].map(screen => [theme, screen]));
screens.push(['cobalt', 'chat'], ['raven', 'chat']);
for (const [theme, screen] of screens) {
  const path = resolve(destination, `${prefix}${theme}-${screen}.png`);
  const result = spawnSync(chrome, ['--headless=new', '--disable-gpu', '--no-sandbox',
    '--hide-scrollbars', '--force-device-scale-factor=1', '--window-size=1600,1000',
    '--virtual-time-budget=8000', '--run-all-compositor-stages-before-draw',
    `--screenshot=${path}`, `http://localhost:5182/?mock=1&theme=${theme}&screen=${screen}`],
  { windowsHide: true, timeout: 45000, stdio: 'pipe' });
  if (result.status !== 0 || statSync(path).size < 10000) throw new Error(`Capture failed: ${path}`);
  process.stdout.write(`${path}\n`);
}

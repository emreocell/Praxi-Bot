# README artwork generator

Generates the hand-crafted SVGs in [`docs/assets/readme/`](../../docs/assets/readme/) used by the
repository README: hero, feature grid, architecture diagram, "how it works" strip, facts strip and a
section divider, each in a dark (Obsidian) and a light (Clay) theme and in a desktop and a mobile
layout.

```sh
node scripts/readme-assets/build.mjs          # rebuild everything
node scripts/readme-assets/build.mjs hero     # only files whose name contains "hero"
```

Node 22 or newer. There are no dependencies and no network access: the output is deterministic, so a
rebuild with unchanged sources produces byte-identical files.

## What the files are

| File | Use |
| --- | --- |
| `hero-{dark,light}[-mobile].svg` | Top of the README: wordmark, tagline and the real bot characters on a grainy mesh |
| `features-{dark,light}[-mobile].svg` | Eight capability cards |
| `architecture-{dark,light}[-mobile].svg` | Daemon, clients, models, tools, node-agents, permission gate |
| `steps-{dark,light}[-mobile].svg` | Install, create a bot, chat and approve |
| `stats-{dark,light}[-mobile].svg` | Four facts: loopback only, no inbound ports, no telemetry, Apache-2.0 |
| `divider-{dark,light}.svg` | Thin ember line with a clay bead (transparent background) |

Desktop files are 1280 px wide; mobile files are 640 px wide and meant for narrow screens.

## Using them in the README

GitHub ignores viewport meta tags, so the mobile layouts are selected with a media query on the
`<source>`, as in the profile README. Order matters: the first matching `<source>` wins.

```html
<picture>
  <source media="(prefers-color-scheme: dark) and (max-width: 680px)" srcset="docs/assets/readme/hero-dark-mobile.svg">
  <source media="(prefers-color-scheme: light) and (max-width: 680px)" srcset="docs/assets/readme/hero-light-mobile.svg">
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/readme/hero-dark.svg">
  <img src="docs/assets/readme/hero-light.svg" alt="Praxi: a local-first, open-source AI agent platform" width="100%">
</picture>
```

Give every image meaningful `alt` text (each SVG also carries its own `<title>` and `<desc>`).
Keep the same information in plain Markdown near the image so the page works without images.

## How it is built

- `lib/core.mjs`: SVG helpers, the two themes, the grain filter (`feTurbulence` fractal noise blended
  with soft-light), the mesh gradients and icon drawing.
- `lib/characters.mjs`: the real Praxi bot characters, drawn the way the desktop app draws them
  (`packages/ui/src/components/BotAvatar.tsx`): the off-white 3D clay WebP from
  `packages/ui/src/assets/chars/` is tinted with the app's gradient-map SVG filter (a port of
  `gradientMapConfig` in `packages/ui/src/botShape.ts`) and gets two tilted dark pill eyes using the
  per-shape eye coordinates parsed from that file. The hero uses the ghost in white (the app's white tint
  `#f4f4f5`) plus cloud (lilac), capsule (teal) and pebble (amber) from the app's tint swatches. The art is
  read from the repository at build time and embedded as base64 WebP (about 150 KB per hero SVG).
  On top of the app look the hero adds rim light and colour bleed from the mesh glow (SVG filters and
  masks), haze on the back row, coloured contact shadows, expressions (pill, happy arcs, wink, wide eyes,
  blush) and the animation described below.

## Hero animation

The hero SVGs animate with plain CSS inside the file, which runs when the SVG is shown through `<img>`
(GitHub, browsers) with no script. Only `transform` and `opacity` are animated, and only on plain groups
(never on filtered elements). 66 animations in total:

- per character: float (4.3 to 6.4 s, 3 to 6 px) with a matching shadow scale and fade, breathing scale,
  eye blink every 3.9 to 5.4 s, and an occasional glance left or right (6.6 to 8.2 s);
- the approval card bobs slowly; about 20 dust grains drift and twinkle.

Delays are negative and fixed, so the first frame is already a finished composition. With
`prefers-reduced-motion: reduce` every animation is switched off and the static base pose is shown.
The static background (mesh blur and grain filters) is repainted by the browser while the characters
move. In headless Edge with software rendering the page kept 60 fps, and on a GPU it held the display
refresh rate. A nested pre-rendered background image was tried and gave no gain, so it is not used.
- `lib/wordmark.mjs`: the "Praxi" wordmark. Manrope Bold with explicit per-glyph positions, so it is
  laid out identically in every browser, and a dotless i topped with a terracotta clay sphere. The advances were
  shaped once with HarfBuzz at `wght=700`.
- `lib/bg.mjs`: canvas background, glass cards, icon tiles, beads.
- `scenes/*.mjs`: one file per artwork. Copy lives in these files; the architecture file lists the
  crates each box corresponds to.
- `fonts/manrope-latin.woff2`: Manrope 4.505 (SIL OFL 1.1, see `fonts/OFL.txt`), a 17 KB Latin subset
  with the Turkish letters, embedded in every SVG as base64 WOFF2 so text stays live and renders the
  same inside GitHub's image proxy, where external fonts cannot load. The subset recipe is in the
  profile repository's `assets/fonts/README.md` (variable weight 400-800, Latin plus Turkish).
- `lib/icons-data.mjs`: vendored icon data, generated by `extract-icons.mjs` from
  [lucide-static](https://lucide.dev) (ISC) for the UI icons and
  [simple-icons](https://simpleicons.org) (CC0-1.0) for brand marks (Tauri, Telegram, Ollama, MCP). Brand
  marks remain the property of their owners and only name the integration. To change the icon list:

  ```sh
  mkdir /tmp/icons && cd /tmp/icons && npm i lucide-static simple-icons
  node <repo>/scripts/readme-assets/extract-icons.mjs /tmp/icons/node_modules
  ```

## Rules for the artwork

- Text is live text using the embedded font. No external references, scripts or fonts, and each file
  stays well under 300 KB.
- Every claim comes from `docs/guide/` or the code (see the header of `scenes/architecture.mjs`). Anything
  experimental is labelled as such. Do not add features that are not shipped.
- Icons are real icons (lucide or simple-icons), never emoji, never generated images.
- Colors come from the Praxi themes: Obsidian `#111217`, Clay `#F4F1ED`, warm gray `#D8D3CC`,
  terracotta `#C86B4A`, amber-brown `#8A4E2A`, cream `#FDEFD6`, text `#1A1A1A`.

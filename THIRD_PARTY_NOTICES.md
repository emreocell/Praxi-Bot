# Third-party notices

## Geist Sans and Geist Mono (SIL OFL 1.1)

Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font).
Bundled locally through `@fontsource-variable/geist` and
`@fontsource-variable/geist-mono` 5.3.0. Variable WOFF2 files include Latin and
Latin Extended (Turkish) glyphs. No font CDN is used. Full notices ship in
`apps/desktop/ui/public/licenses/geist.txt` and `geist-mono.txt`.

## Lucide React (ISC; Feather-derived icons MIT)

`lucide-react` 1.70.0 supplies the desktop UI icons. Copyright 2026 Lucide Icons
and Contributors. Some icons derive from Feather, copyright 2013-present Cole
Bemis (MIT). The complete ISC/MIT notices ship in
`apps/desktop/ui/public/licenses/lucide-react.txt`.

## Simple Icons (CC0 1.0 Universal)

`simple-icons` 16.36.0 supplies bundled monochrome brand paths for GitHub,
GitLab, Notion, Google Drive, PostgreSQL, SQLite, Brave, Puppeteer, Sentry,
Linear, Todoist, Obsidian and Home Assistant. Other entries use a neutral
Lucide glyph. Logos are used to identify their services; brand trademarks
remain with their respective owners. Full CC0 dedication ships in
`apps/desktop/ui/public/licenses/simple-icons.txt`.

## OpenMausBot (Apache-2.0)

`apps/desktop/ui` settings and apps layout ideas are inspired by
[OpenMausBot](https://github.com/felipebraz/OpenMausBot) (`src/` layout, modal
settings with section nav, connected MCP apps). No source was copied from the
`enterprise/` tree. Apache License, Version 2.0
(http://www.apache.org/licenses/LICENSE-2.0).

## vercel-labs/agent-browser (Apache-2.0)

`crates/browser-cdp` adapts ideas and portions of code from
[agent-browser](https://github.com/vercel-labs/agent-browser)
(`cli/src/native/cdp/client.rs`, `snapshot.rs`, `diff.rs`, `cdp/chrome.rs`): the raw CDP client over
tokio-tungstenite, accessibility-tree snapshots with `@e` refs, and line-based snapshot diffs.
Licensed under the Apache License, Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0).
Adapted files carry the header `// Adapted from vercel-labs/agent-browser (Apache-2.0)`.

## browser-harness (MIT)

The "click at the box-model center" interaction approach in `crates/browser-cdp/src/actions.rs`
follows the technique used by browser-harness (MIT License).

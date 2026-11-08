# Third-party notices

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

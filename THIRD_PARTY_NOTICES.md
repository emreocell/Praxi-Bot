# Third-party notices

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

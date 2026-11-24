# Third-party notices

## proptest (MIT OR Apache-2.0)

`proptest` supplies development-only property tests for the scheduler's text parser.
The upstream crate includes both license texts:
[proptest](https://github.com/proptest-rs/proptest).

## Geist Sans and Geist Mono (SIL OFL 1.1)

Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font).
Bundled locally through `@fontsource-variable/geist` and
`@fontsource-variable/geist-mono` 5.3.0. Variable WOFF2 files include Latin and
Latin Extended (Turkish) glyphs. No font CDN is used. Full notices ship in
`apps/desktop/ui/public/licenses/geist.txt` and `geist-mono.txt`.

## OpenDyslexic (SIL OFL 1.1)

Copyright (c) 2019-07-29, Abbie Gonzalez (https://abbiecod.es), with Reserved Font
Name OpenDyslexic. Bundled locally through `@fontsource/opendyslexic` 5.3.0
(Latin WOFF2/WOFF, regular and bold) and used only when the owner turns on
Settings > Appearance > Accessibility > Dyslexia-friendly font. No font CDN is
used. The full notice ships in `apps/desktop/ui/public/licenses/opendyslexic.txt`.

## Lucide React (ISC; Feather-derived icons MIT)

`lucide-react` 1.70.0 supplies the desktop UI icons. Copyright 2026 Lucide Icons
and Contributors. Some icons derive from Feather, copyright 2013-present Cole
Bemis (MIT). The complete ISC/MIT notices ship in
`apps/desktop/ui/public/licenses/lucide-react.txt`.

## Simple Icons (CC0 1.0 Universal)

`simple-icons` 16.36.0 supplies bundled brand paths and official hex colors for GitHub,
GitLab, Notion, Google Drive, PostgreSQL, SQLite, Brave, Puppeteer, Sentry,
Linear, Todoist, Obsidian and Home Assistant. Additional paths include Gmail, Google Calendar, Discord, Figma, Anthropic,
Google Gemini, Ollama and Claude. Unknown integrations receive a deterministic
colored monogram; Filesystem uses a Lucide folder. The Apps grid adds the simple-icons marks (monochrome, tinted
with each brand's official colour) of about sixty popular apps (Stripe, Atlassian, Jira, Trello, Asana, HubSpot,
Shopify, Dropbox, Zoom, X, Telegram, WhatsApp, Spotify, Airtable, Zendesk, Docker, Vercel, Supabase, Google
Sheets/Docs/Meet/Maps and others); apps without a bundled mark show the logo Composio publishes (fetched,
validated and cached by the daemon) or a neutral glyph. Logos are used to identify their services; brand trademarks
remain with their respective owners. Full CC0 dedication ships in
`apps/desktop/ui/public/licenses/simple-icons.txt`.

The engine/provider registry also uses the CC0 marks for Cursor, OpenCode,
OpenRouter, DeepSeek, Mistral AI, LM Studio and GitHub Copilot. OpenAI's
blossom is retained from Simple Icons 15.0.0 (CC0), as detailed below.
See [the dated artwork audit](docs/design/brand-icons.md) for source URLs,
aliases, and brands deliberately represented by neutral initials.

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

## moltis importers (MIT)

`apps/cli/src/import_cmd.rs` (`bot import`) adapts the detection paths and the
"what maps to what" tables of the `import-core`, `claude-import`, `openclaw-import` and
`hermes-import` crates of [moltis](https://github.com/moltis-org/moltis), rewritten against
Praxi-Bot's `McpServerConfig`, secret store and skill hub (no moltis code is linked or copied
verbatim). Copyright (c) 2025 Fabien Penso. Licensed under the MIT License:

```text
Permission is hereby granted, free of charge, to any person obtaining a copy of this software
and associated documentation files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following conditions: the above copyright
notice and this permission notice shall be included in all copies or substantial portions of
the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.
```

## sherpa-onnx (Apache-2.0) and on-device speech models

`apps/cli/src/voice_local_engine.rs` (cargo feature `local-voice`) uses the official `sherpa-onnx` crate and
adapts the Silero-VAD and Whisper calls from its Rust examples
(`rust-api-examples/examples/silero_vad_remove_silence.rs`, Copyright (c) 2026 Xiaomi Corporation, and
`whisper.rs`, Copyright (c) 2026 Yujie Wu; <https://github.com/k2-fsa/sherpa-onnx>), licensed under the
Apache License, Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0). Changes: the two stages are chained,
the recogniser is cached, segments are capped at 25 s. Voice notes are decoded with the pure-Rust
`opus-decoder` (MIT OR Apache-2.0) and `ogg` (BSD-3-Clause) crates.

Model weights are downloaded on demand (never bundled) and their licence is shown before the download:
OpenAI Whisper (MIT, <https://github.com/openai/whisper>), converted to ONNX by the sherpa-onnx project;
Silero VAD (MIT, <https://github.com/snakers4/silero-vad>). Piper (GPL-3.0) is deliberately not used, and
NVIDIA Parakeet (CC-BY-4.0, attribution required) is not offered; if it is added later its attribution
belongs here and in the model picker.

## browser-harness (MIT)

The "click at the box-model center" interaction approach in `crates/browser-cdp/src/actions.rs`
follows the technique used by browser-harness (MIT License).

## clap_complete and clap_mangen (MIT OR Apache-2.0)

Praxi-Bot uses `clap_complete` to generate shell completion scripts and
`clap_mangen` to generate manual pages. Both are dual-licensed under the MIT
License or the Apache License, Version 2.0. The upstream source directories
contain both license texts.
[clap_complete](https://github.com/clap-rs/clap/tree/master/clap_complete),
[clap_mangen](https://github.com/clap-rs/clap/tree/master/clap_mangen)

## Integration identification artwork (v4)

Google Antigravity's one-colour icon comes from the vendor's official
[press assets](https://antigravity.google/press), specifically
`https://antigravity.google/assets/image/brand/antigravity-icon__one-color.svg`
(checked 2026-11-23). It is rescaled from a 180 to a 24 unit viewBox without
redrawing and bundled inline for integration identification under Google's
[product compatibility guidance](https://partnermarketinghub.withgoogle.com/brands/google/use-cases/product-co-branding/).
This is vendor trademark artwork, not CC0. Google Antigravity is a trademark
of Google LLC. No affiliation or endorsement is implied. Source/provenance:
`apps/desktop/ui/src/assets/brands/antigravity.svg` and the dated audit above.

Neutral initials for Groq and Together AI contain no vendor artwork. Groq is
a trademark of Groq LLC and/or its affiliates. Praxi-Bot is not affiliated
with or endorsed by Groq LLC.

OpenAI's blossom path is vendored from Simple Icons 15.0.0 (CC0):
https://github.com/simple-icons/simple-icons/blob/15.0.0/icons/openai.svg
The path in `src/lib/brandIcons.ts` is the runtime source; the source SVG is kept
under `src/assets/brands/` for provenance.

Local multicolor SVG reconstructions in `public/brands/` and the Slack SVG in
`BrandIcon.tsx` depict the Gmail, Google Drive, Google Calendar, Figma and Slack
identification marks. Reference owners and brand guidance:
- Google: https://about.google/brand-resource-center/
- Figma: https://www.figma.com/using-the-figma-brand/
- Slack: https://slack.com/media-kit

These brand logos are trademarks of their respective owners, used only to
identify integrations and model providers. No endorsement is implied. The
original geometric Praxi-Bot placeholders are project artwork, with no
OpenMausBot branding, names or character artwork copied.

## MCP Bundles manifest format and MCP Registry API (Apache-2.0 / MIT)

`crates/mcp-bridge/src/mcpb.rs` reads `.mcpb` bundles following the manifest
specification v0.3 of [modelcontextprotocol/mcpb](https://github.com/modelcontextprotocol/mcpb),
and `crates/mcp-bridge/src/registry.rs` reads the public API and `server.json`
format of the [official MCP Registry](https://github.com/modelcontextprotocol/registry).
Both upstream repositories state "Apache-2.0 for new code, MIT for older
contributions" in their `LICENSE` files (Copyright the Anthropic, PBC and the
Model Context Protocol a Series of LF Projects, LLC contributors; see each
repository's `LICENSE` for the exact text). Praxi's modules are independent
Rust implementations of the published formats; no upstream source code was
copied, only the documented field names and semantics are used.

## Hermes Agent starter skills (MIT)

The bundled skills `document-to-action-items`, `meeting-action-items`,
`weekly-review-planning`, `email-inbox-triage`, `product-price-monitor`,
`competitor-news-monitor`, `grounded-citations`, `blocked-page-recovery` and
`maps` in `skills/` are adapted from the `skills/` folder of
[NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent).
Their `author:` lines keep the upstream authors (Ben Barclay, Hermes Agent,
Teknium, Mibayy). Changes: rewritten for Praxi's tools and approval rules, plain
wording, English and Turkish trigger keywords; the upstream helper scripts
(`maps_client.py`, `sources.py`, `recover_page.py`) are not included.

    MIT License

    Copyright (c) 2025 Nous Research

    Permission is hereby granted, free of charge, to any person obtaining a copy
    of this software and associated documentation files (the "Software"), to deal
    in the Software without restriction, including without limitation the rights
    to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    copies of the Software, and to permit persons to whom the Software is
    furnished to do so, subject to the following conditions:

    The above copyright notice and this permission notice shall be included in all
    copies or substantial portions of the Software.

    THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
    SOFTWARE.

## gitleaks rule table (MIT)

`crates/secret-store/src/rules.rs` is generated by `scripts/tools/gen_secret_rules.py`
from the default `config/gitleaks.toml` of [gitleaks](https://github.com/gitleaks/gitleaks)
(commit 5f6ec56cfa6f8c67648d863582c7ae2f206d5c1c): rule ids, descriptions, regexes,
keywords and entropy thresholds. Praxi-Bot dropped path-gated rules and
`generic-api-key`, and converted the TOML into a Rust table. The matcher in
`crates/secret-store/src/scan.rs` is our own code. Licence text:

```text
MIT License

Copyright (c) 2019 Zachary Rice

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## llama.cpp (MIT) and Qwen3 GGUF models (Apache-2.0)

The one-click local model feature (`crates/provider-catalog/src/local_runtime`) downloads, at
the owner's request and never bundled in the installer, a pinned `llama-server` build from
[ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) release `b12633` (MIT licence,
Copyright (c) 2023-2026 The ggml authors) and a Qwen3 GGUF model from
[unsloth](https://huggingface.co/unsloth) quantisations of Alibaba's Qwen3 (Apache-2.0).
Each file is verified against a SHA-256 pinned in `manifest.rs`. No upstream source code is
copied into this repository; the licence of each model is shown in the picker.

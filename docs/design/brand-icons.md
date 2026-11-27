# Brand icon audit

Checked **2026-11-23**, extended 2026-11-27 (issue #701). Marks identify integrations, without implying endorsement.
No installed binary, ICO or app resource is redistributed. Engine/provider
artwork is bundled inline, with no network request or new dependency.

## UI coverage

Under `apps/desktop/ui`, `lib/brandIcons.ts` supplies both `BrandMark` (bare)
and `BrandIcon` (bare or tiled). The audit covered these lookup boundaries:

| Surface | Call sites / lookup |
| --- | --- |
| Engine selection and onboarding | `firstrun/BrainStep`: `brandOf` → `BrandIcon` |
| Settings → Bots and models | `settings/DetectedAccountsBlock`: `brandOf` → `BrandIcon` |
| Model picker: selected value, groups, rows, custom model | `ModelPicker`: `providerBrand` / `productBrandKey` → `BrandMark` |
| Onboarding provider presets and selected model | `BrainStep`: provider id / `productBrandKey` → `BrandIcon` / `BrandMark` |
| Provider key presets and connection rows | `settings/ApiKeyPaste`: raw provider id → `BrandIcon` |
| Bot editor and side panel model summary | `BotEditorFields`, `BotPanel`: `modelName` → `BrandMark` |
| Account quota indicators (mock-only currently) | `QuotaRings` → `BrandIcon` |
| Apps/catalog, connected apps, connection sheet | `AppsModal`, `AppsGrid`, `ConnectSheet` → `BrandIcon` / `AppLogo` → `BrandMark` |
| App rail, bot profile, permission rows | `AppsPill`, `BotProfileModal`, `BotEditorFields` → `BrandMark` |
| Tool activity and delegated runs | `StepIcon`, `SubagentList`, `ThinkingChain` consume registry paths; `ChatCards`, `ChatView`, `ShellBridgeHost`, `runSentence` consume brand recognition |

Engine/provider marks and explicit initials follow theme text via `currentColor`
on bare marks and tiles, covering Obsidian, Cobalt, Raven and Clay (and custom
theme text colours). Wrappers expose `role="img"` and the brand's accessible
name; controls retain their text labels. Other app marks retain their colours.
No translated UI prose was added.

## Engine and provider inventory

All rows dated **2026-11-23**. **SI** means the corresponding SVG in
[Simple Icons 16.36.0](https://github.com/simple-icons/simple-icons/tree/16.36.0/icons),
licensed [CC0 1.0](https://github.com/simple-icons/simple-icons/blob/16.36.0/LICENSE.md).
The slug in each row identifies its exact source file. CC0 covers artwork,
not trademark ownership.

| Brand / IDs | Before | After / source | Licence |
| --- | --- | --- | --- |
| Claude Code: `claude`, `cli:claude`, legacy `claude-code` | Real Claude starburst | SI `claude.svg`, retained | CC0 1.0 |
| Codex / OpenAI: `codex`, `cli:codex`, `openai`, `chatgpt-plan` | Real OpenAI blossom | [SI 15.0.0/openai.svg](https://github.com/simple-icons/simple-icons/blob/15.0.0/icons/openai.svg), retained | CC0 1.0 |
| Cursor: `cursor`, `cli:cursor`, `cursor-agent` | Real engine/company mark; raw CLI/command IDs fell back | SI `cursor.svg`; aliases fixed | CC0 1.0 |
| OpenCode: `opencode`, `cli:opencode` | Real mark | SI `opencode.svg`, retained | CC0 1.0 |
| Gemini: `gemini`, `gemini-cli`, `cli:gemini`, `gemini-openai-compat` | Real mark; raw CLI/compat IDs fell back | SI `googlegemini.svg`; aliases fixed | CC0 1.0 |
| Antigravity: `agy`, `cli:agy`, `antigravity` | Hand-drawn approximation labelled real | [Official one-colour SVG](https://antigravity.google/assets/image/brand/antigravity-icon__one-color.svg) from [press assets](https://antigravity.google/press) | Vendor compatibility use, below |
| Qwen Code: `qwen`, `cli:qwen` | Not listed | SI `qwen.svg` | CC0 1.0 |
| Kiro CLI: `kiro`, `cli:kiro`, `kiro-cli` | Original neutral terminal-prompt glyph | Official Kiro ghost, from the vendor's own site icon [kiro.dev/icon.svg](https://kiro.dev/icon.svg) (retrieved 2026-11-26): the white ghost silhouette with its eyes cut out, one monochrome path in a 24-box (coordinates rescaled from 1200, geometry unchanged), without the purple tile. Provenance file `src/assets/brands/kiro.svg` | No licence stated; see "Vendor-published icons" below |
| GitHub Copilot CLI: `copilot`, `cli:copilot` | Not listed | SI `githubcopilot.svg`, already bundled | CC0 1.0 |
| Qoder CLI: `qoder`, `cli:qoder`, `qodercli` | The same neutral terminal-prompt glyph as Kiro | Official Qoder mark, from the vendor's own site icon [qoder.com/favIcon.svg](https://qoder.com/favIcon.svg) (retrieved 2026-11-26): the outline mark without its grey tile, stroke expanded to a filled shape and flattened to one monochrome path in a 24-box (maximum deviation 0.03 of 24). Provenance file `src/assets/brands/qoder.svg` | No licence stated; see "Vendor-published icons" below |
| Kimi Code CLI: `kimi`, `cli:kimi` | Not listed | SI `kimi.svg` | CC0 1.0 |
| Mistral Vibe: `vibe`, `cli:vibe`, `vibe-acp` | Not listed | SI `mistralai.svg`, already bundled (Vibe is Mistral AI's own CLI) | CC0 1.0 |
| Goose: `goose`, `cli:goose` | Not listed | Original neutral terminal-prompt glyph (the generic fallback glyph also named in "Vendor-published icons"), not the Goose mark | Absent from SI 16.38.1. Goose is an LF Projects series: [trademark policy](https://lfprojects.org/policies/trademark-policy/) allows fair use of word marks only and requires written permission for logos in other products; no grant found (checked 2026-11-27) |
| Anthropic: `anthropic` | Real mark | SI `anthropic.svg`, retained | CC0 1.0 |
| DeepSeek: `deepseek` | Real mark | SI `deepseek.svg`, retained | CC0 1.0 |
| LM Studio: `lmstudio` | Real mark | SI `lmstudio.svg`, retained | CC0 1.0 |
| Ollama: `ollama` | Real mark | SI `ollama.svg`, retained | CC0 1.0 |
| OpenRouter: `openrouter` | Real mark | SI `openrouter.svg`, retained | CC0 1.0 |
| Mistral: `mistral`, `mistralai` | Raw provider fell back; maker prefix had real mark | SI `mistralai.svg`; alias fixed | CC0 1.0 |
| Groq: `groq` | Generic two-letter fallback | Neutral **G**; [trademark policy](https://groq.com/trademark-policy) (effective 2025-10-24, re-read 2026-11-27) | Nominative use is "limited to word marks (no logos)"; "Use Groq logos in ads, packaging, UI, or signage without a license from Groq" is prohibited. Brand guidelines only on request to trademark-legal@groq.com |
| Together AI: `together`, `Together AI` | Generic two-letter fallback | Neutral **T**; [brand](https://www.together.ai/brand), [terms](https://www.together.ai/terms-of-service) (re-read 2026-11-27) | The brand page offers a logo package with no usage terms; the terms say "You may not use Company name, logo, or trademarks without the Company's prior written consent" |
| llama.cpp: `llamacpp` (detected local server) | Generic two-letter fallback | Official llama.cpp icon from [ggml-org/llama-brand](https://github.com/ggml-org/llama-brand) (`logo/svg/icon/icon-light.svg`, commit `9e91eb16`, retrieved 2026-11-27), one monochrome path in a 24-box (rescaled from 600, geometry unchanged), drawn in the theme text colour. Provenance file `src/assets/brands/llamacpp.svg` | Marks: CC BY-NC 4.0 plus the written grant in [BRAND-USAGE.md](https://github.com/ggml-org/llama-brand/blob/master/BRAND-USAGE.md): everyone may reproduce and redistribute them, commercially and in software, to identify or refer to llama.cpp, including rescaling and monochrome rendering. Attribution: this document and `THIRD_PARTY_NOTICES.md` name the repository |

Antigravity is absent from the installed SI release. Google's
[compatibility guidance](https://partnermarketinghub.withgoogle.com/brands/google/use-cases/product-co-branding/)
allows necessary product icons in a compatible-products grid or product UI,
with context and without implying Google authorship. This integration list
shows the vendor's one-colour icon next to the Antigravity name. Coordinates
and viewBox are rescaled from 180 to 24; geometry is unchanged. The provenance
SVG is `src/assets/brands/antigravity.svg`; its path is inlined by the registry.
Google Antigravity is a trademark of Google LLC.

### Vendor-published icons (Kiro, Qoder)

Neither product is in Simple Icons (checked in 16.38.1, the latest release on
2026-11-26: no `kiro` or `qoder` file), and neither vendor publishes a brand or
press kit that could be found (`kiro.dev/brand`, `/press`, `/media-kit`,
`/brand-guidelines` and `qoder.com/brand`, `/press`, `/media-kit` answer 404).
The marks therefore come from the icon each vendor serves for its own product
site. No licence or brand-use terms accompany those files, so this is
nominative use only: the mark identifies the vendor's CLI in the engine list and
model picker, next to its name, and implies no endorsement. Kiro and Qoder are
trademarks of their owners. If a vendor objects, or publishes a kit with
different rules, replace the path in `lib/brandIcons.ts` (and the file under
`src/assets/brands/`) with the generic glyph `M3.5 6.5 5 5l7 7-7 7-1.5-1.5L9
12ZM12 17h8.5v2H12Z`. This is the one engine-logo decision that needs the
owner's confirmation (see the pull request).

Read-only installed-app inspection: Antigravity IDE's SVG agrees with the press
mark; Codex's Windows app PNG shows the OpenAI blossom. Cursor's Start Menu
shortcut points to an absent executable; its installed CLI provides no brand
image found during this inspection. Cursor keeps its existing CC0 mark.
No app was started, authenticated or modified.

## Apps/catalog inventory

Real bundled SI marks (CC0 1.0, checked 2026-11-23; sources are the SVGs
in the versioned directory above) also include GitHub, GitLab, Notion,
Google Drive, Google Calendar, Gmail, Discord, Figma, Linear, PostgreSQL,
SQLite, Brave, Puppeteer, Sentry, Todoist, Obsidian, Home Assistant,
GitHub Copilot, Stripe, Atlassian, Jira, Confluence, Trello, Asana, HubSpot,
Shopify, Dropbox, Zoom, X, Facebook, Instagram, YouTube, Reddit, Telegram,
WhatsApp, Spotify, Airtable, ClickUp, Zendesk, Intercom, Mailchimp, Calendly,
Docker, Vercel, Netlify, Supabase, Cloudflare, Google Sheets, Google Docs,
Google Slides, Google Meet, Google Maps, Google Photos, Bitbucket, Datadog,
PagerDuty, MongoDB, Redis, Miro, Box, Evernote, PayPal, Square, QuickBooks,
Xero, WordPress, Medium, Ghost, Substack, Zapier, Make, n8n, Resend, Mixpanel,
PostHog, Firebase and DigitalOcean. Existing Google/Figma multicolour variants
and Slack artwork keep the vendor references in `THIRD_PARTY_NOTICES.md`.

The bundled MCP catalog resolves real marks for GitHub, GitLab, Slack, Notion,
Google Drive, PostgreSQL, SQLite, Brave Search, Sentry, Linear, Todoist,
Obsidian, Home Assistant, Stripe, Atlassian, Vercel, Figma, PostHog, Intercom,
PayPal and MongoDB. `notion-remote` aliases Notion. Filesystem and Fetch are
generic capabilities and retain neutral tool glyphs.

These branded catalog entries previously had generic fallbacks. They were first
given neutral initials; the vendors' brand pages were then read (2026-11-27, issue
#701). Four now show the vendor's own mark; Playwright keeps its initial:

| Brand | Result | Official source | Licence / terms |
| --- | --- | --- | --- |
| Tavily | Brand mark (rounded tile with the cut-out glyph), theme text colour. `src/assets/brands/tavily.svg` | [Brand guidelines](https://tavily.com/brand), file `/logos/tavily-mark-black.svg` (rescaled from 55 to 24) | Express permission: "Third parties may refer to Tavily assets to identify Tavily products or services (e.g. in compatibility statements)", if necessary to identify, with no endorsement implied and not more prominent than necessary. Use files as provided: black on light, off-white on dark, no recolouring outside the palette |
| Firecrawl | Flame symbol in Heat orange `#FA5D19`. `src/assets/brands/firecrawl.svg` | [Press & brand](https://www.firecrawl.dev/brand), `firecrawl-logo.svg` from the brand-assets zip (rescaled from 200x284 to 24) | Vendor brand kit ("Download our logos and follow the guidelines below"); no licence text, and the terms of service say nothing about logos. The only rule is not to stretch, recolour or modify the wordmark |
| Context7 | Standalone symbol, theme text colour. `src/assets/brands/context7.svg` | [Brand assets](https://context7.com/brand), `context7-symbol.svg` (four paths joined, rescaled to 24) | Vendor brand kit "for using Context7 in your own work"; no licence text. Context7 is an Upstash project |
| Exa | Logomark in Exa blue `#0143D9` (the file's fill). `src/assets/brands/exa.svg` | [Brand guidelines](https://exa.ai/brand), `Exa Logomark Blue.svg` from the brand-assets zip (rescaled from 151x182 to 24) | Guidelines written for "partners, developers, customers, and anyone building with Exa" to display the marks, with downloadable files and no licence text. Only co-branded materials need approval from both parties; this list only names the integration |
| Playwright | Initial **P** | [project](https://github.com/microsoft/playwright), [playwright.dev](https://playwright.dev) | Apache-2.0 covers the code; its section 6 grants no trademark rights, and the repository and site publish no brand kit or logo terms (Microsoft Corporation). No grant found |

The remote Composio catalog is open-ended, so it cannot be an exhaustive
bundled inventory. Its other apps retain the existing daemon-validated cached
logo / neutral glyph path. Engine/provider IDs and explicit initials always
resolve locally. Custom providers/servers keep the deterministic unknown fallback.

### Composio apps pinned in the Apps grid (checked 2026-11-27)

Every app of `crates/composio/src/curated.json` was looked up in Simple Icons 16.36.0
(CC0 1.0). TikTok (`siTiktok`) was the only one missing from the bundle and now uses it.

| Pinned app | Result | Why |
| --- | --- | --- |
| LinkedIn, OneDrive, Microsoft Outlook, Microsoft Teams, Salesforce | Composio catalog logo or initials | Simple Icons 16.36.0 has no matching marks. Praxi displays the validated catalog logo supplied by Composio when available, with initials as a fallback. |

`brandIcons.test.ts` names these five, so a new pinned app without a mark fails the test and
has to be added to this audit. Catalog apps that are not pinned follow the same rule.

## Licence register

Every artwork that is not a Simple Icons CC0 path, and every brand kept on a neutral
initial or glyph, has one row here. `brandIcons.test.ts` fails when `lib/brandIcons.ts`
or `src/assets/brands/` gains an entry without a row with a source URL, terms and a
check date. Simple Icons paths are CC0 1.0 (`simple-icons` ships its `LICENSE.md`;
the test reads it). **Result** is `logo` (artwork bundled) or `initial` / `glyph`
(no artwork, with the reason).

| Brand | Key | Result | Artwork file | Source | Licence / terms | Checked |
| --- | --- | --- | --- | --- | --- | --- |
| OpenAI | `openai` | logo | `openai.svg` | [SI 15.0.0 openai.svg](https://github.com/simple-icons/simple-icons/blob/15.0.0/icons/openai.svg) | CC0 1.0 | 2026-11-23 |
| Antigravity | `antigravity` | logo | `antigravity.svg` | [press assets](https://antigravity.google/press) | Vendor compatibility use ([Google guidance](https://partnermarketinghub.withgoogle.com/brands/google/use-cases/product-co-branding/)) | 2026-11-23 |
| Kiro CLI | `kiro` | logo | `kiro.svg` | [kiro.dev/icon.svg](https://kiro.dev/icon.svg) | No licence stated; nominative use, see "Vendor-published icons" | 2026-11-26 |
| Qoder CLI | `qoder` | logo | `qoder.svg` | [qoder.com/favIcon.svg](https://qoder.com/favIcon.svg) | No licence stated; nominative use, see "Vendor-published icons" | 2026-11-26 |
| llama.cpp | `llamacpp` | logo | `llamacpp.svg` | [ggml-org/llama-brand](https://github.com/ggml-org/llama-brand) | CC BY-NC 4.0 plus the commercial, monochrome-allowed [grant](https://github.com/ggml-org/llama-brand/blob/master/BRAND-USAGE.md) for referring to llama.cpp | 2026-11-27 |
| Tavily | `tavily` | logo | `tavily.svg` | [tavily.com/brand](https://tavily.com/brand) | Referential use in compatibility statements allowed; no endorsement implied | 2026-11-27 |
| Firecrawl | `firecrawl` | logo | `firecrawl.svg` | [firecrawl.dev/brand](https://www.firecrawl.dev/brand) | Vendor brand kit, no licence text; do not stretch, recolour or modify | 2026-11-27 |
| Context7 | `context7` | logo | `context7.svg` | [context7.com/brand](https://context7.com/brand) | Vendor brand kit "for using Context7 in your own work", no licence text | 2026-11-27 |
| Exa | `exa` | logo | `exa.svg` | [exa.ai/brand](https://exa.ai/brand) | Guidelines for anyone building with Exa, no licence text; co-branding needs approval | 2026-11-27 |
| Slack | `slack` | logo | inline in `BrandIcon.tsx` / `BrandMark.tsx` | [Slack media kit](https://slack.com/media-kit) | Identification of the integration, see `THIRD_PARTY_NOTICES.md` | 2026-11-23 |
| Goose | `goose` | glyph | none | [LF Projects trademark policy](https://lfprojects.org/policies/trademark-policy/) | Logos need written permission; the glyph is original project artwork | 2026-11-27 |
| Groq | `groq` | initial | none | [groq.com/trademark-policy](https://groq.com/trademark-policy) | Logos in a UI need a licence from Groq | 2026-11-27 |
| Together AI | `together` | initial | none | [together.ai/terms-of-service](https://www.together.ai/terms-of-service) | Logo use needs prior written consent | 2026-11-27 |
| Playwright | `playwright` | initial | none | [microsoft/playwright](https://github.com/microsoft/playwright) | No trademark or logo grant found | 2026-11-27 |

## Open maintainer note

Obtain written permission or a published brand grant for Groq (trademark-legal@groq.com),
Together AI, Playwright (Microsoft) and Goose (LF Projects) before replacing their initials
or neutral glyph. Kiro, Qoder, Firecrawl, Context7 and Exa rely on vendor-published marks
without licence text (see "Vendor-published icons" and the apps table): the owner should confirm that reading.
Tavily and llama.cpp have express grants. Keep this note open until each mark has a source
and licence.
Do not trace logos or extract installed-app artwork. Groq is a trademark of
Groq LLC and/or its affiliates. Praxi-Bot is not affiliated with or endorsed
by Groq LLC.

`brandIcons.test.ts` reads the actual CLI registry, provider TOMLs, offline
presets and local-server IDs. It checks raw IDs, company keys and model defaults,
the official Antigravity path, explicit initials, and the licence register. New IDs without artwork
fail instead of silently using an unknown fallback.

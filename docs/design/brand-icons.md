# Brand icon audit

Checked **2026-11-23**. Marks identify integrations, without implying endorsement.
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
| Kiro CLI: `kiro`, `cli:kiro`, `kiro-cli` | Not listed | Original neutral terminal-prompt glyph, not the Kiro mark | Absent from SI 16.38.1; no mark redistribution grant verified |
| GitHub Copilot CLI: `copilot`, `cli:copilot` | Not listed | SI `githubcopilot.svg`, already bundled | CC0 1.0 |
| Qoder CLI: `qoder`, `cli:qoder`, `qodercli` | Not listed | The same neutral terminal-prompt glyph as Kiro, not the Qoder mark | Absent from SI 16.38.1; no mark redistribution grant verified |
| Anthropic: `anthropic` | Real mark | SI `anthropic.svg`, retained | CC0 1.0 |
| DeepSeek: `deepseek` | Real mark | SI `deepseek.svg`, retained | CC0 1.0 |
| LM Studio: `lmstudio` | Real mark | SI `lmstudio.svg`, retained | CC0 1.0 |
| Ollama: `ollama` | Real mark | SI `ollama.svg`, retained | CC0 1.0 |
| OpenRouter: `openrouter` | Real mark | SI `openrouter.svg`, retained | CC0 1.0 |
| Mistral: `mistral`, `mistralai` | Raw provider fell back; maker prefix had real mark | SI `mistralai.svg`; alias fixed | CC0 1.0 |
| Groq: `groq` | Generic two-letter fallback | Neutral **G**; [trademark policy](https://groq.com/trademark-policy) | Policy limits nominative use to words; UI logos need permission |
| Together AI: `together`, `Together AI` | Generic two-letter fallback | Neutral **T**; [brand](https://www.together.ai/brand), [terms](https://www.together.ai/terms-of-service) | No clear grant; terms require written consent |
| llama.cpp: `llamacpp` (detected local server) | Generic two-letter fallback | Neutral **L**; [upstream](https://github.com/ggml-org/llama.cpp) | MIT code; no separate mark grant established |

Antigravity is absent from the installed SI release. Google's
[compatibility guidance](https://partnermarketinghub.withgoogle.com/brands/google/use-cases/product-co-branding/)
allows necessary product icons in a compatible-products grid or product UI,
with context and without implying Google authorship. This integration list
shows the vendor's one-colour icon next to the Antigravity name. Coordinates
and viewBox are rescaled from 180 to 24; geometry is unchanged. The provenance
SVG is `src/assets/brands/antigravity.svg`; its path is inlined by the registry.
Google Antigravity is a trademark of Google LLC.

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

These branded catalog entries previously had generic fallbacks. They now
have explicit neutral initials; no artwork is copied. All dated 2026-11-23:

| Brand | Initial | Official source / maintainer reference | Licence status |
| --- | --- | --- | --- |
| Playwright | P | [project](https://github.com/microsoft/playwright) | Absent from installed SI; no separate mark grant verified |
| Exa | E | [site](https://exa.ai) | No mark redistribution grant verified |
| Tavily | T | [site](https://tavily.com) | No mark redistribution grant verified |
| Firecrawl | F | [project](https://github.com/firecrawl/firecrawl) | Code licence does not establish a permissive logo grant |
| Context7 | C | [project](https://github.com/upstash/context7) | No separate mark grant verified |

The remote Composio catalog is open-ended, so it cannot be an exhaustive
bundled inventory. Its other apps retain the existing daemon-validated cached
logo / neutral glyph path. Engine/provider IDs and explicit initials always
resolve locally. Custom providers/servers keep the deterministic unknown fallback.

## Open maintainer note

Obtain explicit redistributable artwork or permission for Groq, Together AI,
llama.cpp, Playwright, Exa, Tavily, Firecrawl and Context7 before replacing
their initials, and for Kiro and Qoder before replacing their neutral glyph. Keep this note open until each mark has a source and licence.
Do not trace logos or extract installed-app artwork. Groq is a trademark of
Groq LLC and/or its affiliates. Praxi-Bot is not affiliated with or endorsed
by Groq LLC.

`brandIcons.test.ts` reads the actual CLI registry, provider TOMLs, offline
presets and local-server IDs. It checks raw IDs, company keys and model defaults,
the official Antigravity path, and explicit initials. New IDs without artwork
fail instead of silently using an unknown fallback.

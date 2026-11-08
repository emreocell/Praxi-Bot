# W5 — Praxi-Bot visual redesign

Date: 2026-11-08 · Branch: `feat/settings-ui`

Local `main` was merged first, without conflicts. The Settings modal, Apps/MCP modal, account menu, model picker, routes and existing settings behavior remain in place. No push was performed.

## Visual system

The desktop now uses locally bundled Geist Sans and Geist Mono, consistent Lucide icons, restrained borders, layered surfaces, theme-aware message bubbles and code blocks, and a floating composer. The sidebar has tinted bot avatars, status dots, quieter action rows and a compact account menu. Modal backdrops use an 8px blur and a separate dimming token. Empty states include contextual icons.

Typography uses 12/14/15/20/24px roles, with a 30px display token available. Spacing follows the 4/8px scale. Radii are 6px controls, 8px fields, 12px cards/avatars and 16px dialogs/composer. State transitions are 160–200ms; reduced motion disables them. No paper noise was added: Clay uses clean warm surfaces, preserving a calm appearance and avoiding a rendering filter.

## Themes and ADR 0022

JSON in `packages/ui/themes` is the source of truth. `pnpm --dir packages/ui generate:themes` emits the committed named `:root[data-theme=...]` selectors and miniature preview scopes into `packages/ui/tokens.css`. Component CSS consumes tokens exclusively. Existing `light` and `dark` saved settings resolve to Clay and Obsidian. New installations default to Obsidian. Follow system / Sistemi izle resolves OS dark to Obsidian and OS light to Clay, including live OS changes.

Theme selection is available in Settings > Genel > Tema and onboarding. Four miniature previews show each theme's actual surface, text and accent tokens. Selection applies immediately and persists through the existing Tauri/localStorage settings mechanism. Accent swatches continue to override the active theme, with reset support. Custom accents select a readable black/white foreground and fall back to theme text for links when the supplied color lacks contrast. Focus colors remain accessible independently of custom accents.

| Theme | Intent | Base | Primary text | Accent | Minimum text contrast across surfaces 0–3 |
|---|---|---|---|---|---|
| Obsidian | Default refined dark | #111217 | #F2F2F5 | #B6B8FA | 5.06:1 |
| Cobalt | OLED, minimal borders | #010102 | #FAFAFC | #9DBBFF | 6.95:1 |
| Raven | Warm editorial dark | #191613 | #FDEFD6 | #E4B683 | 5.91:1 |
| Clay | Warm off-white, terracotta | #F4F1ED | #1A1A1A | #C86B4A | 4.59:1 |

The attached `C:/praxi-ext-logs/clay-palette.webp` was viewed. Clay preserves #D8D3CC for soft surfaces/borders and #8A4E2A as the deep accent. Small link text uses #7C4525, a darker derivative: #8A4E2A measured only 4.40:1 on the deepest surface. The focus/control border was also darkened to #7D7066 to clear 3:1 on all surfaces.

### Complete per-theme token table

| Token | Obsidian | Cobalt | Raven | Clay |
|---|---|---|---|---|
| `--color-background` | `#111217` | `#010102` | `#191613` | `#F4F1ED` |
| `--color-surface` | `#18191F` | `#08090C` | `#211D19` | `#FAF8F5` |
| `--color-surface-elevated` | `#202127` | `#101116` | `#2B2520` | `#EAE5DF` |
| `--color-surface-active` | `#2A2B33` | `#1C1D24` | `#352D27` | `#D8D3CC` |
| `--color-text` | `#F2F2F5` | `#FAFAFC` | `#FDEFD6` | `#1A1A1A` |
| `--color-text-secondary` | `#B8B8C2` | `#C3C5D0` | `#D0BEA6` | `#554B42` |
| `--color-text-muted` | `#999AA8` | `#A3A6B7` | `#BBA990` | `#63594F` |
| `--color-accent` | `#B6B8FA` | `#9DBBFF` | `#E4B683` | `#C86B4A` |
| `--color-accent-contrast` | `#171824` | `#07152B` | `#24180D` | `#1A100B` |
| `--color-accent-text` | `#C4C5FF` | `#B7CDFF` | `#F0C99D` | `#7C4525` |
| `--color-border` | `#ffffff12` | `#ffffff0d` | `#fdefd617` | `#D8D3CC` |
| `--color-border-strong` | `#797B8C` | `#747A8C` | `#9D8973` | `#7D7066` |
| `--color-text-inverse` | `#171824` | `#07152B` | `#24180D` | `#1A100B` |
| `--color-accent-hover` | `color-mix(in srgb, #B6B8FA 88%, #ffffff)` | `color-mix(in srgb, #9DBBFF 88%, #ffffff)` | `color-mix(in srgb, #E4B683 88%, #ffffff)` | `#DB825F` |
| `--color-accent-subtle` | `color-mix(in srgb, #B6B8FA 12%, #18191F)` | `color-mix(in srgb, #9DBBFF 12%, #08090C)` | `color-mix(in srgb, #E4B683 12%, #211D19)` | `color-mix(in srgb, #C86B4A 12%, #FAF8F5)` |
| `--color-surface-overlay` | `color-mix(in srgb, #18191F 88%, transparent)` | `color-mix(in srgb, #08090C 88%, transparent)` | `color-mix(in srgb, #211D19 88%, transparent)` | `color-mix(in srgb, #FAF8F5 88%, transparent)` |
| `--color-backdrop-dim` | `#100E0C` | `#100E0C` | `#100E0C` | `#100E0C` |
| `--color-modal-backdrop` | `#00000099` | `#00000099` | `#00000099` | `#100e0c66` |
| `--color-success` | `#92CEAC` | `#92CEAC` | `#92CEAC` | `#25633F` |
| `--color-warning` | `#E8BE7C` | `#E8BE7C` | `#E8BE7C` | `#80500C` |
| `--color-danger` | `#F4A3A3` | `#F4A3A3` | `#F4A3A3` | `#A52D2D` |
| `--color-danger-contrast` | `#351111` | `#351111` | `#351111` | `#FFFFFF` |
| `--color-danger-subtle` | `color-mix(in srgb, var(--color-danger) 10%, #18191F)` | `color-mix(in srgb, var(--color-danger) 10%, #08090C)` | `color-mix(in srgb, var(--color-danger) 10%, #211D19)` | `color-mix(in srgb, var(--color-danger) 10%, #FAF8F5)` |
| `--color-warning-subtle` | `color-mix(in srgb, var(--color-warning) 10%, #18191F)` | `color-mix(in srgb, var(--color-warning) 10%, #08090C)` | `color-mix(in srgb, var(--color-warning) 10%, #211D19)` | `color-mix(in srgb, var(--color-warning) 10%, #FAF8F5)` |
| `--color-focus` | `#797B8C` | `#747A8C` | `#9D8973` | `#7D7066` |
| `--color-scrollbar` | `#797B8C` | `#747A8C` | `#9D8973` | `#7D7066` |
| `--color-selection` | `#2A2B33` | `#1C1D24` | `#352D27` | `#D8D3CC` |
| `--color-selection-text` | `#F2F2F5` | `#FAFAFC` | `#FDEFD6` | `#1A1A1A` |
| `--color-code-bg` | `#111217` | `#010102` | `#191613` | `#F4F1ED` |
| `--color-code-text` | `#F2F2F5` | `#FAFAFC` | `#FDEFD6` | `#1A1A1A` |
| `--color-code-keyword` | `#C4C5FF` | `#B7CDFF` | `#F0C99D` | `#7C4525` |
| `--color-code-string` | `#92CEAC` | `#92CEAC` | `#92CEAC` | `#25633F` |
| `--color-code-number` | `#E8BE7C` | `#E8BE7C` | `#E8BE7C` | `#80500C` |
| `--color-code-comment` | `#999AA8` | `#A3A6B7` | `#BBA990` | `#63594F` |
| `--color-overlay-target-bg` | `color-mix(in srgb, #B6B8FA 16%, transparent)` | `color-mix(in srgb, #9DBBFF 16%, transparent)` | `color-mix(in srgb, #E4B683 16%, transparent)` | `color-mix(in srgb, #C86B4A 16%, transparent)` |
| `--color-overlay-ripple` | `color-mix(in srgb, #B6B8FA 48%, transparent)` | `color-mix(in srgb, #9DBBFF 48%, transparent)` | `color-mix(in srgb, #E4B683 48%, transparent)` | `color-mix(in srgb, #C86B4A 48%, transparent)` |
| `--color-overlay-human-bg` | `var(--color-warning-subtle)` | `var(--color-warning-subtle)` | `var(--color-warning-subtle)` | `var(--color-warning-subtle)` |
| `--color-accent-deep` | `#C4C5FF` | `#B7CDFF` | `#F0C99D` | `#8A4E2A` |
| `--shadow-sm` | `0 1px 2px #00000030` | `0 1px 2px #00000030` | `0 1px 2px #00000030` | `0 1px 2px #100e0c0a` |
| `--shadow-card` | `0 4px 16px #00000018` | `0 4px 16px #00000018` | `0 4px 16px #00000018` | `0 4px 16px #100e0c08` |
| `--shadow-overlay` | `0 32px 100px #00000080, 0 0 0 1px #ffffff04` | `0 32px 100px #00000080, 0 0 0 1px #ffffff04` | `0 32px 100px #00000080, 0 0 0 1px #ffffff04` | `0 24px 80px #100e0c30, 0 2px 8px #100e0c14` |
| `--shadow-focus` | `0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)` | `0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)` | `0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)` | `0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)` |
| `--shadow-target` | `0 0 0 4px var(--color-accent-subtle)` | `0 0 0 4px var(--color-accent-subtle)` | `0 0 0 4px var(--color-accent-subtle)` | `0 0 0 4px var(--color-accent-subtle)` |
| `--shadow-human-banner` | `0 0 0 3px var(--color-warning-subtle)` | `0 0 0 3px var(--color-warning-subtle)` | `0 0 0 3px var(--color-warning-subtle)` | `0 0 0 3px var(--color-warning-subtle)` |

## Icons and licenses

- Lucide React 1.70.0: ISC, with the bundled MIT notices for Feather-derived icons. Common strokes are 1.5; sizes are 16/18/20px. Navigation, sidebar actions, model/account menus, chat controls, copy actions, schedule/bot actions and empty states share the set.
- Simple Icons 16.36.0: CC0 1.0. Named imports include GitHub, GitLab, Notion, Google Drive, PostgreSQL, SQLite, Brave, Puppeteer, Sentry, Linear, Todoist, Obsidian and Home Assistant. The logos are monochrome for contrast in every theme. Unavailable brands (including Slack in this package version) and unknown MCP servers use a neutral Lucide glyph. Filesystem uses FolderOpen. Brand trademarks belong to their owners.
- Geist Sans / Geist Mono via Fontsource Variable 5.3.0: SIL OFL 1.1. Latin Extended includes Turkish glyphs. WOFF2 assets are bundled by Vite; no external CDN or runtime logo request is required.
- `THIRD_PARTY_NOTICES.md` was updated. Complete license texts are shipped under `apps/desktop/ui/public/licenses/`, including the upstream copyright statements.

## Accessibility and behavior verification

The theme check validates all 44 tokens per theme, generated CSS parity, normal text >=4.5:1 across all four surfaces, accent/danger button text, accent hover, status badge backgrounds, code syntax and focus rings >=3:1. Muted decorative separators intentionally remain subtle; focus/control boundaries use the stronger token. Disabled controls retain their disabled appearance and are excluded from contrast claims.

Browser interaction checks passed 19 assertions: all four themes update live and persist; local Geist loads; accent overrides apply; forward and reverse Tab stay inside dialogs; the background becomes inert and is restored; Escape closes dialogs; account/model menus support arrow keys and restore trigger focus; English settings render; system appearance changes update live; reduced motion disables dialog animation; no runtime exceptions were observed. Dialog focus no longer resets when a settings update changes the onClose callback identity. Icon-only controls retain accessible labels. UI translation key parity is preserved.

## Validation results

- `pnpm typecheck`: passed.
- `pnpm lint`: passed (ESLint and token-enforcing Stylelint).
- `pnpm --dir apps/desktop/ui test`: 105 passed, 0 failed.
- `pnpm --dir apps/desktop/ui check:i18n`: passed, 658 keys.
- `pnpm --dir apps/desktop/ui check:themes`: all four themes passed.
- `pnpm --dir apps/desktop/ui build`: passed; font files and license notices are local build assets. Vite reported its >500kB main chunk advisory (about 636kB minified / 197kB gzip), not a build failure.
- Chrome/CDP interaction check: 19 passed; evidence in `C:/praxi-ext-logs/v3-interaction-results.json`.
- Native Tauri binary was not launched; validation used the requested Vite mock mode in desktop Chrome. Window decoration/settings bridge behavior is retained, and no Rust/Tauri configuration was changed.

## Screenshot reproduction and visual review

Start the requested server:

```powershell
pnpm --dir apps/desktop/ui exec vite --port 5181 --strictPort
```

Then capture with the committed helper (native headless Chrome, 1440×900, 5s virtual time budget):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/ui/scripts/capture-design.ps1 -OutputDirectory C:/Users/emreo/OneDrive/Masaüstü/emre/bot/design/v3
```

The review hooks apply only in mock mode: `?mock=1&theme=obsidian&screen=chat`, `screen=settings`, or `screen=apps`; use `lang=en` for English. The default mock review is Turkish. Mock chat fixtures exercise paragraphs, emphasis, lists and a copyable code block without a daemon. Production routes do not respond to these review hooks unless explicitly built with VITE_MOCK.

All 12 final PNGs were opened with the image viewer and checked individually. Iteration removed clipped first-turn content from the mock conversation, refined the composer/model control, corrected badge icon alignment, and fixed Clay contrast. Theme previews, connected/error states, brand logos and scroll boundaries were inspected in each modal. Final images are 1440×900.

Screenshot directory: `C:/Users/emreo/OneDrive/Masaüstü/emre/bot/design/v3/`

| Theme | Screen | File | SHA-256 (first 16 characters) |
|---|---|---|---|
| obsidian | chat | `obsidian-chat.png` | `75ed67641af2fb49` |
| obsidian | settings | `obsidian-settings.png` | `b9ada145dfbc9fc5` |
| obsidian | apps | `obsidian-apps.png` | `095f94f68c1763a7` |
| cobalt | chat | `cobalt-chat.png` | `84a36bd3fc758994` |
| cobalt | settings | `cobalt-settings.png` | `5fb527213d0086ee` |
| cobalt | apps | `cobalt-apps.png` | `435ea33015dd3976` |
| raven | chat | `raven-chat.png` | `1aa87d167f88a1e9` |
| raven | settings | `raven-settings.png` | `74c8309e242ee090` |
| raven | apps | `raven-apps.png` | `702ada7bfac05d26` |
| clay | chat | `clay-chat.png` | `1374db85d5c6796f` |
| clay | settings | `clay-settings.png` | `f24645ecff588839` |
| clay | apps | `clay-apps.png` | `1b245cda94fe9967` |

This report is saved both in the requested external `tasks/W5-redesign-report.md` location and in `docs/design/W5-redesign-report.md` for repository review. Screenshot artifacts remain in the requested external design directory.

# Adding a language to the desktop app

The desktop UI is translated with `i18next`. Today it ships **English** (`en`, the fallback) and
**Turkish** (`tr`). A new language is a good first contribution: it is mostly translation work, and
the checks tell you what is missing.

## Steps

All paths are under `apps/desktop/ui/src/`.

1. **Copy the English file.** `cp i18n/locales/en.json i18n/locales/<code>.json` (use the
   [ISO 639-1](https://en.wikipedia.org/wiki/List_of_ISO_639-1_codes) code, for example `de`).
   Translate the *values*, never the keys. Keep placeholders such as `{{name}}` and `{{count}}`
   exactly as they are; you may move them inside the sentence.
2. **Register it** in `i18n/index.ts`: import the file, add it to `resources`, and extend
   `SupportedLanguage` (`"en" | "tr" | "<code>"`).
3. **Offer it in the pickers.**
   - First-run wizard: add the code to `LANGUAGES` in `views/OnboardingView.tsx`.
   - Settings: add an option in `views/settings/GeneralCards.tsx` and the language's own name
     (written in that language) as `settings.language.<code>` in **every** locale file.
4. **Dates and clocks.** `lib/messageTime.ts` formats times for the active language; add your locale
   there if it needs a specific hour cycle.
5. **Check.** From the repository root:
   ```sh
   pnpm --dir apps/desktop/ui check:i18n   # same keys in every locale, every t("...") key exists
   pnpm --dir apps/desktop/ui typecheck
   pnpm --dir apps/desktop/ui test
   ```
   `check:i18n` currently compares `en.json` with `tr.json`; extend `scripts/check-i18n.mjs` so it
   compares every file in `i18n/locales/` (this is part of the same pull request).
6. **Look at it.** Run the UI in mock mode (`pnpm --dir apps/desktop/ui dev`, then
   `http://localhost:1420/?mock=1&lang=<code>`) and check long strings do not break the layout
   (buttons, the Apps grid, the right panel). Add screenshots of a dark and the light theme to the
   pull request.

## Not covered by the UI files

- The command line (`bot setup`) offers English and Turkish only (`apps/cli/src/wizard.rs`,
  `apps/cli/src/presets.rs`); a new CLI language is a separate, larger change.
- The natural-language schedule parser understands English and Turkish phrases
  (`crates/scheduler/src/nl.rs`).
- The user guide in `docs/guide/` is English only.

Tell us in the pull request which of these you did or did not do.

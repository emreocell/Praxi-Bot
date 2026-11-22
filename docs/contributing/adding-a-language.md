# Adding a language to the desktop app

The desktop UI is translated with `i18next`. Today it ships **English** (`en`, the fallback), **German** (`de`) and
**Turkish** (`tr`). A new language is a good first contribution: it is mostly translation work, and
the checks tell you what is missing.

## Steps

All paths are under `apps/desktop/ui/src/`.

1. **Copy the English file.** `cp i18n/locales/en.json i18n/locales/<code>.json` (use the
   [ISO 639-1](https://en.wikipedia.org/wiki/List_of_ISO_639-1_codes) code, for example `fr`).
   Translate the *values*, never the keys. Keep placeholders such as `{{name}}` and `{{count}}`
   exactly as they are; you may move them inside the sentence. Follow the
   [translation glossary](translation-glossary.md) for consistent terminology and tone.
2. **Register it** in `i18n/index.ts`: import the file, add it to `resources`, and extend
   `SupportedLanguage` (`"en" | "tr" | "de" | "<code>"`).
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
   `check:i18n` compares every file in `i18n/locales/` against `en.json`, so a new locale only has to be
   added there.

   It also prints a summary per locale, such as `tr: 27 values identical to English`, followed by
   one `warn <code>: <key> = "<value>"` line for each value that is exactly the English text. These
   are often strings that were copied from `en.json` and not translated yet. The list is only a hint:
   many words are correctly the same in your language (`Terminal`, `Port`), so translate what needs
   it and leave the rest. The warnings never make the check fail. Brand and product names,
   placeholder-only values such as `{{count}}`, example values in `*Placeholder` keys and keyboard
   shortcuts are skipped; the allow-list is at the top of `scripts/check-i18n.mjs`.
6. **Look at it.** Run the UI in mock mode (`pnpm --dir apps/desktop/ui dev`, then
   `http://localhost:1420/?mock=1&lang=<code>`) and check long strings do not break the layout
   (buttons, the Apps grid, the right panel). Add screenshots of a dark and the light theme to the
   pull request.

## Not covered by the UI files

- The command line (`bot setup`) offers English and Turkish only (`apps/cli/src/wizard.rs`,
  `apps/cli/src/presets.rs`); a new CLI language is a separate, larger change.
- The natural-language schedule parser understands English and Turkish phrases
  (`crates/scheduler/src/nl.rs`).
- The Telegram channel's own texts are an English/Turkish catalog in
  `crates/channel-telegram/src/messages.rs`, and `/lang` accepts only `en` and `tr`. A new Telegram
  language needs a new `Language` value, a text for every message, and tests; keep each message's
  arguments and the security meaning of approval texts exactly as in English.
- The user guide in `docs/guide/` is English, with Turkish and German pages in `docs/guide/tr/` and `docs/guide/de/`; see
  [Translating the user guide](translating-the-guide.md).

Tell us in the pull request which of these you did or did not do.

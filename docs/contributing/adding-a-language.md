# Adding a language to the desktop app

The desktop UI is translated with `i18next`. The language pickers currently offer **English** (`en`,
the fallback), **German** (`de`), **Turkish** (`tr`), **Spanish** (`es`), **French** (`fr`),
**Portuguese** (`pt`) and **Indonesian** (`id`). German, Turkish and Spanish have translated values;
French, Portuguese and Indonesian are scaffolds with English values, filled in one part at a time
(see [Scaffold locales](#scaffold-locales)).
A new language is a good first contribution: it is mostly translation work, and the checks tell you
what is missing.

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
   added there. A **strict** locale must have exactly the keys of `en.json`; a **scaffold** locale (next
   section) may lag.

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

## Scaffold locales

A language you add in pieces is a *scaffold*: a copy of `en.json` whose values are translated part by
part. New strings keep landing in `en.json` while you translate, so a scaffold always lags a little.
To keep that from failing everyone else's CI, the codes of the scaffold locales are listed in one
place, `i18n/scaffold-locales.json`:

```json
{ "scaffold": ["fr", "id", "pt"] }
```

| | Strict locale (`en`, `tr`, `de`, `es`, and every locale not in the list) | Scaffold locale |
| --- | --- | --- |
| Key missing from the locale | **error** | warning (the app shows the English text: `fallbackLng` is `en`) |
| Key that `en.json` does not have | error | error |
| `{{placeholders}}` different from English | error | error |
| Malformed JSON, wrong kind (string/object/array), unsorted keys | error | error |
| Value identical to English | warning | warning |

- **New locale:** add its code to `scaffold-locales.json` in the same pull request that adds the
  English-copy file, unless you translate every key at once (then leave it out: it is strict from the
  start).
- **Leaving the list:** when all translation parts are done, remove the code from
  `scaffold-locales.json` in the pull request that finishes the last part. From then on a missing key
  fails CI, like it does for `tr`, `de` and `es`, so check `check:i18n` prints no `missing` line first.
- **Catching up:** the check prints one `warn missing in <code> (scaffold): <key>` line per lagging
  key. Copy the English value (or translate it), then run `pnpm --dir apps/desktop/ui sort:i18n`.
- Never add a strict locale to the list to get a failing check green. The list is only for languages
  that are really being translated in parts.

## Not covered by the UI files

- The command line (`bot setup`) offers English and Turkish only (`apps/cli/src/wizard.rs`,
  `apps/cli/src/presets.rs`); a new CLI language is a separate, larger change.
- The natural-language schedule parser understands English and Turkish phrases
  (`crates/scheduler/src/nl.rs`).
- The Telegram and Discord channels' own texts are Rust catalogs, and the owner notices both share are one more; see
  [The Telegram message catalog](#the-telegram-message-catalog) and
  [The Discord and shared notice catalogs](#the-discord-and-shared-notice-catalogs) below.
- The user guide in `docs/guide/` is English, with Turkish and German pages in `docs/guide/tr/` and `docs/guide/de/`; see
  [Translating the user guide](translating-the-guide.md).

Tell us in the pull request which of these you did or did not do.

## The Telegram message catalog

The texts the Telegram channel writes itself (command replies, approval-card headings and buttons,
callback answers, pairing replies and run notices) live in `crates/channel-telegram/src/messages.rs`,
in English and Turkish. Each paired chat has its own language (`/lang en`, `/lang tr`; see
[the Telegram guide](../guide/telegram.md#language)). Text that comes from elsewhere is never put in
the catalog: bot names, daemon errors, tool output, voice transcripts, and anything that could hold
a secret.

- **A message** is a variant of `Message`. Values are typed fields (`BotSwitched { name, id }`), not
  format strings, so a translation cannot drop or rename a placeholder by accident.
- **Every language** is an arm of the `match` in `Message::render`. It has no wildcard, so a message
  without a Turkish text does not compile.
- **The tests** in the same file check that every message is translated, that the Turkish text is not
  a copy of the English one, and that each argument appears exactly once in every language. When you
  add a message, add it to `every_message()` and `arguments()` in the tests. `arguments()` has no
  wildcard either, so the compiler reminds you.
- **The English text** of existing messages is pinned by tests; change it only on purpose.
- **Approval texts** must keep their security meaning in every language: what will happen, what is
  hidden, and that the decision may have to be made in the desktop app. Do not soften a warning.

Adding a third Telegram language is a larger change: `Language` is currently a two-way choice, so
it would become an enum with a code for `/lang`, and every arm in `render` gets a new text. Run
`cargo test -p channel-telegram` and `cargo clippy -p channel-telegram --all-targets -- -D warnings`.

## The Discord and shared notice catalogs

Discord has the same shape as Telegram, in `crates/channel-discord/src/messages.rs`: a `Msg` enum with
typed arguments and one `Msg::render(lang)` `match` per language, with no wildcard. It speaks English,
Turkish and German (`!lang en|tr|de`, `bot channels discord setup --lang ...`; see
[the Discord guide](../guide/discord.md)). The tests in that file check that every message exists in
every language, that no translation is a copy of the English text, and that each argument and each
command (`!use`, `!bots`, ...) appears exactly once. Commands and button ids never change with the
language. Add a message to `every_message()` and `arguments()` in the tests; the latter has no wildcard
either.

What a bot reports on its own (a finished, failed or blocked mission, the questions in it, and the
answers the owner gets back) is written once for both channels in `crates/channel-api/src/notice.rs`
(`render` and `feedback`, `NoticeLang` is English or Turkish). Each bridge picks the language of its
own chat. A Discord chat set to German therefore still gets these notices in English until a German
text is added there: this is the fallback, not a bug. A new notice text needs an English and a Turkish
arm, and the tests in that file check that both show the same facts.

Credentials, tool output, daemon errors and the body of an approval card (`protocol::approval_text`,
whose fixed `DAEMON SUMMARY:` line the channels rely on) are not part of any catalog. Run
`cargo test -p channel-api -p channel-discord` and
`cargo clippy -p channel-api -p channel-discord --all-targets -- -D warnings`.

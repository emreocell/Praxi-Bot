# Translating the user guide

The guide in `docs/guide/` is English. Turkish lives in `docs/guide/tr/` and German in `docs/guide/de/`.
A page that has no translation yet simply opens in English, so you can translate one page at a time.

## How to add or update a page

1. Copy `docs/guide/<page>.md` to `docs/guide/<lang>/<page>.md` (same file name).
2. Translate the text. Keep the structure: same headings, same tables, same order.
3. Fix the links: a link to another translated page stays `other.md`; a link to a page that is not
   translated yet becomes `../other.md`; a link to a file outside the guide gets one more `../`.
   Do not link to a heading anchor (`#...`): translated headings have different anchors.
4. Add the page to `docs/guide/<lang>/index.md`.
5. From the repository root run:
   ```sh
   pnpm --dir apps/desktop/ui build:guide-index   # rebuilds the in-app search index
   python scripts/check_markdown_links.py
   pnpm --dir apps/desktop/ui test
   ```
6. If the English page changed after you translated it, update your page in the same pull request or say so in
   the description. A stale translation is worse than none: it can describe a safety rule that no longer exists.

## How the in-app search uses your page

The Help window searches the title, the headings and the first sentence of each page (the index built in step 5).
Pick a clear title and headings, in the page's own language: the search ignores case and accents, so "ısı" and
"ISI" match each other in Turkish, and in German "für" is found by "fur" and "fuer" and "Straße" by "strasse".
A language that has a folder in `docs/guide/` gets its own index; pages it does not have yet are searched
(and opened) in English. A new language folder also needs its index registered in `apps/desktop/ui/src/lib/helpFaq.ts`;
a test fails until it is.

## Style

- **Plain words.** Many readers are not programmers. Short sentences, everyday words, no jargon where a normal
  word works. Say what the reader will see and do ("press **Create**"), not how it is built.
- **Same register as the app.** Turkish: warm `sen`. German: informal `du`. Use the terms in the
  [translation glossary](translation-glossary.md), so the guide matches the buttons people see.
- **Buttons and menu names** are written exactly as in the app's translated UI (`apps/desktop/ui/src/i18n/locales/`),
  in bold, so people can find them.
- **Do not translate** commands, file names, code, option names (`--replace`), tool names (`calendar_list_events`),
  paths, URLs and product names (Praxi, Telegram, Home Assistant). Comments inside code blocks may be translated.
- **Keep every safety statement as strong as the English one.** "Always asks", "never", "only the owner" and
  "untrusted" must stay just as strict. If something is unclear, ask in the pull request instead of softening it.
- **Be honest.** Keep notes such as "Planned", "experimental" and "Not verified". Do not invent features.
- Turkish: no apostrophe after common nouns (`botun`), Turkish quotes and `yapay zekâ` with the circumflex.
  German: natural wording, not word by word; `Bot`, `Skill` and `Workflow` stay as in the glossary.
- Machine translation is fine as a first draft, but read the result aloud once. If it sounds odd, rewrite it.

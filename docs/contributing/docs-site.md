# The documentation website

The pages in `docs/` are also published as a searchable website on GitHub Pages
(`https://emreocell.github.io/Praxi-Bot/`). The website is built from the same Markdown files you see
on GitHub: there is no second copy to keep up to date.

## Why VitePress

| | VitePress | MkDocs Material | mdBook |
| --- | --- | --- | --- |
| Licence | MIT | MIT | MPL-2.0 |
| Languages | Plain folders, our own switcher | Needs a plugin per language | One book per language |
| Search | Built in, runs in the browser | Built in | Built in |
| Build | A few seconds | Python, slower | Fast |
| Runtime server | None (static files) | None | None |
| Extra tooling | Node (the repo already uses it) | Python plugins | A Rust binary |

We picked VitePress because it needs no extra toolchain, indexes all three languages for search
without a plugin, and can keep our existing folder layout (`docs/guide/tr/`, `docs/guide/de/`)
unchanged. MkDocs Material is in maintenance mode, and mdBook has no real multi-language support.

## Preview it on your computer

You need Node 20 or newer.

```sh
cd docs-site
npm ci
DOCS_BASE=/ npm run dev       # live preview, usually http://localhost:5173/
DOCS_BASE=/ npm run build     # same build CI runs; output goes to docs-site/.vitepress/dist
```

On Windows use PowerShell and `$env:DOCS_BASE = "/"` before the command (Git Bash rewrites a bare `/`).
`DOCS_BASE=/` serves the site at the root. Without it the site expects to live under `/Praxi-Bot/`, like on GitHub Pages.

The build fails on a dead link, so fix the link in the Markdown file. Also run
`python scripts/check_markdown_links.py` from the repository root, as for any docs change.

## How the pages are laid out

- `docs/index.md` is the home page, with the two "Start here" cards (users and contributors).
- The sidebar of the user guide is **generated from `docs/guide/index.md`** (and the Turkish and German
  indexes): every `##` heading becomes a group and every link in it a sidebar entry, in the same order. To add
  a page to the website, add it to the index, as you already must for `scripts/check_guide_index.py`.
- `docs/contributing/` and `docs/design/` (design notes and the `*-adr.md` decisions) get their sidebars from
  the file names and first headings. `docs/contributing/README.md` is the contributing home page.
- Links that point outside `docs/` (the README, source code, scripts) become links to the file on GitHub.
- Text such as `<bot-id>` that is not real HTML is shown as written, so you do not need to escape it.
- Every page has an **Edit this page on GitHub** link.

## Screenshots

The pictures in the guide (`docs/guide/images/<lang>/`, English and Turkish) are made from the mock-mode app by
a script, not by hand. After a UI change, refresh them with one command and commit the result with the page
changes:

```sh
pnpm --dir apps/desktop/ui docs:screenshots
```

See [apps/desktop/ui/scripts/docs-screenshots/README.md](../../apps/desktop/ui/scripts/docs-screenshots/README.md)
for the options and for how to add a screen.

## Translations

English, Turkish and German pages live in `docs/guide/`, `docs/guide/tr/` and `docs/guide/de/`, as described in
[Translating the guide](translating-the-guide.md). The language switcher in the top bar keeps you on the same
page when it exists in the other language. A page that is not translated yet opens in English. Only the user
guide is translated; the contributor and design pages are English only, and the switcher then goes to the
guide home of the chosen language. The website's own menus and search box are in English.

## Publishing

`.github/workflows/docs-site.yml` builds the site on every pull request that touches `docs/**` or
`docs-site/**` (check only, nothing is published, and fork pull requests get a read-only token). A push to
`main` that changes those paths builds again and publishes to GitHub Pages. The workflow needs
**Settings → Pages → Source: GitHub Actions** to be switched on once by a repository admin.
The site contains only what is in `docs/`: no secrets, no analytics, no paid service.

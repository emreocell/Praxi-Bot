# Guide screenshots

The images in the user guide (`docs/guide/images/`) and on the documentation website are made by a
script, not by hand, so they can be refreshed in one command whenever the app's look changes.

```sh
pnpm install --frozen-lockfile                 # once, from the repository root
pnpm --dir apps/desktop/ui docs:screenshots    # all screens, English and Turkish (about 2 minutes)
```

Then look at the changed images (`git status docs/guide/images`), commit them with the page changes, and
refresh the Help window's search index if you edited a guide page:

```sh
pnpm --dir apps/desktop/ui build:guide-index
```

## What it does

1. Builds the app in mock mode (`VITE_MOCK=1`: no daemon, fixed sample data) and serves it with `vite preview`.
2. Opens every screen of [`screens.mjs`](screens.mjs) in headless Chrome, once per language, at a fixed state:
   1280 x 800 window (a taller one for long forms), the Obsidian (dark) theme, glass style, no motion, time
   zone UTC and a frozen clock (27 Nov 2026, 09:41), so a run only changes an image when the app changed.
3. Writes `docs/guide/images/<lang>/<name>.webp`. The WebP is encoded by Chrome itself (quality 0.9), so no
   image tool is needed; a screen is 20 to 70 KB.

It needs Chrome or Chromium. It is found through `CHROME_BIN`, then in the usual places
(`/usr/bin/google-chrome`, `/usr/bin/chromium`, Windows and macOS Chrome). If Playwright has already downloaded
a Chromium for you: `CHROME_BIN=~/.cache/ms-playwright/chromium-*/chrome-linux64/chrome`.

## Options

| Option | Meaning |
| --- | --- |
| `--lang=en,tr` | Languages to capture (default: both). Add one by adding it to `LANGUAGES` in `capture.mjs`. |
| `--only=<text>` | Only the screens whose name contains the text, for example `--only=settings`. |
| `--out=<dir>` | Another output folder (default `docs/guide/images`). |
| `--quality=0.9` | WebP quality, 0 to 1. |

Pass them after the script name: `pnpm --dir apps/desktop/ui docs:screenshots --lang=en --only=apps`.

## Add or change a screen

1. Add an entry to `SCREENS` in [`screens.mjs`](screens.mjs). `query` holds the mock-mode URL parameters
   (`screen=...`, `section=...`, `panel=1`, `lv=open`; see `src/App.tsx` and `src/api/mock.ts`); `ready` is a
   selector to wait for; `act` clicks through to the state you want; `dialog: true` crops to the open dialog.
   Use the existing `data-testid` attributes. Do not change the app just to take a picture.
2. Run it for that screen: `pnpm --dir apps/desktop/ui docs:screenshots --only=<name>`.
3. Reference it from the page. English pages use `images/en/<name>.webp`; Turkish pages
   (`docs/guide/tr/`) use `../images/tr/<name>.webp`. Write an alt text that says what the picture shows.

German pages have no screenshots yet: the German UI strings are not captured, and an English image on a
German page would be confusing. Add `de` to `LANGUAGES` (and a `de` locale in the mock mode, which already
accepts `lang=de`) when someone wants them.

## Known limits

- A run is repeatable, but a few dialog shots (about 2 of 44) can differ in barely visible backdrop shading
  (at most 14 of 255 levels) between two runs. After a refresh, look at `git diff --stat` and
  `git checkout` the images whose page did not change on purpose.

- The mock data is partly Turkish whatever the language (bot status lines in the left rail, some sample
  messages and schedule names). That is the mock fixture in `src/api/mock.ts`, not the script.
- Screens that need a real daemon (a live host, a real provider sign-in) cannot be captured.

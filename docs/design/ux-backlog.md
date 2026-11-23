# UX backlog: beginner walkthrough (c528)

Found by walking first run, New bot, Settings, "Where do my bots run?", Add a server, Scheduled tasks and Help as a
complete beginner. The walk was done by reading the screens' code and strings, not in a live browser, so layout
items are inferred from the CSS and still need a look at 640 / 800 / 1280 px.

## Fixed in this change
Scheduled tasks: header buttons wrap at 640 px; the empty state has its own "New schedule" button; the disabled
button explains itself; the List/Calendar group has its own label; the cron hint gives an example; opening the form
scrolls it into view and focuses its title. Add a server: plainer intro, release note, SSH agent and fingerprint
text; the "Is this your server?" card takes the keyboard. Servers: "Check now" (it really switches computer) is now
"Switch to it". New bot: "Autonomy" is now "How much it may do alone".

## Not fixed
- **Add a server, details page:** three info callouts (password, release, agent) stack above the Connect button and
  push it off screen at 640 px. Fold the two notes into one "Good to know" disclosure.
- **Add a server, "SSH" and "key file":** still jargon on the choose page (`choose.ssh`, `keyHint`). Needs a short
  "what is a key file?" help link.
- **Add a server:** the user name hint only names Oracle and Ubuntu; a home PC owner has no idea what to type.
- **Servers dialog:** the remove confirmation does not take focus; the empty state is a bare sentence.
- **Where do my bots run?:** an offline server row is still selectable and gives no reason or retry; the popover
  has no visible "what is a server?" link.
- **New bot:** the Free level hint uses "catastrophic commands"; the brain "Auto" has no one-line explanation.
- **First run:** the brain step is long (591 lines of states) and has no "skip, decide later" next to the key box;
  the recap says "Freedom" where New bot says "Autonomy" (now a different label again).
- **Settings:** keyword search matches Turkish words without diacritics only; no empty-result state was checked.
- **Help:** FAQ answers open external docs, which fails silently offline; guide results are English-only in tr/de.
- **Scheduled tasks:** the form's many fields are all visible at once; "Cron" and timezone could sit behind
  "Other". Calendar view day cells have no text for screen readers beyond the count.
- **Keyboard:** the Add a server SSH/agent and key pickers are radiogroups where each radio is a separate Tab stop
  (no arrow-key roving as the Computer settings radios have).

# Accessibility pass on the new UI (c545)

`pnpm --dir apps/desktop/ui check:a11y` now also opens Settings search (with and without results), Help, the run
location popover, Add a server, the schedule builder, the run timeline and the `/` and `@` popovers (axe incl.
WCAG 2.2 target size), tabs through each of them in clay, obsidian, contrast-dark and contrast-light looking for a
focus indicator, and checks that Escape returns focus to the control that opened the popover/dialog.

## Fixed in this change
- Schedule builder went blank in the browser (a date format mixed `dateStyle` with `weekday`, which throws).
- Help answers were `role="listitem"` on `<details>` inside a `role="list"` div; now a real list.
- Add a server: the step ("Step 2 of 3") and the install stage are announced through live regions.

## Not fixed / not verified
- Nobody has run a real screen reader (NVDA, VoiceOver, Orca) over these views; only axe and keyboard checks.
- Windows forced-colours mode was not checked.
- The Add a server radiogroups are still one Tab stop per radio (see above).
- The composer's combobox-on-textarea and the muted panel labels stay in `a11y-allowlist.mjs`.
- The Tab walk stops after 45 stops per view; very long lists (Help FAQ with search results) are only sampled.

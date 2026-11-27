# Writing a goal recipe

A recipe turns "what I want" into a configured bot and a started task: the goal text, what must be
connected, a few questions asked once, how freely the bot may work and an optional schedule. The
desktop app offers them under **Start from a goal** (home screen, empty chat and the New-bot dialog).

Recipes are data: add an object to `recipes` in
[`apps/desktop/ui/src/data/recipes.json`](../../apps/desktop/ui/src/data/recipes.json). The file is
versioned (`schema_version: 1`) and validated at load by `validateRecipe` in
`apps/desktop/ui/src/lib/recipes.ts`. A recipe that fails validation is left out of the app, and
`recipes.test.ts` fails on it, so run `pnpm --dir apps/desktop/ui test` before sending a PR.

## Fields

| Field | Meaning |
| --- | --- |
| `id` | Lowercase letters, digits and `-`, unique. |
| `icon` | A lucide icon name known to `views/RecipeFlow.tsx` (`link`, `inbox`, `sun`, `eye`, `table`, `file-text`). Add one there if you need another. |
| `level` | How freely the bot may work: `ask` or `balanced`. **`auto` is not allowed**: a recipe can only ask, and the owner confirms (and may change) the level in the review step. The bot gets the confirmed level, never the recipe's. |
| `bot` | `shape`, `color` (`#rrggbb`) and `name` per language. |
| `needs` | What must be connected: `model`, `browser`, `email` (`purpose`: `own` for the bot's own mailbox, `inbox` for the owner's inbox to read) or `app` (`apps`: names or slugs to look for, `label`: text per language). Add `"optional": true` for a need that should not block starting. |
| `questions` | Asked once, in order. `kind`: `text`, `longtext`, `url`, `choice` (with `options` and a label per option) or `folder` (at most one; the bot is then confined to that folder). Each has `required`, an optional `default` and `text` per language (`label`, `hint`, `placeholder`). |
| `schedule` | Optional: `freq` (`daily`, `weekdays`, `weekly`, `monthly`), `time` (`HH:MM`) and `weekday` (0 = Sunday, weekly only). The owner can turn it off or change the time in the review step. |
| `text` | Per language: `name`, `blurb` and `goal`. |

## Rules the validator enforces

- Every localized object has an `en` entry. Other languages are optional and fall back to English;
  shipped recipes also have `tr` (a test checks it).
- The `goal` uses an answer as `{{question_id}}`. Every placeholder must name a question and every
  non-folder question must be used. Answers are inserted in one pass, so an answer that itself
  looks like a placeholder is never expanded again. An empty optional answer reads as "(nothing specified)".
- Unknown keys are rejected.

## What the flow does

1. The owner picks a recipe and answers the questions (checked: required, a real web address, an
   absolute folder path without `..`).
2. The review step checks the needs against what is really connected (model, browser, mailboxes,
   apps). A missing need has a button that opens the screen that fixes it; the answers are kept.
   "Could not check" is never shown as ready and never blocks.
3. One button creates the bot at the confirmed level, opens a chat, sends the goal followed by the
   shared autonomy guidance (`guidance` in the file) and, if the owner kept the repeat on, creates a
   schedule that repeats the same message. A retry after a failure continues where it stopped and
   never creates a second bot.

There is no mission object yet (see [the Missions ADR](../design/missions-adr.md)); until there is,
the task runs as a normal chat run with the guidance text.

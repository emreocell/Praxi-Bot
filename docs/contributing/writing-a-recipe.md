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
| `mission` | Optional, for long multi-item work: `plan` (per language; what one ledger item is, appended to the goal) and `budgets` (`max_sessions`, `max_model_steps`, `max_minutes`, whole numbers under fixed ceilings). See "Missions" below. |
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
3. One button creates the bot at the confirmed level and starts the task (a chat run or a mission,
   see below). If the owner kept the repeat on, it also creates a schedule that repeats the goal
   followed by the shared autonomy guidance (`guidance` in the file) as a normal run. A retry after
   a failure continues where it stopped and never creates a second bot or mission; a retry at
   another level than the bot was created with is refused.

## Missions

A recipe with a `mission` block describes long work (many sites, pages or files). It runs as a
[mission](../guide/missions.md) **only when the owner confirmed the Full level** in the review step:
a mission works without asking, the daemon refuses to start one on a bot below Full, and a recipe can
never ask for Full itself. At Ask or Balanced the same recipe runs as one chat task (goal + guidance),
and the review step says so.

For a mission the flow calls `POST /v1/missions` with the filled goal followed by the recipe's `plan`
(no chat guidance: the ledger and the daemon's brief replace the progress file), the recipe name as
title and the recipe's `budgets`. The daemon makes the mission's own chat and the app opens it, where
the progress shows. The bot lists the ledger items itself; `plan` tells it what one item is. The goal
must stay under 4000 characters with all answers filled in; the flow checks this before it creates anything.

A recipe with both `mission` and `schedule` starts the mission as the first, big pass; the schedule
keeps repeating the goal as a normal run (a schedule cannot start a mission). Say so in `plan`
("this mission is the first pass only"). Short recipes (a daily brief, inbox triage) have no `mission`.

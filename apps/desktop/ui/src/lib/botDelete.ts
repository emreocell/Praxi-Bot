import type { BotDeletePreview, BotDeleteReport, Run } from "../api/types.ts";

// Deleting a bot for good: two steps (what goes, then type the name) and the numbers they show.

export type DeleteStep = "review" | "confirm";

/**
 * Where focus goes when the dialog shows `to` after `from`: the name box on step 2; back on step 1
 * (Back, or a refusal) the Cancel button, as the control that had focus is gone with step 2.
 */
export function focusAfterStep(from: DeleteStep | null, to: DeleteStep): "name" | "cancel" | null {
  if (to === "confirm") return "name";
  return from === "confirm" ? "cancel" : null;
}

/** The guide page that says how to make an encrypted backup first. */
export const BACKUP_GUIDE_URL = "https://github.com/emreocell/Praxi-Bot/blob/main/docs/guide/backup.md";

/** A thing the bot owns that goes with it; `key` names its label (`botDelete.item.<key>`). */
export type DeleteItemKey =
  | "chats"
  | "messages"
  | "runs"
  | "schedules"
  | "webhooks"
  | "missions"
  | "memories"
  | "grants"
  | "channels"
  | "mailboxes";

export interface DeleteCount {
  key: DeleteItemKey;
  count: number;
}

function nonZero(rows: Array<[DeleteItemKey, number]>): DeleteCount[] {
  return rows.filter(([, count]) => count > 0).map(([key, count]) => ({ key, count }));
}

/** What the daemon will remove, in the order shown: only what the bot has. */
export function deletedCounts(p: BotDeletePreview): DeleteCount[] {
  return nonZero([
    ["chats", p.chats],
    ["messages", p.messages],
    ["runs", p.runs],
    ["schedules", p.schedules],
    ["webhooks", p.webhooks],
    ["missions", p.missions],
    ["memories", p.memories],
    ["grants", p.grants],
    ["channels", p.channels.reduce((n, c) => n + c.chats, 0)],
    ["mailboxes", p.mailboxes.filter((m) => m.fate === "removed").length]
  ]);
}

/** What was removed, for the toast after the delete. */
export function reportCounts(r: BotDeleteReport): DeleteCount[] {
  return nonZero([
    ["chats", r.chats],
    ["messages", r.messages],
    ["runs", r.runs],
    ["schedules", r.schedules],
    ["webhooks", r.webhooks],
    ["missions", r.missions],
    ["memories", r.memories],
    ["mailboxes", r.mailboxes_removed]
  ]);
}

/**
 * What stops the delete for now: the bot is working (a run, a running mission) or leads a
 * mission that waits for a timer, a mail or a webhook and would start again on its own.
 */
export type DeleteBlock = "run" | "mission" | "waiting" | null;

export function deleteBlock(p: BotDeletePreview): DeleteBlock {
  if (p.active_runs > 0) return "run";
  if (p.running_missions > 0) return "mission";
  if (p.waiting_missions > 0) return "waiting";
  return null;
}

/** Mission states "stop it first" cancels: a running one, and a waiting one that would wake. */
export const STOPPED_MISSION_STATES: ReadonlySet<string> = new Set(["running", "waiting"]);

/** A cleanup step that did not finish after the bot was deleted (`botDelete.step.<key>`). */
export type DeleteStepKey = BotDeleteReport["cleanup_pending"][number];

/**
 * What did not finish after the bot was deleted: cleanup steps and inboxes that are still at
 * AgentMail. Empty when everything went; otherwise the same request retries it.
 */
export function leftovers(r: BotDeleteReport): { steps: DeleteStepKey[]; inboxes: string[] } {
  return {
    steps: r.cleanup_pending,
    inboxes: r.inboxes_not_deleted.map((n) => n.address)
  };
}

/** AgentMail inboxes only this bot used: the owner may delete them at the provider as well. */
export function deletableInboxes(p: BotDeletePreview): BotDeletePreview["mailboxes"] {
  return p.mailboxes.filter((m) => m.inbox_deletable);
}

/** Mailbox accounts that stay for other bots (handed over or only unlinked). */
export function keptMailboxes(p: BotDeletePreview): BotDeletePreview["mailboxes"] {
  return p.mailboxes.filter((m) => m.fate !== "removed");
}

/**
 * The second step opens the red button only for the bot's own name (spaces around it do not
 * count). Pass the name the daemon just reported (the preview), not one the screen kept.
 */
export function nameMatches(typed: string, name: string): boolean {
  const want = name.trim();
  return want !== "" && typed.trim() === want;
}

const FINISHED = new Set(["succeeded", "failed", "cancelled"]);

/** Runs of the bot that have not finished (what "stop it first" stops). */
export function unfinishedRunIds(runs: readonly Run[], botId: string): string[] {
  return runs.filter((r) => r.bot_id === botId && !FINISHED.has(r.state)).map((r) => r.id);
}

/** The delete request: the inbox choice is sent only when there is an inbox to choose about. */
export function deleteOptions(p: BotDeletePreview, alsoInbox: boolean): { deleteInbox: boolean } {
  return { deleteInbox: alsoInbox && deletableInboxes(p).length > 0 };
}

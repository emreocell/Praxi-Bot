import type { BotDeletePreview, BotDeleteReport, Run } from "../api/types.ts";

// Deleting a bot for good: two steps (what goes, then type the name) and the numbers they show.

export type DeleteStep = "review" | "confirm";

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
    ["missions", r.missions],
    ["memories", r.memories],
    ["mailboxes", r.mailboxes_removed]
  ]);
}

/** What stops the delete for now: the bot is working. */
export type DeleteBlock = "run" | "mission" | null;

export function deleteBlock(p: BotDeletePreview): DeleteBlock {
  if (p.active_runs > 0) return "run";
  if (p.running_missions > 0) return "mission";
  return null;
}

/** AgentMail inboxes only this bot used: the owner may delete them at the provider as well. */
export function deletableInboxes(p: BotDeletePreview): BotDeletePreview["mailboxes"] {
  return p.mailboxes.filter((m) => m.inbox_deletable);
}

/** Mailbox accounts that stay for other bots (handed over or only unlinked). */
export function keptMailboxes(p: BotDeletePreview): BotDeletePreview["mailboxes"] {
  return p.mailboxes.filter((m) => m.fate !== "removed");
}

/** The second step opens the red button only for the bot's own name (spaces around it do not count). */
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

import { type CollapsedNotification, isActionable } from "./notificationHistory.ts";

/**
 * The notification center reads per bot: who needs me, what failed, what finished. Three types only
 * (a held question that was answered elsewhere is no longer "needs you"), and one primary action per row.
 */
export type NotifyType = "needsYou" | "failed" | "finished";
export type NotifyPrimary = "reply" | "retry" | "open";

/** Rows shown per bot before "N more". */
export const BOT_PREVIEW_MAX = 4;

/** Row order inside a bot: open questions, then unread failures, then the rest newest first. */
function rowRank(r: BotInboxRow): number {
  if (r.type === "needsYou") return 0;
  return r.type === "failed" && r.item.unread ? 1 : 2;
}

export function notifyType(item: CollapsedNotification, liveCallIds?: ReadonlySet<string>): NotifyType {
  if (item.activeCallId !== undefined || item.entries.some((e) => isActionable(e, liveCallIds))) return "needsYou";
  return item.primary.kind === "failed" ? "failed" : "finished";
}

/**
 * Reply (open the chat where the bot asks), Try again (re-send the failed turn) or Open. A failure
 * without a chat, one already retried, and every failure in private mode only open.
 */
export function primaryAction(item: CollapsedNotification, type: NotifyType, privateMode: boolean): NotifyPrimary {
  if (type === "needsYou") return "reply";
  const n = item.primary;
  if (type === "failed" && !privateMode && n.chatId && n.resolved !== "retried") return "retry";
  return "open";
}

export interface BotInboxRow {
  item: CollapsedNotification;
  type: NotifyType;
}

export interface BotInbox {
  /** "" for Praxi itself (daily digest and other app notices). */
  botId: string;
  /** Name stored with the newest notification (the bot may be renamed or gone since). */
  botName: string;
  rows: BotInboxRow[];
  counts: Record<NotifyType, number>;
  unread: number;
  latestTs: number;
}

/** 0: something needs me, 1: an unread failure, 2: the rest. */
function urgency(g: BotInbox): number {
  if (g.counts.needsYou > 0) return 0;
  return g.rows.some((r) => r.type === "failed" && r.item.unread) ? 1 : 2;
}

/** Collapsed notifications grouped by bot; rows by urgency then newest, bots by urgency then newest. */
export function inboxByBot(items: readonly CollapsedNotification[], liveCallIds?: ReadonlySet<string>): BotInbox[] {
  const byBot = new Map<string, BotInbox>();
  for (const item of items) {
    const botId = item.primary.botId;
    let group = byBot.get(botId);
    if (!group) {
      group = { botId, botName: "", rows: [], counts: { needsYou: 0, failed: 0, finished: 0 }, unread: 0, latestTs: 0 };
      byBot.set(botId, group);
    }
    const type = notifyType(item, liveCallIds);
    group.rows.push({ item, type });
    group.counts[type] += 1;
    if (item.unread) group.unread += 1;
    if (item.latestTs >= group.latestTs) {
      group.latestTs = item.latestTs;
      group.botName = item.primary.botName || group.botName;
    }
  }
  const groups = [...byBot.values()];
  for (const g of groups) {
    g.rows.sort((a, b) => rowRank(a) - rowRank(b) || b.item.latestTs - a.item.latestTs);
  }
  return groups.sort((a, b) => urgency(a) - urgency(b) || b.latestTs - a.latestTs);
}

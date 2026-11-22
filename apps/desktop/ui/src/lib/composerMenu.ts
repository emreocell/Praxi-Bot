import type { Bot, Chat } from "../api/types.ts";
import { botView, type BotView } from "./botProfile.ts";
import { rank } from "./fuzzy.ts";
import { APP_COMMANDS, LOCAL_COMMANDS, SLASH_COMMANDS, mentionHandle } from "./slash.ts";

export interface ComposerOption {
  id: string;
  label: string;
  detail: string;
  insert: string;
  indices: number[];
  group: string;
  bot?: BotView;
}

/** Keep the entire sidebar identity, including explicit shape and uploaded avatar. */
export function mentionOptions(bots: readonly Bot[], chats: readonly Chat[], query: string, activity: ReadonlyMap<string, number> = new Map()): ComposerOption[] {
  const latest = (id: string) => Math.max(0, ...chats.filter((c) => c.bot_id === id && !c.archived).map((c) => Math.max(c.created_at_ms, activity.get(c.id) ?? 0)));
  const pinned = (id: string) => chats.some((c) => c.bot_id === id && !c.archived && c.pinned);
  const group = (id: string) => pinned(id) ? "pinned" : latest(id) ? "recent" : "bots";
  const priority = (id: string) => pinned(id) ? 0 : latest(id) ? 1 : 2;
  const ordered = [...bots].sort((a, b) => priority(a.id) - priority(b.id) || latest(b.id) - latest(a.id));
  return rank(ordered, query, (b) => ({ text: mentionHandle(b.name) })).map((r) => {
    const bot = botView(r.item);
    return { id: bot.id, label: `@${mentionHandle(bot.name)}`, detail: bot.executor.split("/").pop() ?? "",
      insert: mentionHandle(bot.name), indices: r.indices.map((i) => i + 1), group: group(bot.id), bot };
  });
}

export function commandOptions(query: string, recent: readonly string[], translate: (key: string) => string): ComposerOption[] {
  const commands = [...SLASH_COMMANDS, ...LOCAL_COMMANDS, ...APP_COMMANDS];
  const group = (name: string) => recent.includes(name) ? "recent" : APP_COMMANDS.some((c) => c.name === name) ? "app" : "chat";
  const priority = (name: string) => group(name) === "recent" ? 0 : group(name) === "chat" ? 1 : 2;
  commands.sort((a, b) => priority(a.name) - priority(b.name) ||
    (group(a.name) === "recent" ? recent.indexOf(a.name) - recent.indexOf(b.name) : 0));
  return rank(commands, query, (c) => ({ text: c.name, keywords: [translate(c.i18nKey)] })).map((r) => ({
    id: r.item.name, label: `/${r.item.name}`, detail: translate(r.item.i18nKey), insert: r.item.name,
    indices: r.indices.map((i) => i + 1), group: group(r.item.name)
  }));
}

/** Fuzzy results stay in score order within their section. */
export function optionGroups(options: readonly ComposerOption[]): string[] {
  return ["pinned", "recent", "chat", "app", "bots"].filter((g) => options.some((o) => o.group === g));
}

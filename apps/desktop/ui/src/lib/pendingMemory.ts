/**
 * How a memory waiting for the owner's OK reads as a plain card: where it came from (a chat, or
 * outside text such as a web page), which bots it is for, and the quote only when it adds something.
 */
export interface PendingMemoryLike {
  content: string;
  source_quote?: string | null;
  trust: "user" | "untrusted";
  bot_id?: string | null;
}

export interface PendingMemoryView {
  /** i18n suffix under `memory.origin.` */
  origin: "chat" | "outside";
  /** `shared` = every bot of this owner; otherwise the bot id it belongs to. */
  scope: { kind: "shared" } | { kind: "bot"; botId: string };
  /** The source quote, or null when the memory text already says the same thing. */
  quote: string | null;
}

const squash = (text: string) =>
  text
    .toLocaleLowerCase()
    .replace(/["'“”‘’«»]/g, "")
    .replace(/\s+/g, " ")
    .trim();

export function pendingMemoryView(item: PendingMemoryLike): PendingMemoryView {
  const quote = item.source_quote?.trim() ?? "";
  const redundant = quote === "" || squash(item.content).includes(squash(quote));
  return {
    origin: item.trust === "user" ? "chat" : "outside",
    scope: item.bot_id ? { kind: "bot", botId: item.bot_id } : { kind: "shared" },
    quote: redundant ? null : quote
  };
}

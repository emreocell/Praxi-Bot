import type { Message } from "../api/types";

/** What the owner may do with the chat's last turn (edit & resend / regenerate). */
export interface LastTurn {
  /** The last live user message. */
  user: Message;
  /** Written in this app and followed only by assistant answers: the daemon accepts a resend. */
  resendable: boolean;
  /** A run of this turn used a tool that is not read-only: redoing it can repeat an effect. */
  usedWriteTools: boolean;
  /** The bot answered at least once (regenerate has something to replace). */
  answered: boolean;
}

/** The last turn among live (not superseded) messages, or null for an empty chat. */
export function lastTurn(messages: readonly Message[]): LastTurn | null {
  const live = messages.filter((m) => !m.superseded);
  let at = -1;
  for (let i = live.length - 1; i >= 0; i -= 1) {
    if (live[i].role === "user") {
      at = i;
      break;
    }
  }
  if (at < 0) {
    return null;
  }
  const user = live[at];
  const rest = live.slice(at + 1);
  return {
    user,
    resendable: user.source === "owner" && rest.every((m) => m.role === "assistant"),
    usedWriteTools: [user, ...rest].some((m) => m.used_write_tools === true),
    answered: rest.length > 0
  };
}

/**
 * Whether the finished last run gets its "regenerate" action: the turn can be resent and either the bot
 * answered or the run failed (nothing to replace, but the owner can try the same message again after
 * fixing what the error said). `runFailed` is the last run's error, if any.
 */
export function offersRegenerate(turn: LastTurn | null, runFailed: boolean): boolean {
  return turn !== null && turn.resendable && (turn.answered || runFailed);
}

/** Runs whose messages were superseded: their live events must not render as the current answer. */
export function supersededRunIds(messages: readonly Message[]): Set<string> {
  const ids = new Set<string>();
  for (const m of messages) {
    if (m.superseded && m.run_id) {
      ids.add(m.run_id);
    }
  }
  return ids;
}

/** A run of superseded messages shown as one collapsed "earlier version". */
export interface SupersededGroup {
  key: string;
  ts: number;
  messages: Message[];
}

/** Consecutive superseded messages, in order. */
export function supersededGroups(messages: readonly Message[]): SupersededGroup[] {
  const groups: SupersededGroup[] = [];
  let open: SupersededGroup | null = null;
  for (const m of messages) {
    if (!m.superseded) {
      open = null;
      continue;
    }
    if (!open) {
      open = { key: `sup-${m.id}`, ts: m.created_at_ms, messages: [] };
      groups.push(open);
    }
    open.messages.push(m);
  }
  return groups;
}

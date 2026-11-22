/**
 * What the small info popover next to a message shows (design v7). Pure: it maps what the daemon
 * already stores (message time and source, the run's events, its usage record) to labelled rows
 * and leaves out whatever is not known. Wording and clock formats come from the caller.
 */
import { formatClock } from "./messageTime.ts";
import { modelLine } from "./modelName.ts";
import { sourceLabel } from "./modelBrand.ts";

export type InfoKey = "model" | "source" | "duration" | "started" | "finished" | "steps" | "where" | "tokens" | "sent";

export interface InfoRow {
  key: InfoKey;
  value: string;
}

export interface ReplyInfoInput {
  startedMs?: number;
  finishedMs?: number;
  /** Number of tool steps of the run. */
  steps?: number;
  /** `provider/model` the turn used (the run's usage record). */
  model?: string | null;
  /** Name of the computer the run worked on. */
  computer?: string | null;
  promptTokens?: number | null;
  completionTokens?: number | null;
}

/** "12,4 sn" / "1 dk 5 sn": a run's wall time, with seconds. */
export function durationLabel(ms: number, lang: string, units: { s: string; m: string }): string {
  const clamped = Math.max(0, ms);
  const tr = lang.toLowerCase().startsWith("tr");
  if (clamped < 60_000) {
    const one = new Intl.NumberFormat(tr ? "tr-TR" : lang, { minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(
      Math.round(clamped / 100) / 10
    );
    return `${one} ${units.s}`;
  }
  const total = Math.round(clamped / 1000);
  const s = total % 60;
  return `${Math.floor(total / 60)} ${units.m}${s > 0 ? ` ${s} ${units.s}` : ""}`;
}

/** Whole numbers with the language's grouping: 4.210 / 4,210. */
function count(n: number, lang: string): string {
  return new Intl.NumberFormat(lang.toLowerCase().startsWith("tr") ? "tr-TR" : lang).format(n);
}

/** The rows of a bot reply's popover, in the design's order. */
export function replyInfoRows(
  i: ReplyInfoInput,
  lang: string,
  units: { s: string; m: string; steps: (n: number) => string }
): InfoRow[] {
  const rows: InfoRow[] = [];
  const ref = (i.model ?? "").trim();
  if (ref) {
    const line = modelLine(ref);
    if (line.name) {
      rows.push({ key: "model", value: line.name });
    }
    const slash = ref.indexOf("/");
    if (slash > 0) {
      rows.push({ key: "source", value: sourceLabel(ref.slice(0, slash)) });
    }
  }
  if (i.startedMs !== undefined && i.finishedMs !== undefined && i.finishedMs >= i.startedMs) {
    rows.push({ key: "duration", value: durationLabel(i.finishedMs - i.startedMs, lang, units) });
  }
  if (i.startedMs !== undefined) {
    rows.push({ key: "started", value: formatClock(i.startedMs, lang, true) });
  }
  if (i.finishedMs !== undefined) {
    rows.push({ key: "finished", value: formatClock(i.finishedMs, lang, true) });
  }
  if (i.steps !== undefined) {
    rows.push({ key: "steps", value: units.steps(i.steps) });
  }
  if (i.computer) {
    rows.push({ key: "where", value: i.computer });
  }
  if (i.promptTokens != null && i.completionTokens != null && (i.promptTokens > 0 || i.completionTokens > 0)) {
    rows.push({ key: "tokens", value: `${count(i.promptTokens, lang)} / ${count(i.completionTokens, lang)}` });
  }
  return rows;
}

/** Where a user message came from, as the daemon records it (`owner` = this app). */
export type MessageSourceKind = "app" | "telegram" | "peer" | "api" | "imported" | "unknown";

export function messageSourceKind(source: string | null | undefined): MessageSourceKind {
  switch (source) {
    case "owner":
      return "app";
    case "telegram":
      return "telegram";
    case "peer":
    case "peer_admin":
      return "peer";
    case "local_api":
      return "api";
    case "imported":
      return "imported";
    default:
      return "unknown";
  }
}

/** The rows of a user message's popover: sent time (with seconds) and source. */
export function userInfoRows(sentMs: number, source: string | null | undefined, lang: string, sourceText: (k: MessageSourceKind) => string): InfoRow[] {
  const rows: InfoRow[] = [{ key: "sent", value: formatClock(sentMs, lang, true) }];
  const kind = messageSourceKind(source);
  if (kind !== "unknown") {
    rows.push({ key: "source", value: sourceText(kind) });
  }
  return rows;
}

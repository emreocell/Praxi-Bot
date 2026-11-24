/**
 * How a thinking chain is laid out as rows (pure, no React).
 *
 * Consecutive finished calls of the same app or tool fold into ONE group row
 * ("Playwright - browser evaluate x9"); calls that failed, were refused or are
 * still running never hide inside a group. A collapsed chain keeps only what
 * the owner must not miss: failures and the step that is running now.
 */
import type { ToolItem } from "../state/timeline.ts";
import type { Chain, ChainStep } from "./thinkingChain.ts";
import { hostOf, resolveToolCall } from "./toolLabels.ts";

export type ChainRow =
  | { kind: "thought"; key: string; step: Extract<ChainStep, { kind: "thought" }> }
  | { kind: "tool"; key: string; tool: ToolItem }
  | {
      kind: "group";
      key: string;
      tools: ToolItem[];
      /** Every call is the same action (else the row names only the app). */
      sameAction: boolean;
    };

/** `mcp:github/x` and `github.x` are the same call. */
function canonicalName(tool: ToolItem): string {
  const name = resolveToolCall(tool.name, tool.input).name;
  const mcp = /^mcp:([^/]+)\/(.+)$/.exec(name);
  return mcp ? `${mcp[1]}.${mcp[2]}` : name;
}

/** The app of an MCP call (`playwright.browser_click` -> `playwright`). */
export function appOf(tool: ToolItem): string | null {
  const m = /^([^.]+)\.(.+)$/.exec(canonicalName(tool)) ?? /^([^_]+)__(.+)$/.exec(canonicalName(tool));
  return m ? (m[1] ?? null) : null;
}

/** Calls with the same key fold together: the app for MCP calls, else the tool. */
export function groupKey(tool: ToolItem): string {
  const app = appOf(tool);
  return app ? `app:${app}` : `tool:${canonicalName(tool)}`;
}

/** Finished fine: the only status a group may absorb. */
function isPlainDone(tool: ToolItem): boolean {
  return tool.status === "done";
}

/** A call the owner must see even in a collapsed chain. */
export function isProblem(tool: ToolItem): boolean {
  return tool.status === "error" || tool.status === "expired" || tool.status === "denied";
}

export function buildRows(steps: readonly ChainStep[]): ChainRow[] {
  const rows: ChainRow[] = [];
  let run: ToolItem[] = [];
  const flush = () => {
    if (run.length === 0) return;
    const first = run[0] as ToolItem;
    if (run.length === 1) {
      rows.push({ kind: "tool", key: first.key, tool: first });
    } else {
      const names = new Set(run.map(canonicalName));
      rows.push({
        kind: "group",
        key: `group-${first.key}`,
        tools: run,
        sameAction: names.size === 1
      });
    }
    run = [];
  };
  for (const s of steps) {
    if (s.kind === "thought") {
      flush();
      rows.push({ kind: "thought", key: s.key, step: s });
      continue;
    }
    if (!isPlainDone(s.tool)) {
      flush();
      rows.push({ kind: "tool", key: s.tool.key, tool: s.tool });
      continue;
    }
    const prev = run[run.length - 1];
    if (prev && groupKey(prev) !== groupKey(s.tool)) flush();
    run.push(s.tool);
  }
  flush();
  return rows;
}

function rowIsProblem(r: ChainRow): boolean {
  return r.kind === "tool" && (isProblem(r.tool) || r.tool.status === "requested" || r.tool.status === "running");
}

/**
 * Rows shown while the chain is collapsed: failures, plus the last row of a
 * live chain (the step in progress). `hidden` counts what is folded away.
 */
export function collapsedRows(rows: readonly ChainRow[], live: boolean): { shown: ChainRow[]; hidden: number } {
  const lastTool = (() => {
    for (let i = rows.length - 1; i >= 0; i--) {
      if ((rows[i] as ChainRow).kind !== "thought") return i;
    }
    return -1;
  })();
  const shown = rows.filter((r, i) => rowIsProblem(r) || (live && i === lastTool));
  return { shown, hidden: rows.length - shown.length };
}

export interface ChainSummary {
  /** Tool calls (thoughts are not steps). */
  steps: number;
  /** Distinct MCP apps (server ids), first-seen order. */
  names: string[];
}

export function chainSummary(chain: Pick<Chain, "steps">): ChainSummary {
  const names: string[] = [];
  let steps = 0;
  for (const s of chain.steps) {
    if (s.kind !== "tool") continue;
    steps++;
    const n = appOf(s.tool);
    if (n && !names.includes(n)) names.push(n);
  }
  return { steps, names };
}

/** The one site every call of a group visited, else null ("Browsed webioo.com.tr"). */
export function groupHost(tools: readonly ToolItem[]): string | null {
  let host: string | null = null;
  for (const t of tools) {
    const input = t.input as Record<string, unknown> | undefined;
    const url = input && typeof input.url === "string" ? input.url : "";
    const h = url ? hostOf(url) : null;
    if (!h || (host !== null && host !== h)) return null;
    host = h;
  }
  return host;
}

/** A chain longer than this shows only its first rows until the owner asks for the rest. */
export const ROW_CAP = 40;

/** Keeps long runs (200+ steps) fast: the DOM holds at most `cap` rows until "show all". */
export function capRows<T>(rows: readonly T[], expanded: boolean, cap = ROW_CAP): { shown: T[]; hidden: number } {
  if (expanded || rows.length <= cap) return { shown: [...rows], hidden: 0 };
  return { shown: rows.slice(0, cap), hidden: rows.length - cap };
}

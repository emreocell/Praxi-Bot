/**
 * The terminals the bot runs (`shell_interactive`). A program on a pseudo-terminal has no window,
 * so the screen picture cannot show it: the live view lists the sessions in a panel next to the
 * picture, as read-only text. Pure rules, shared by the panel and its tests.
 */
import type { BotTerminal, BotTerminalState } from "../api/types";

/** How often the open live view asks the daemon for the terminals. */
export const TERMINAL_POLL_MS = 1500;

/**
 * The terminals to show next to the picture of a target. A bot's own browser shows that bot's
 * terminals; a desktop is shared, so it shows every terminal on this computer (a terminal with no
 * known bot is shown everywhere). Order is kept (the daemon sends the newest first).
 */
export function terminalsForTarget(
  terminals: readonly BotTerminal[],
  target: { kind: string; bot_id?: string | null } | null
): BotTerminal[] {
  const bot = target && target.kind !== "desktop" ? (target.bot_id ?? null) : null;
  return terminals.filter((t) => bot === null || !t.bot_id || t.bot_id === bot);
}

/** The terminal to show: the chosen one while it still exists, else the newest. */
export function activeTerminal(
  terminals: readonly BotTerminal[],
  chosenId: string | null
): BotTerminal | null {
  return terminals.find((t) => t.id === chosenId) ?? terminals[0] ?? null;
}

/** The word key of a state (`liveView.terminals.state.<key>`). */
export function terminalStateKey(state: BotTerminalState): "running" | "waiting" | "finished" | "failed" {
  switch (state) {
    case "waiting_for_input":
      return "waiting";
    case "finished":
      return "finished";
    case "failed":
      return "failed";
    default:
      return "running";
  }
}

/** The screen as text lines: trailing blank rows dropped, never empty (a blank screen is one line). */
export function screenLines(screen: string): string[] {
  const lines = screen.replace(/\r\n?/g, "\n").split("\n");
  while (lines.length > 1 && lines[lines.length - 1]?.trim() === "") {
    lines.pop();
  }
  return lines;
}

/** Sessions that are still alive (the count in the panel's header). */
export function aliveCount(terminals: readonly BotTerminal[]): number {
  return terminals.filter((t) => t.state === "running" || t.state === "waiting_for_input").length;
}

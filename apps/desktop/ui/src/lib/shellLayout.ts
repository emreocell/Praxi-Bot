/**
 * Geometry of the app shell (card 198-B): the resizable left rail and right panel.
 * Pure functions plus a tolerant localStorage mirror, so the numbers are testable.
 */

/** Rail widths in px.  Below `RAIL_COLLAPSE_BELOW` the rail snaps to icons only. */
export const RAIL_MINI = 68;
export const RAIL_MIN = 200;
export const RAIL_MAX = 440;
export const RAIL_COLLAPSE_BELOW = 150;
export const RAIL_DEFAULT = 264;
/** A rail narrower than this shows only the characters. */
export const RAIL_MINI_BELOW = 100;

export const PANEL_MIN = 260;
export const PANEL_DEFAULT = 320;
export const PANEL_MAX_FRACTION = 0.6;
/** Sanity cap for stored widths; the live window clamps them again at render time. */
const PANEL_MAX_ABSOLUTE = 1600;

export interface ShellLayout {
  rail: number;
  panel: number;
}

export const DEFAULT_SHELL_LAYOUT: ShellLayout = { rail: RAIL_DEFAULT, panel: PANEL_DEFAULT };

export function isMiniRail(width: number): boolean {
  return width < RAIL_MINI_BELOW;
}

export type RailTier = "mini" | "narrow" | "medium" | "large" | "wide";

/** What the apps pill may show at this rail width: an icon, three logos, five logos, logos and the word. */
export function railTier(width: number): RailTier {
  if (width < RAIL_MINI_BELOW) return "mini";
  if (width < 240) return "narrow";
  if (width < 300) return "medium";
  if (width < 380) return "large";
  return "wide";
}

/** Rail width for a pointer at `x` (px from the window's left edge), `pad` = outer gap. */
export function railWidthFromPointer(x: number, pad = 0): number {
  const w = x - pad;
  if (w < RAIL_COLLAPSE_BELOW) return RAIL_MINI;
  return Math.round(Math.min(RAIL_MAX, Math.max(RAIL_MIN, w)));
}

/** The chat keeps at least this much room next to the rail and the panel. */
export const MAIN_MIN = 320;

/** Largest panel width the window allows, leaving the chat `MAIN_MIN` next to the rail. */
export function panelMax(viewportWidth: number, rail = 0): number {
  const byShare = Math.round(viewportWidth * PANEL_MAX_FRACTION);
  return Math.max(PANEL_MIN, Math.min(byShare, viewportWidth - rail - MAIN_MIN));
}

/** Panel width for a pointer at `x`, measured from the window's right edge. */
export function panelWidthFromPointer(x: number, viewportWidth: number, pad = 0, rail = 0): number {
  const w = viewportWidth - x - pad;
  return Math.round(Math.min(panelMax(viewportWidth, rail), Math.max(PANEL_MIN, w)));
}

/** Keyboard step on the rail grip: -1 narrows (snapping to icons), +1 widens. */
export function stepRail(width: number, dir: -1 | 1): number {
  if (dir < 0) return width <= RAIL_MIN ? RAIL_MINI : Math.max(RAIL_MIN, width - 16);
  return width < RAIL_MIN ? RAIL_MIN : Math.min(RAIL_MAX, width + 16);
}

/** Keyboard step on the panel grip: +1 widens, -1 narrows. */
export function stepPanel(width: number, dir: -1 | 1, viewportWidth: number, rail = 0): number {
  return Math.round(Math.min(panelMax(viewportWidth, rail), Math.max(PANEL_MIN, width + dir * 16)));
}

/** Coerce stored values: unknown shapes fall back to the defaults, numbers are clamped. */
export function normalizeShellLayout(raw: unknown): ShellLayout {
  const o = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const num = (v: unknown, fallback: number) =>
    typeof v === "number" && Number.isFinite(v) ? v : fallback;
  const rail = num(o.rail, RAIL_DEFAULT);
  const panel = num(o.panel, PANEL_DEFAULT);
  return {
    rail:
      rail < RAIL_COLLAPSE_BELOW
        ? RAIL_MINI
        : Math.round(Math.min(RAIL_MAX, Math.max(RAIL_MIN, rail))),
    panel: Math.round(Math.min(PANEL_MAX_ABSOLUTE, Math.max(PANEL_MIN, panel)))
  };
}

const KEY = "praxi.shell.layout.v1";

export function loadShellLayout(): ShellLayout {
  try {
    const raw = typeof window !== "undefined" ? window.localStorage?.getItem(KEY) : null;
    return raw ? normalizeShellLayout(JSON.parse(raw)) : { ...DEFAULT_SHELL_LAYOUT };
  } catch {
    return { ...DEFAULT_SHELL_LAYOUT };
  }
}

export function saveShellLayout(layout: ShellLayout): void {
  try {
    window.localStorage?.setItem(KEY, JSON.stringify(layout));
  } catch {
    // Storage unavailable (private mode, quota): the layout simply is not remembered.
  }
}

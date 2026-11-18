/**
 * Geometry of the app shell: the resizable left rail and right panel.
 * Pure functions plus a tolerant localStorage mirror, so the numbers are testable.
 */
import { paintedUiStyle } from "ui/surface-model";
import type { UiStyle } from "ui/theme-model";

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

/** Floating shell (glass style or a backdrop): outer padding and the gap between columns, px. */
export const FLOAT_PAD = 12;
export const FLOAT_GAP = 12;

/**
 * The shell floats in the glass and tinted styles or over a backdrop (mirrors `applyTheme`'s
 * `data-float`).  `style` is the saved style; the painted one may differ on this platform
 * (reduced transparency paints solid), which `painted` resolves.
 */
export function isFloatShell(
  style: string,
  backdropActive: boolean,
  painted: (style: UiStyle) => UiStyle = paintedUiStyle
): boolean {
  const s = painted(style as UiStyle);
  return s === "glass" || s === "tinted" || backdropActive;
}

/**
 * Horizontal room the floating chrome takes from the columns: the outer padding on both sides
 * and one gap per neighbouring pair.  A closed panel pulls its gap back with a negative margin.
 */
export function shellChrome(float: boolean, panelOpen = true): number {
  if (!float) return 0;
  return FLOAT_PAD * 2 + FLOAT_GAP * (panelOpen ? 2 : 1);
}

/** Largest panel width the window allows, leaving the chat `MAIN_MIN` next to the rail. */
export function panelMax(viewportWidth: number, rail = 0, float = false): number {
  const byShare = Math.round(viewportWidth * PANEL_MAX_FRACTION);
  const byRoom = viewportWidth - shellChrome(float) - rail - MAIN_MIN;
  return Math.max(PANEL_MIN, Math.min(byShare, byRoom));
}

/**
 * The panel's width/margin transition lasts `--motion-duration-slow`.  The rail keeps the geometry
 * of the OPEN panel for that long (plus a small margin) after the panel starts closing, so the chat
 * column never drops below `MAIN_MIN` at any frame of the transition.  The delay is derived from
 * the same token the CSS transition uses, so the two cannot drift (checked by check:layout).
 */
export const RESERVE_MARGIN_MS = 50;
/** Used only when the token cannot be read at run time. */
export const RESERVE_FALLBACK_MS = 500;

/** Parse a CSS `<time>` (`450ms`, `0.45s`) into milliseconds; undefined for anything else. */
export function parseDurationMs(text: string | undefined): number | undefined {
  const m = /^\s*(\d*\.?\d+)\s*(ms|s)\s*$/i.exec(text ?? "");
  if (!m) return undefined;
  const ms = Number(m[1]) * (m[2].toLowerCase() === "s" ? 1000 : 1);
  return Number.isFinite(ms) ? ms : undefined;
}

/** How long after closing starts the rail still reserves the open panel (0: no transition, no delay). */
export function reserveDelayMs(slideMs: number | undefined, reducedMotion: boolean): number {
  if (reducedMotion || slideMs === 0) return 0;
  return slideMs === undefined ? RESERVE_FALLBACK_MS : slideMs + RESERVE_MARGIN_MS;
}

/** Whether the panel is still reserved `msSinceClose` after it started closing. */
export function reserveActive(open: boolean, msSinceClose: number, delayMs: number): boolean {
  return open || msSinceClose < delayMs;
}

/** Room the rail may take next to the panel while the chat keeps `MAIN_MIN`. */
export function railRoom(viewportWidth: number, panelWidth: number, float: boolean, panelOpen: boolean): number {
  return viewportWidth - shellChrome(float, panelOpen) - (panelOpen ? panelWidth : 0) - MAIN_MIN;
}

/**
 * Rail width that fits next to the panel: the panel clamp wins, so a rail that would squeeze the
 * chat below `MAIN_MIN` is narrowed, and snaps to icons when it would drop below `RAIL_MIN`.
 * `panelOpen` is the panel's RESERVED state (open, or closing and not yet settled).
 */
export function fitRail(
  viewportWidth: number,
  rail: number,
  panelWidth: number,
  float: boolean,
  panelOpen: boolean
): number {
  const room = railRoom(viewportWidth, panelWidth, float, panelOpen);
  if (rail <= room) return rail;
  return room >= RAIL_MIN ? room : RAIL_MINI;
}

/**
 * Explicit keyboard resize of the rail.  The step starts from the DISPLAYED (fitted) width and the
 * result becomes the stored preference, so the arrow keys always move what the user sees.  A step
 * that would only widen the stored value beyond what fits (no visible change) leaves the stored
 * preference alone instead of silently overwriting it.
 */
export function railAfterStep(
  stored: number,
  displayed: number,
  dir: -1 | 1,
  fitted: (candidate: number) => number
): number {
  const next = stepRail(displayed, dir);
  return fitted(next) === displayed && next !== displayed ? stored : next;
}

/**
 * The Search button.  A rail the user collapsed (stored width = icons) expands as before; a rail that is
 * only collapsed by fitting keeps the stored preference and shows the field as a compact popover.
 */
export function searchPlan(
  storedRail: number,
  displayedRail: number,
  searchOpen: boolean
): { expandStored: boolean; popover: boolean; open: boolean } {
  const userMini = isMiniRail(storedRail);
  const displayedMini = isMiniRail(displayedRail);
  const expandStored = displayedMini && userMini;
  return { expandStored, popover: displayedMini && !userMini, open: !searchOpen || expandStored };
}

/** Panel width for a pointer at `x`, measured from the window's right edge. */
export function panelWidthFromPointer(
  x: number,
  viewportWidth: number,
  pad = 0,
  rail = 0,
  float = false
): number {
  const w = viewportWidth - x - pad;
  return Math.round(Math.min(panelMax(viewportWidth, rail, float), Math.max(PANEL_MIN, w)));
}

/** Keyboard step on the rail grip: -1 narrows (snapping to icons), +1 widens. */
export function stepRail(width: number, dir: -1 | 1): number {
  if (dir < 0) return width <= RAIL_MIN ? RAIL_MINI : Math.max(RAIL_MIN, width - 16);
  return width < RAIL_MIN ? RAIL_MIN : Math.min(RAIL_MAX, width + 16);
}

/** Keyboard step on the panel grip: +1 widens, -1 narrows. */
export function stepPanel(
  width: number,
  dir: -1 | 1,
  viewportWidth: number,
  rail = 0,
  float = false
): number {
  return Math.round(Math.min(panelMax(viewportWidth, rail, float), Math.max(PANEL_MIN, width + dir * 16)));
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

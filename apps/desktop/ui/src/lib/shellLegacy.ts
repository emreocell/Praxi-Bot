/**
 * The shell as it was before the shell redesign, kept as a fixture so the layout check can prove it still
 * catches the bug (mutation self-test): the inner column as wide as the panel's border box, and
 * the clamp that ignores the floating chrome and never narrows the rail.
 */
import { MAIN_MIN, PANEL_MAX_FRACTION, PANEL_MIN } from "./shellLayout.ts";
import type { ShellFns } from "./shellGeometry.ts";

export const LEGACY_SIDEBAR_CSS = `
.foregroundContainer { display: flex; }
.mainPane { min-width: var(--space-0); flex: 1; }
:global(:root[data-float="on"]) .foregroundContainer { padding: var(--space-3); gap: var(--space-3); }
.panel { position: relative; flex-shrink: 0; width: 0; overflow: hidden; }
.panel[data-open="true"] { width: var(--panel-w); border-left: var(--size-border) solid var(--color-border); }
.panelInner { width: var(--panel-w); height: 100%; }
:global(:root[data-float="on"]) .panel { margin-left: calc(var(--space-3) * -1); border: none; }
:global(:root[data-float="on"]) .panel[data-open="true"] { margin-left: 0; border: var(--size-border) solid var(--color-border); }
`;

export const LEGACY_FNS: ShellFns = {
  panelMax: (vw, rail) => Math.max(PANEL_MIN, Math.min(Math.round(vw * PANEL_MAX_FRACTION), vw - rail - MAIN_MIN)),
  fitRail: null
};

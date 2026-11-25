/**
 * Pure geometry of the v6 shell: where the rail, the chat, the right panel and the
 * panel's inner column land for a window width, using the SAME clamp functions the app uses
 * (`shellLayout.ts`) and the border / padding numbers read from the CSS (`shellContract.ts`).
 * `checkMatrix` asserts the layout invariants over the whole width x rail x panel x style grid.
 */
import { FLOAT_GAP, FLOAT_PAD, fitRail, MAIN_MIN, panelMax, PANEL_MIN, reserveActive, reserveDelayMs } from "./shellLayout.ts";
import type { ShellModel, StateKey } from "./shellContract.ts";

export interface Box {
  left: number;
  right: number;
}

export interface ShellInput {
  vw: number;
  /** Stored rail width (before the fit against the window). */
  rail: number;
  /** Stored panel width (before the clamp against the window). */
  panel: number;
  /** The panel's DOM state (`data-open`). */
  panelOpen: boolean;
  /** Whether the panel is docked in flow or overlays the main pane. Default: docked. */
  panelMode?: "docked" | "overlay";
  float: boolean;
  /** Slide progress of the panel's width/margin transition, 0 = closed width, 1 = open width. Default: settled. */
  progress?: number;
  /** Whether the rail fit still reserves the panel (open, or closing and not yet settled). Default: panelOpen. */
  reserve?: boolean;
}

export interface ShellBoxes {
  content: Box;
  rail: Box;
  main: Box;
  panel: Box;
  panelInner: Box;
  /** Right edge of the panel's content box (border-box right minus its right border). */
  panelContentRight: number;
  /** Width of the panel's content box. */
  panelContentWidth: number;
}

/** The clamp functions under test: the app's, or an older variant for the mutation self-test. */
export interface ShellFns {
  panelMax: (vw: number, rail: number, float: boolean) => number;
  fitRail: ((vw: number, rail: number, panelW: number, float: boolean, panelOpen: boolean) => number) | null;
}

export const APP_FNS: ShellFns = { panelMax, fitRail };

export function shellBoxes(input: ShellInput, model: ShellModel, fns: ShellFns = APP_FNS): ShellBoxes {
  const { vw, float, panelOpen } = input;
  const overlay = input.panelMode === "overlay";
  const reserve = overlay ? false : (input.reserve ?? panelOpen);
  const pad = float ? model.floatPad : 0;
  const gap = float ? model.floatGap : 0;
  // Target width (what the app writes into --panel-w), also while the panel is closed or closing.
  const targetW = Math.min(input.panel, fns.panelMax(vw, input.rail, float));
  const railW = fns.fitRail ? fns.fitRail(vw, input.rail, targetW, float, reserve) : input.rail;
  const openKey: StateKey = float ? "floatOpen" : "plainOpen";
  const closedKey: StateKey = float ? "floatClosed" : "plainClosed";
  const key = overlay ? openKey : panelOpen ? openKey : closedKey;
  const state = model.states[key];
  const p = input.progress ?? (panelOpen ? 1 : 0);
  // Slide: width and margin move together from the closed to the open value in docked mode;
  // in overlay mode the panel keeps its open width and translates over the right edge.
  const openW = model.states[openKey].width(targetW);
  const closedW = model.states[closedKey].width(targetW);
  const panelW = overlay ? openW : panelOpen && p === 1 ? state.width(targetW) : closedW + (openW - closedW) * p;
  const margin = overlay
    ? 0
    : model.states[closedKey].marginLeft + (model.states[openKey].marginLeft - model.states[closedKey].marginLeft) * p;
  const content = { left: pad, right: vw - pad };
  const rail = { left: content.left, right: content.left + railW };
  const mainRight = overlay ? content.right : content.right - panelW - margin - gap;
  const main = { left: rail.right + gap, right: mainRight };
  const panelLeft = overlay ? content.right - panelW : mainRight + gap + margin;
  const panel = { left: panelLeft, right: panelLeft + panelW };
  const innerW = model.innerWidth(targetW, key);
  const innerLeft = panel.left + state.borderLeft;
  return {
    content,
    rail,
    main,
    panel,
    panelInner: { left: innerLeft, right: innerLeft + innerW },
    panelContentRight: panel.right - state.borderRight,
    panelContentWidth: panelW - state.borderLeft - state.borderRight
  };
}

export const MATRIX = {
  widths: [760, 900, 1024, 1120, 1400, 1920, 2560],
  rails: [68, 200, 264, 440],
  corners: { sharp: 0.4, soft: 1, round: 1.6 } as Record<string, number>
};

/** Panel cases of the matrix: closed, the two common widths and the largest the window allows. */
export function panelCases(vw: number, rail: number, float: boolean, fns: ShellFns): Array<number | "closed"> {
  return ["closed", 260, 320, fns.panelMax(vw, rail, float)];
}

export interface MatrixResult {
  cells: number;
  violations: string[];
  /** Smallest chat width seen and the cell that produced it. */
  narrowestMain: { width: number; cell: string };
}

const EPS = 0.01;

export interface MatrixOptions {
  /** `settled`: the rail keeps the open geometry until the reserve delay has passed (the app). `immediate`: the pre-review behaviour. */
  railPolicy?: "settled" | "immediate";
  /** Override of the reserve delay (ms); default: derived from the contract's slide token exactly like the hook does. */
  reserveDelay?: (slideMs: number) => number;
}

/** Fractions of the slide sampled (width and margin are linear in progress, so the ends are the worst case). */
const SLIDE = [0, 0.25, 0.5, 0.75, 1];

export function checkMatrix(model: ShellModel, fns: ShellFns = APP_FNS, options: MatrixOptions = {}): MatrixResult {
  const policy = options.railPolicy ?? "settled";
  const violations: string[] = [];
  let cells = 0;
  let narrowest = { width: Infinity, cell: "" };
  const add = (cell: string, msg: string) => violations.push(`${cell}: ${msg}`);
  const common = (cell: string, g: ShellBoxes, vw: number, pad: number, panelVisible: boolean) => {
    const mainW = g.main.right - g.main.left;
    if (mainW < narrowest.width) narrowest = { width: mainW, cell };
    const boxes: Array<[string, Box]> = [["rail", g.rail], ["main", g.main]];
    if (panelVisible) boxes.push(["panel", g.panel]);
    for (const [name, b] of boxes) {
      if (b.left < pad - EPS || b.right > vw - pad + EPS) add(cell, `${name} [${b.left}, ${b.right}] leaves [${pad}, ${vw - pad}]`);
    }
    if (g.rail.right > g.main.left + EPS) add(cell, "rail overlaps main");
    if (panelVisible && g.main.right > g.panel.left + EPS) add(cell, "main overlaps panel");
    if (mainW < 280 - EPS) add(cell, `main ${mainW}px < 280`);
    if (vw >= 900 && mainW < MAIN_MIN - EPS) add(cell, `main ${mainW}px < MAIN_MIN ${MAIN_MIN}`);
    if (!Number.isFinite(mainW)) add(cell, "main width is not a number");
  };
  for (const vw of MATRIX.widths) {
    for (const rail of MATRIX.rails) {
      for (const float of [false, true]) {
        const pad = float ? model.floatPad : 0;
        for (const panelCase of panelCases(vw, rail, float, fns)) {
          const open = panelCase !== "closed";
          for (const [corners, scale] of Object.entries(MATRIX.corners)) {
            cells++;
            const cell = `vw=${vw} rail=${rail} panel=${panelCase} float=${float ? "on" : "off"} corners=${corners}`;
            const g = shellBoxes({ vw, rail, panel: open ? panelCase : PANEL_MIN, panelOpen: open, float }, model, fns);
            common(cell, g, vw, pad, open);
            if (open) {
              if (Math.abs(g.panel.right - (vw - pad)) > EPS) add(cell, `panel.right ${g.panel.right} != ${vw - pad}`);
              if (g.panelInner.right > g.panelContentRight + EPS) {
                add(cell, `panelInner.right ${g.panelInner.right} > panel content right ${g.panelContentRight} (clipped ${g.panelInner.right - g.panelContentRight}px)`);
              }
              const innerW = g.panelInner.right - g.panelInner.left;
              if (!(Math.abs(innerW - g.panelContentWidth) <= EPS)) {
                add(cell, `panelInner width ${innerW} != available content width ${g.panelContentWidth}`);
              }
              if (g.panelInner.right > vw + EPS) add(cell, `content reaches ${g.panelInner.right} > window ${vw}`);
              // Corners: the panel must be able to hold its own radius (12px card radius x 1.35 x scale).
              const radius = 12 * scale * 1.35;
              if (float && 2 * radius > g.panel.right - g.panel.left) add(cell, `radius ${radius}px does not fit the panel`);
            }
          }
          if (!open) continue;
          // Transition frames, both directions.  Closing: time since the close started decides, via the
          // same `reserveActive` the hook uses, whether the rail still reserves the open panel.
          const slide = model.slideMs;
          const delay = policy === "immediate" ? 0 : (options.reserveDelay ?? ((ms: number) => reserveDelayMs(ms, false)))(slide);
          const times = [...new Set([...SLIDE.map((f) => f * slide), Math.min(slide, Math.max(0, delay)), Math.min(slide, Math.max(0, delay - 1))])];
          for (const t of times) {
            const progress = Math.max(0, 1 - t / slide);
            const reserve = reserveActive(false, t, delay);
            const cell = `vw=${vw} rail=${rail} panel=${panelCase} float=${float ? "on" : "off"} closing t=${t}ms progress=${progress.toFixed(2)} (transition)`;
            const g = shellBoxes({ vw, rail, panel: panelCase, panelOpen: false, float, progress, reserve }, model, fns);
            common(cell, g, vw, pad, progress > 0);
          }
          for (const progress of SLIDE) {
            const cell = `vw=${vw} rail=${rail} panel=${panelCase} float=${float ? "on" : "off"} opening progress=${progress} (transition)`;
            const g = shellBoxes({ vw, rail, panel: panelCase, panelOpen: true, float, progress, reserve: true }, model, fns);
            common(cell, g, vw, pad, progress > 0);
          }
          // Overlay mode: opening never shrinks main or overlaps rail, and inner column stays unclipped.
          cells++;
          const overlayCell = `vw=${vw} rail=${rail} panel=${panelCase} float=${float ? "on" : "off"} mode=overlay`;
          const go = shellBoxes({ vw, rail, panel: panelCase, panelOpen: true, panelMode: "overlay", float }, model, fns);
          const closedMain = shellBoxes({ vw, rail, panel: PANEL_MIN, panelOpen: false, float }, model, fns).main;
          if (Math.abs((go.main.right - go.main.left) - (closedMain.right - closedMain.left)) > EPS) {
            add(overlayCell, `overlay changed main width from ${closedMain.right - closedMain.left} to ${go.main.right - go.main.left}`);
          }
          if (go.rail.right > go.panel.left + EPS) {
            add(overlayCell, `overlay panel [${go.panel.left}, ${go.panel.right}] overlaps rail [${go.rail.left}, ${go.rail.right}]`);
          }
          if (go.panelInner.right > go.panelContentRight + EPS) {
            add(overlayCell, `overlay panelInner.right ${go.panelInner.right} > panel content right ${go.panelContentRight}`);
          }
        }
      }
    }
  }
  return { cells, violations, narrowestMain: narrowest };
}

export { FLOAT_GAP, FLOAT_PAD };

import { readShellContract, type CssSources } from "./shellContract.ts";

export interface LayoutReport extends MatrixResult {
  contractFailures: string[];
}

/** Contract rules + the whole matrix, for the sources and clamp functions given. */
export function evaluateShell(src: CssSources, fns: ShellFns = APP_FNS, options: MatrixOptions = {}): LayoutReport {
  const { model, failures } = readShellContract(src, { floatPad: FLOAT_PAD, floatGap: FLOAT_GAP });
  return { ...checkMatrix(model, fns, options), contractFailures: failures };
}

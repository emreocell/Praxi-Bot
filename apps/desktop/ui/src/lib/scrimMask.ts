/**
 * The readable scrim under a transparent chat area: geometry of its mask, shared by
 * `App.module.css` (which paints it) and the tests (which check that no text sits on a feather).
 *
 * The scrim covers the whole pane width up to a plateau that is at least as wide as the reading
 * column plus its padding; only OUTSIDE the plateau does the mask fade (so a wide pane shows a soft
 * column edge, a narrow one a full-width scrim).  The title row at the top is covered across the
 * full width.  The plateau is `min(50%, content max / 2 + gutter)` on each side of the centre line.
 */
export const SCRIM_CONTENT_MAX = 760;
/** Room between the reading column and the plateau edge (`--space-6`), px. */
export const SCRIM_GUTTER = 24;
/** Width of the soft edge outside the plateau (`--space-16`), px. */
export const SCRIM_FEATHER = 64;
/** Rows from the top with a full-width scrim (`--space-16 + --space-8`), px. */
export const SCRIM_TOP = 96;

/** Half-width of the full-strength plateau for a pane `paneWidth` px wide. */
export function scrimPlateauHalf(paneWidth: number): number {
  return Math.min(paneWidth / 2, SCRIM_CONTENT_MAX / 2 + SCRIM_GUTTER);
}

/** Fraction of the scrim's alpha that survives the horizontal mask at `x` px from the pane's left edge. */
export function scrimCoverageX(paneWidth: number, x: number): number {
  const fromCentre = Math.abs(x - paneWidth / 2);
  const half = scrimPlateauHalf(paneWidth);
  if (fromCentre <= half) return 1;
  return Math.max(0, 1 - (fromCentre - half) / SCRIM_FEATHER);
}

/**
 * Horizontal extent of text in the pane: the reading column (at most `SCRIM_CONTENT_MAX` wide,
 * centred, inside the scroller's padding `padding` on each side).  Returns [left, right] px.
 */
export function textExtent(paneWidth: number, padding = SCRIM_GUTTER): [number, number] {
  const width = Math.min(SCRIM_CONTENT_MAX, Math.max(0, paneWidth - 2 * padding));
  const left = (paneWidth - width) / 2;
  return [left, left + width];
}

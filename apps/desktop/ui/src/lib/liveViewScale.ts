/**
 * How the remote screen sits in the viewer: the whole picture always fits the free area with its
 * aspect ratio kept (bars where the shapes differ, never cropped), or the owner picks a zoom
 * (1:1 is one remote pixel per device pixel). Pure math, shared by the render and the tests.
 *
 * The picture element is given the exact size computed here (same aspect ratio as the frame), so
 * `mapClientPointToNormalized` on its bounding box (scroll included) maps pointers in every mode.
 */

export interface Size {
  width: number;
  height: number;
}

/** `fit`: the whole screen in the free area. `zoom`: `factor` device pixels per remote pixel (1 = 1:1). */
export type ViewScale = { kind: "fit" } | { kind: "zoom"; factor: number };

export const FIT_SCALE: ViewScale = { kind: "fit" };
export const ACTUAL_SCALE: ViewScale = { kind: "zoom", factor: 1 };

/** The zoom levels the viewer steps through (1 is "actual size"). */
export const ZOOM_STEPS: readonly number[] = [0.5, 0.75, 1, 1.5, 2, 3, 4];

const EPS = 1e-6;

function valid(size: Size | null | undefined): size is Size {
  return (
    !!size &&
    Number.isFinite(size.width) &&
    Number.isFinite(size.height) &&
    size.width > 0 &&
    size.height > 0
  );
}

/**
 * The remote display's own size, which is what a "1280×800" label and 1:1 mean. The stream may be
 * scaled down for the link (960×600 for a 1280×800 display), so the frame's pixel size is only a
 * fallback. The height follows the picture's aspect ratio so the box never distorts it.
 */
export function remoteSize(frame: {
  width: number;
  height: number;
  sourceRect?: { width: number; height: number } | null;
}): Size | null {
  const picture = { width: frame.width, height: frame.height };
  if (!valid(picture)) {
    return null;
  }
  const source = frame.sourceRect && valid(frame.sourceRect) ? frame.sourceRect : null;
  if (!source) {
    return picture;
  }
  return { width: source.width, height: (source.width * picture.height) / picture.width };
}

/** "1280×800": the remote display's size, the same whatever the view or stream size is. */
export function resolutionLabel(frame: {
  width: number;
  height: number;
  sourceRect?: { width: number; height: number } | null;
}): string {
  const source = frame.sourceRect && valid(frame.sourceRect) ? frame.sourceRect : frame;
  return `${Math.round(source.width)}×${Math.round(source.height)}`;
}

/** CSS pixels per remote pixel that make `content` fit `area` whole (never cropped). */
export function fitFactor(area: Size, content: Size): number {
  if (!valid(area) || !valid(content)) {
    return 0;
  }
  return Math.min(area.width / content.width, area.height / content.height);
}

/** CSS pixels per remote pixel for a scale; `dpr` turns "device pixels" into CSS pixels. */
export function cssFactor(scale: ViewScale, area: Size, content: Size, dpr: number): number {
  if (scale.kind === "fit") {
    return fitFactor(area, content);
  }
  const ratio = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  return scale.factor / ratio;
}

export interface StageGeometry {
  /** The picture's size in CSS px. */
  width: number;
  height: number;
  /** The picture is larger than the area, so the area scrolls. */
  scrolls: boolean;
}

/** The size to draw the picture at, or `null` while the area or the frame is not known yet. */
export function stageGeometry(
  scale: ViewScale,
  area: Size,
  content: Size | null,
  dpr = 1
): StageGeometry | null {
  if (!content || !valid(area) || !valid(content)) {
    return null;
  }
  const factor = cssFactor(scale, area, content, dpr);
  const width = content.width * factor;
  const height = content.height * factor;
  return {
    width,
    height,
    scrolls: width > area.width + 0.5 || height > area.height + 0.5
  };
}

/**
 * Device pixels per remote pixel the picture is shown at, or `null` while that is not known (the
 * free area or the frame has not been measured yet).
 */
export function shownFactor(scale: ViewScale, area: Size, content: Size | null, dpr = 1): number | null {
  if (scale.kind === "zoom") {
    return scale.factor;
  }
  const ratio = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  const fit = content ? fitFactor(area, content) * ratio : 0;
  return fit > 0 ? fit : null;
}

/** The zoom shown to the owner, as a whole percent (100 = 1:1), or `null` while it is not known. */
export function zoomPercent(scale: ViewScale, area: Size, content: Size | null, dpr = 1): number | null {
  const factor = shownFactor(scale, area, content, dpr);
  return factor === null ? null : Math.round(factor * 100);
}

/**
 * Next zoom level from `scale`: from "fit" the step is taken from the factor the picture is shown
 * at right now, so the first press always changes the size (an unmeasured area counts as 1:1).
 * Stays within `ZOOM_STEPS`.
 */
export function stepZoom(
  scale: ViewScale,
  direction: 1 | -1,
  area: Size,
  content: Size | null,
  dpr = 1
): ViewScale {
  const shown = shownFactor(scale, area, content, dpr) ?? 1;
  const first = ZOOM_STEPS[0] as number;
  const last = ZOOM_STEPS[ZOOM_STEPS.length - 1] as number;
  const next =
    direction === 1
      ? ZOOM_STEPS.find((s) => s > shown + EPS) ?? last
      : [...ZOOM_STEPS].reverse().find((s) => s < shown - EPS) ?? first;
  return { kind: "zoom", factor: next };
}

/** True when `direction` cannot go any further (the first or last level of `ZOOM_STEPS`). */
export function atZoomLimit(
  scale: ViewScale,
  direction: 1 | -1,
  area: Size,
  content: Size | null,
  dpr = 1
): boolean {
  const next = stepZoom(scale, direction, area, content, dpr);
  return scale.kind === "zoom" && next.kind === "zoom" && Math.abs(next.factor - scale.factor) < EPS;
}

export function isFit(scale: ViewScale): boolean {
  return scale.kind === "fit";
}

export function isActualSize(scale: ViewScale): boolean {
  return scale.kind === "zoom" && Math.abs(scale.factor - 1) < EPS;
}

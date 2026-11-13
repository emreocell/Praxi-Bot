/**
 * Backdrop scenes (card 198-A).  The catalog is pure data so preference
 * validation and the picker do not pull in the canvas drawing code.
 */

export type BackdropGroup = "dotted" | "grain" | "glass" | "other";

export interface BackdropSceneInfo {
  id: string;
  group: BackdropGroup;
  /** Scenes with an effect render smoother (grain / flute / contour / raw); the rest are dithered. */
  fx?: "grain" | "flute" | "contour" | "raw";
  /** Theme-aware scenes depend on the active theme / accent colours. */
  themed?: boolean;
}

export const BACKDROP_SCENES: readonly BackdropSceneInfo[] = [
  { id: "ink", group: "dotted", themed: true },
  { id: "aurora", group: "dotted" },
  { id: "meadow", group: "dotted" },
  { id: "sunset", group: "dotted" },
  { id: "night", group: "dotted" },
  { id: "peach", group: "dotted" },
  { id: "lagoon", group: "dotted" },
  { id: "silk", group: "grain", fx: "grain" },
  { id: "dawn", group: "grain", fx: "grain" },
  { id: "ocean", group: "grain", fx: "grain" },
  { id: "lava", group: "grain", fx: "grain" },
  { id: "pearl", group: "grain", fx: "grain" },
  { id: "smoke", group: "grain", fx: "grain" },
  { id: "curtain", group: "grain", fx: "grain" },
  { id: "reed", group: "glass", fx: "flute" },
  { id: "reed2", group: "glass", fx: "flute" },
  { id: "reed3", group: "glass", fx: "flute" },
  { id: "contour", group: "glass", fx: "contour", themed: true },
  { id: "grid", group: "glass", fx: "raw", themed: true }
];

/** `none` = no backdrop; `custom` = the user's own image (existing background image setting). */
export const NO_BACKDROP = "none";
export const CUSTOM_BACKDROP = "custom";

/** Shown as cards on the Appearance page (plus "none" and the "+" card). */
export const FAVORITE_BACKDROPS: readonly string[] = ["ink", "aurora", "silk", "sunset"];

export function backdropInfo(id: string): BackdropSceneInfo | undefined {
  return BACKDROP_SCENES.find((s) => s.id === id);
}

export function backdropsInGroup(group: BackdropGroup): BackdropSceneInfo[] {
  return BACKDROP_SCENES.filter((s) => s.group === group);
}

/** Narrow a stored value to a known backdrop id (default: none). */
export function parseBackdrop(value: unknown): string {
  if (value === CUSTOM_BACKDROP) return CUSTOM_BACKDROP;
  return typeof value === "string" && backdropInfo(value) ? value : NO_BACKDROP;
}

export const BACKDROP_DIM_MIN = 20;
export const BACKDROP_DIM_MAX = 88;
export const DEFAULT_BACKDROP_DIM = 60;

/**
 * Stored value is the percentage of the theme background laid over the scene
 * (20..88).  The "Calm - Lively" slider shows `108 - dim`.
 */
export function parseBackdropDim(value: unknown): number {
  const n = typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
  if (!Number.isFinite(n)) return DEFAULT_BACKDROP_DIM;
  return Math.min(BACKDROP_DIM_MAX, Math.max(BACKDROP_DIM_MIN, Math.round(n)));
}

/** Slider position (calm .. lively) for a stored dim, and back. */
export const dimToSlider = (dim: number): number => BACKDROP_DIM_MIN + BACKDROP_DIM_MAX - dim;
export const sliderToDim = (slider: number): number => parseBackdropDim(BACKDROP_DIM_MIN + BACKDROP_DIM_MAX - slider);

/** A scene is active, or the own-image backdrop has an image to show. */
export function backdropIsActive(backdrop: string, backgroundUrl: string): boolean {
  if (backdrop === NO_BACKDROP) return false;
  return backdrop !== CUSTOM_BACKDROP || backgroundUrl.trim() !== "";
}

/**
 * The bot's grainy mesh gradient (design v7). The recipe is the design's: three radial gradients
 * from the bot colour (`--bot`) and two hue-shifted derivatives (`--bot2`, `--bot3`, written with
 * relative colour syntax), a slow drift and a fractal-noise grain on top.
 *
 * Two guards the mock did not need, both decided here so they can be tested without a browser:
 *  - a neutral colour (white, grey, black) has no hue to shift, so its derivatives would all be the
 *    same grey; it is given a faint cool tint instead;
 *  - lightness is kept in a middle band, so a white bot does not wash the card out on a light
 *    palette (Clay) and a near-black one does not vanish on a dark palette.
 */

export interface Oklch {
  l: number;
  c: number;
  h: number;
}

const SRGB_TO_LINEAR = (v: number): number => {
  const x = v / 255;
  return x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4;
};

/** `#rgb` / `#rrggbb` to OKLCH; `null` for anything else. */
export function hexToOklch(hex: string | undefined | null): Oklch | null {
  const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec((hex ?? "").trim());
  if (!m) {
    return null;
  }
  const full = m[1].length === 3 ? [...m[1]].map((ch) => ch + ch).join("") : m[1];
  const [r, g, b] = [0, 2, 4].map((i) => SRGB_TO_LINEAR(Number.parseInt(full.slice(i, i + 2), 16)));
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const mm = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const L = 0.2104542553 * l + 0.793617785 * mm - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * mm + 0.4505937099 * s;
  const B = 0.0259040371 * l + 0.7827717662 * mm - 0.808675766 * s;
  const c = Math.hypot(A, B);
  const h = c < 1e-4 ? 0 : ((Math.atan2(B, A) * 180) / Math.PI + 360) % 360;
  return { l: L, c, h };
}

/** Below this chroma a colour is grey for the eye: it has no hue worth shifting. */
export const NEUTRAL_CHROMA = 0.035;
/** The tint a neutral bot colour gets (a soft silver-lilac). */
const NEUTRAL_TINT = { c: 0.05, h: 268 };
const L_MIN = 0.5;
const L_MAX = 0.82;
/** On a light palette the stops are darker and richer, or the card would wash out into the page. */
const LIGHT_BAND = { min: 0.42, max: 0.62, chroma: 0.1 };
const FALLBACK: Oklch = { l: 0.7, c: 0.12, h: 270 };

/** The colour the mesh is built from: the bot's own, kept chromatic and mid-light. */
export function gradientBase(color: string | undefined | null, light = false): Oklch {
  const o = hexToOklch(color) ?? FALLBACK;
  const neutral = o.c < NEUTRAL_CHROMA;
  const lo = light ? LIGHT_BAND.min : L_MIN;
  const hi = light ? LIGHT_BAND.max : L_MAX;
  return {
    l: Math.min(hi, Math.max(lo, o.l)),
    c: Math.max(neutral ? NEUTRAL_TINT.c : o.c, light ? LIGHT_BAND.chroma : neutral ? 0 : 0.07),
    h: neutral ? NEUTRAL_TINT.h : o.h
  };
}

const num = (n: number): string => (Math.round(n * 1000) / 1000).toString();

/**
 * Inline custom properties for one gradient card. `--bot2` / `--bot3` use the design's own relative
 * colour expressions on top of the guarded `--bot`.
 */
export function gradientVars(color: string | undefined | null, light = false): Record<string, string> {
  const b = gradientBase(color, light);
  return {
    "--bot": `oklch(${num(b.l)} ${num(b.c)} ${num(b.h)})`,
    "--bot2": "oklch(from var(--bot) calc(l - .08) c calc(h + 48))",
    "--bot3": "oklch(from var(--bot) calc(l - .22) calc(c * .8) calc(h - 30))"
  };
}

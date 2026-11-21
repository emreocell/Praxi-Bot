/**
 * Pure silhouette generator for the bot characters (64x64 viewBox, y grows downwards).
 *
 * Every body is a closed Catmull-Rom spline through sampled points, so silhouettes stay soft
 * and organic: superellipse "squircles" with a little sag, taper and wobble, or hand-placed
 * control points for the drop, heart and ghost. Rotation is baked into the points, which keeps
 * the studio light (top-left) fixed in world space when a body leans.
 */

export type Point = readonly [number, number];

/** Closed Catmull-Rom spline through `points` as an SVG path (cubic Béziers, 0.01 precision). */
export function smoothClosedPath(points: readonly Point[], tension = 1): string {
  const n = points.length;
  if (n < 3) {
    return '';
  }
  const f = (v: number) => (Math.round(v * 100) / 100).toString();
  const at = (i: number) => points[((i % n) + n) % n];
  let d = `M ${f(at(0)[0])} ${f(at(0)[1])}`;
  for (let i = 0; i < n; i += 1) {
    const p0 = at(i - 1);
    const p1 = at(i);
    const p2 = at(i + 1);
    const p3 = at(i + 2);
    const c1x = p1[0] + ((p2[0] - p0[0]) / 6) * tension;
    const c1y = p1[1] + ((p2[1] - p0[1]) / 6) * tension;
    const c2x = p2[0] - ((p3[0] - p1[0]) / 6) * tension;
    const c2y = p2[1] - ((p3[1] - p1[1]) / 6) * tension;
    d += ` C ${f(c1x)} ${f(c1y)}, ${f(c2x)} ${f(c2y)}, ${f(p2[0])} ${f(p2[1])}`;
  }
  return `${d} Z`;
}

export interface Wobble {
  /** Lobes around the outline. */
  readonly k: number;
  /** Relative radial amplitude (0.05 = 5 %). */
  readonly amp: number;
  /** Phase in degrees. */
  readonly phase?: number;
  /** Only apply on the upper half (cloud puffs) when true. */
  readonly topOnly?: boolean;
}

export interface SuperBlobSpec {
  readonly cx: number;
  readonly cy: number;
  readonly rx: number;
  readonly ry: number;
  /** Superellipse exponent: 2 = ellipse, 3-4 = squircle, < 2 = rhombus-like. */
  readonly n?: number;
  /** Baked rotation in degrees (positive = clockwise on screen). */
  readonly rot?: number;
  /** Widens the lower half (soft bodies sag under their own weight). */
  readonly sag?: number;
  /** Positive widens the top, negative narrows it (trapezoid feel). */
  readonly taper?: number;
  /** Flattens the very bottom so the body sits on the ground (0..1). */
  readonly flat?: number;
  /** Horizontal shear of the top relative to the bottom (lean). */
  readonly lean?: number;
  readonly wobble?: readonly Wobble[];
  /** Sample count (default 48). */
  readonly samples?: number;
}

/** Sample a soft superellipse body with sag, taper, flat bottom, lean, wobble and rotation. */
export function superBlobPoints(spec: SuperBlobSpec): Point[] {
  const {
    cx,
    cy,
    rx,
    ry,
    n = 2,
    rot = 0,
    sag = 0,
    taper = 0,
    flat = 0,
    lean = 0,
    wobble = [],
    samples = 48
  } = spec;
  const e = 2 / n;
  const rr = (rot * Math.PI) / 180;
  const cosR = Math.cos(rr);
  const sinR = Math.sin(rr);
  const pts: Point[] = [];
  for (let i = 0; i < samples; i += 1) {
    const t = (i / samples) * Math.PI * 2;
    const c = Math.cos(t);
    const s = Math.sin(t);
    let x = Math.sign(c) * Math.abs(c) ** e;
    let y = Math.sign(s) * Math.abs(s) ** e;
    let radial = 1;
    for (const w of wobble) {
      const ph = ((w.phase ?? 0) * Math.PI) / 180;
      const weight = w.topOnly ? Math.max(0, -s) ** 0.8 : 1;
      radial += w.amp * weight * Math.cos(w.k * t + ph);
    }
    x *= radial;
    y *= radial;
    // y in [-1, 1]: -1 top, 1 bottom.
    x *= 1 + sag * Math.max(0, y) - taper * Math.min(0, y);
    if (flat > 0 && y > 0) {
      y = y * (1 - flat * 0.16 * y ** 4);
    }
    x += lean * -y;
    const px = x * rx;
    const py = y * ry;
    pts.push([cx + px * cosR - py * sinR, cy + px * sinR + py * cosR]);
  }
  return pts;
}

/**
 * Soft polygon: every corner is rounded with a quadratic arc of `radius` (or per-corner radii)
 * and every edge bulges outwards by `bulge` x its length, so faces stay convex and chubby.
 */
export function roundedPolygonPoints(
  vertices: readonly Point[],
  radius: number | readonly number[],
  bulge = 0.06,
  count = 44
): Point[] {
  const perCorner = 24;
  const perEdge = 24;
  const n = vertices.length;
  const out: Point[] = [];
  const at = (i: number) => vertices[((i % n) + n) % n];
  const rad = (i: number) => (typeof radius === 'number' ? radius : radius[((i % n) + n) % n]);
  const corner = (i: number): { a: Point; b: Point } => {
    const v = at(i);
    const prev = at(i - 1);
    const next = at(i + 1);
    const lp = Math.hypot(prev[0] - v[0], prev[1] - v[1]);
    const ln = Math.hypot(next[0] - v[0], next[1] - v[1]);
    const r = Math.min(rad(i), lp * 0.48, ln * 0.48);
    return {
      a: [v[0] + ((prev[0] - v[0]) / lp) * r, v[1] + ((prev[1] - v[1]) / lp) * r],
      b: [v[0] + ((next[0] - v[0]) / ln) * r, v[1] + ((next[1] - v[1]) / ln) * r]
    };
  };
  for (let i = 0; i < n; i += 1) {
    const v = at(i);
    const { a, b } = corner(i);
    for (let k = 0; k < perCorner; k += 1) {
      const t = k / perCorner;
      const u = 1 - t;
      out.push([u * u * a[0] + 2 * u * t * v[0] + t * t * b[0], u * u * a[1] + 2 * u * t * v[1] + t * t * b[1]]);
    }
    const nextA = corner(i + 1).a;
    const ex = nextA[0] - b[0];
    const ey = nextA[1] - b[1];
    const len = Math.hypot(ex, ey);
    // Outward normal for a clockwise (screen) polygon.
    const nx = len > 0 ? ey / len : 0;
    const ny = len > 0 ? -ex / len : 0;
    for (let k = 0; k < perEdge; k += 1) {
      const t = k / perEdge;
      const lift = Math.sin(Math.PI * t) * bulge * len;
      out.push([b[0] + ex * t + nx * lift, b[1] + ey * t + ny * lift]);
    }
  }
  return resampleClosed(out, count);
}

/** Resample a dense closed polyline to `count` points evenly spaced by arc length. */
export function resampleClosed(points: readonly Point[], count: number): Point[] {
  const n = points.length;
  const cum: number[] = [0];
  for (let i = 1; i <= n; i += 1) {
    const a = points[i - 1];
    const b = points[i % n];
    cum.push(cum[i - 1] + Math.hypot(b[0] - a[0], b[1] - a[1]));
  }
  const total = cum[n];
  const out: Point[] = [];
  let j = 0;
  for (let k = 0; k < count; k += 1) {
    const target = (k / count) * total;
    while (j < n - 1 && cum[j + 1] < target) j += 1;
    const seg = cum[j + 1] - cum[j] || 1;
    const t = (target - cum[j]) / seg;
    const a = points[j];
    const b = points[(j + 1) % n];
    out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
  }
  return out;
}

/** Rotate points around (cx, cy) by `deg` degrees (clockwise on screen). */
export function rotatePoints(points: readonly Point[], deg: number, cx = 32, cy = 32): Point[] {
  const r = (deg * Math.PI) / 180;
  const c = Math.cos(r);
  const s = Math.sin(r);
  return points.map(([x, y]) => [cx + (x - cx) * c - (y - cy) * s, cy + (x - cx) * s + (y - cy) * c] as Point);
}

export interface ShapeBounds {
  readonly minX: number;
  readonly maxX: number;
  readonly minY: number;
  readonly maxY: number;
}

export function pointBounds(points: readonly Point[]): ShapeBounds {
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const [x, y] of points) {
    minX = Math.min(minX, x);
    maxX = Math.max(maxX, x);
    minY = Math.min(minY, y);
    maxY = Math.max(maxY, y);
  }
  return { minX, maxX, minY, maxY };
}

/** Translate points so the body rests with its lowest point on `groundY` and is centred on x=32. */
export function groundPoints(points: readonly Point[], groundY = 55.5): Point[] {
  const b = pointBounds(points);
  const dx = 32 - (b.minX + b.maxX) / 2;
  const dy = groundY - b.maxY;
  return points.map(([x, y]) => [x + dx, y + dy] as Point);
}

/** Hand-placed control points (pre-grounding) for silhouettes a superellipse cannot express. */
const DROP_POINTS: readonly Point[] = [
  [41.6, 6.6], [45.8, 5.6], [48.2, 8.2], [47.8, 12.4], [46.0, 16.6], [46.4, 21.4],
  [49.6, 27.0], [52.8, 33.6], [54.0, 41.0], [52.2, 48.2], [46.4, 53.8], [37.4, 56.4],
  [26.6, 56.2], [17.2, 52.8], [11.6, 46.0], [10.8, 37.6], [14.0, 29.6], [20.0, 23.0],
  [27.0, 18.0], [33.2, 13.8], [37.6, 9.4]
];

const HEART_POINTS: readonly Point[] = [
  [32.4, 19.2], [36.4, 13.6], [42.8, 11.0], [49.4, 12.0], [54.2, 16.8], [55.8, 23.8],
  [54.0, 31.4], [49.6, 38.4], [43.6, 44.6], [37.8, 49.6], [34.0, 52.6], [31.4, 53.4],
  [28.6, 51.8], [24.0, 47.6], [16.4, 40.6], [10.8, 33.0], [8.8, 24.8], [10.4, 17.4],
  [15.2, 12.4], [21.6, 11.0], [27.8, 13.4]
];

const CLOUD_POINTS: readonly Point[] = [
  [24.6, 22.6], [28.4, 16.6], [34.6, 14.2], [40.8, 16.0], [44.6, 21.4], [45.4, 25.4],
  [50.6, 26.4], [54.6, 31.0], [55.6, 37.4], [54.0, 44.4], [49.4, 49.6], [42.0, 51.6],
  [32.0, 52.0], [22.0, 51.6], [14.6, 49.6], [10.0, 45.0], [8.6, 39.0], [10.4, 33.0],
  [15.0, 28.8], [20.2, 27.6], [22.8, 25.2]
];

const GHOST_POINTS: readonly Point[] = [
  [32.6, 8.4], [41.4, 10.6], [47.8, 16.6], [51.2, 25.0], [52.6, 34.0], [54.4, 42.4],
  [57.0, 48.6], [55.0, 54.0], [49.0, 54.6], [44.0, 50.8], [38.8, 54.2], [33.0, 55.6],
  [27.6, 53.6], [23.4, 50.2], [18.4, 53.8], [12.4, 54.6], [8.6, 50.4], [10.2, 42.4],
  [12.2, 33.0], [15.0, 23.4], [19.6, 15.2], [25.8, 10.0]
];

export interface ShapeSpec {
  readonly points: () => Point[];
  /** Spline tension for the outline (1 = Catmull-Rom). */
  readonly tension?: number;
}

/**
 * The 16 persisted ids. Several ids keep their historical name but carry a new silhouette
 * (capsule = tilted soft squircle, ears = rounded square resting on a corner, boulder = chubby
 * bean): profiles store the id, so ids never change.
 */
export const SHAPE_SPECS: Readonly<Record<string, ShapeSpec>> = {
  pebble: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 33, rx: 21, ry: 20, n: 2.15, rot: -6, sag: 0.1, taper: -0.04, flat: 0.5,
        wobble: [{ k: 2, amp: 0.03, phase: 30 }, { k: 3, amp: 0.025, phase: 80 }]
      })
  },
  cube: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 33, rx: 20, ry: 19.5, n: 3.4, rot: -5, sag: 0.06, flat: 0.4,
        wobble: [{ k: 3, amp: 0.015, phase: 40 }]
      })
  },
  diamond: {
    points: () =>
      rotatePoints(
        roundedPolygonPoints([[32, 7.5], [55, 31], [32.5, 56], [9.5, 32.5]], [11, 13, 14, 13], 0.012),
        5
      )
  },
  cloud: { points: () => CLOUD_POINTS.map((p) => p) },
  tri: {
    points: () =>
      rotatePoints(roundedPolygonPoints([[33.5, 9], [55, 53.5], [9.5, 54]], [14, 13, 13], 0.02), 4)
  },
  flower: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 32, rx: 21.5, ry: 21, n: 2, rot: -4, sag: 0.03,
        wobble: [{ k: 5, amp: 0.11, phase: 90 }],
        samples: 60
      })
  },
  gem: {
    points: () =>
      rotatePoints(
        roundedPolygonPoints(
          [[22, 11.5], [42.5, 11], [54, 32], [43.5, 54], [21.5, 54], [10.5, 32.5]],
          [7, 7, 8, 8, 8, 7],
          0.01
        ),
        -6
      )
  },
  drop: { points: () => DROP_POINTS.map((p) => p) },
  star: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 33, rx: 22.5, ry: 22, n: 2, rot: -7, sag: 0.03,
        wobble: [{ k: 5, amp: 0.17, phase: 90 }],
        samples: 60
      })
  },
  ghost: { points: () => GHOST_POINTS.map((p) => p) },
  capsule: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 32, rx: 18.5, ry: 21.5, n: 3, rot: 11, taper: 0.1, flat: 0.3,
        wobble: [{ k: 2, amp: 0.02, phase: 60 }]
      })
  },
  ears: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 32, rx: 18.5, ry: 17.5, n: 2.9, rot: -24, sag: 0.04,
        wobble: [{ k: 3, amp: 0.02, phase: 15 }]
      })
  },
  gumdrop: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 33, rx: 21, ry: 24, n: 2.15, sag: 0.12, flat: 0.55, lean: 0.1,
        wobble: [{ k: 3, amp: 0.1, phase: 270 }, { k: 2, amp: 0.025, phase: 40 }]
      })
  },
  heart: { points: () => HEART_POINTS.map((p) => p) },
  clover: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 32, rx: 21.5, ry: 21.5, n: 2, rot: -12, sag: 0.03,
        wobble: [{ k: 4, amp: 0.19, phase: 0 }],
        samples: 64
      })
  },
  boulder: {
    points: () =>
      superBlobPoints({
        cx: 32, cy: 34, rx: 23.5, ry: 18, n: 2.2, rot: 4, sag: 0.12, flat: 0.5, lean: 0.06,
        wobble: [{ k: 3, amp: 0.045, phase: 210 }, { k: 2, amp: 0.03, phase: 120 }]
      })
  }
};

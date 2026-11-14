/** All 16 3D clay character shapes (eye coordinates in a 64x64 viewBox). */
export const BOT_SHAPES = [
  { id: 'pebble', alias: 'daire', leftEyeX: 25, rightEyeX: 39, eyeY: 33 },
  { id: 'cube', alias: 'kare', leftEyeX: 25, rightEyeX: 39, eyeY: 31 },
  { id: 'diamond', alias: 'elmas', leftEyeX: 26, rightEyeX: 38, eyeY: 32 },
  { id: 'cloud', alias: 'bulut', leftEyeX: 26, rightEyeX: 39, eyeY: 38 },
  { id: 'tri', alias: 'ucgen', leftEyeX: 26, rightEyeX: 38, eyeY: 42 },
  { id: 'flower', alias: 'cicek', leftEyeX: 26.5, rightEyeX: 37.5, eyeY: 32 },
  { id: 'gem', alias: 'altigen', leftEyeX: 25, rightEyeX: 39, eyeY: 32 },
  { id: 'drop', alias: 'damla', leftEyeX: 25.5, rightEyeX: 38, eyeY: 41 },
  { id: 'star', alias: 'yildiz', leftEyeX: 26.5, rightEyeX: 37.5, eyeY: 34 },
  { id: 'ghost', alias: 'hayalet', leftEyeX: 26, rightEyeX: 38, eyeY: 30 },
  { id: 'capsule', alias: 'kapsul', leftEyeX: 28, rightEyeX: 36, eyeY: 27 },
  { id: 'ears', alias: 'kulakli', leftEyeX: 25, rightEyeX: 39, eyeY: 37 },
  { id: 'gumdrop', alias: 'jole', leftEyeX: 25, rightEyeX: 39, eyeY: 34 },
  { id: 'heart', alias: 'kalp', leftEyeX: 25, rightEyeX: 39, eyeY: 29 },
  { id: 'clover', alias: 'yonca', leftEyeX: 26.5, rightEyeX: 37.5, eyeY: 32 },
  { id: 'boulder', alias: 'kaya', leftEyeX: 25, rightEyeX: 39, eyeY: 37 }
] as const;

export type BotShapeDef = (typeof BOT_SHAPES)[number];
export type BotShapeId = BotShapeDef['id'];

export const BOT_SHAPE_IDS: readonly BotShapeId[] = BOT_SHAPES.map((s) => s.id);

/** Default clay character colour swatches matching the v6 palette. */
export const CHARACTER_COLORS = [
  '#f4f4f5',
  '#b98a5e',
  '#f2545b',
  '#f5803e',
  '#f2b233',
  '#34c77b',
  '#2fc4b2',
  '#4c8dff',
  '#9b7bff',
  '#f25fa8',
  '#8e9099'
] as const;

const DEFAULT_TINT_COLOR = '#8e9099';

const SHAPE_BY_NAME = new Map<string, BotShapeDef>();
for (const shape of BOT_SHAPES) {
  SHAPE_BY_NAME.set(shape.id, shape);
  SHAPE_BY_NAME.set(shape.alias, shape);
}

/** Deterministic FNV-1a 32-bit hash for stable identity fallbacks. */
export function fnv1a32(input: string): number {
  let hash = 2166136261;
  for (const char of input) {
    hash = Math.imul(hash ^ (char.codePointAt(0) ?? 0), 16777619) >>> 0;
  }
  return hash;
}

/** Normalize an explicit shape id or prototype alias (`daire` -> `pebble`). */
export function normalizeBotShape(shape: string | null | undefined): BotShapeId | null {
  if (typeof shape !== 'string') {
    return null;
  }
  const key = shape.trim().toLowerCase();
  return SHAPE_BY_NAME.get(key)?.id ?? null;
}

/** Deterministic fallback mapping from bot ID to one of the 16 shapes. */
export function fallbackBotShape(id: string): BotShapeId {
  return BOT_SHAPES[fnv1a32(id) % BOT_SHAPES.length].id;
}

/** Deterministic chromatic fallback tint when a caller omits `bot.color`. */
export function fallbackBotColor(id: string): string {
  const chromatic = CHARACTER_COLORS.slice(1, -1);
  return chromatic[fnv1a32(id) % chromatic.length];
}

/** Resolve character geometry (explicit shape when valid, else ID fallback). */
export function botShape(id: string, explicitShape?: string | null) {
  const hash = fnv1a32(id);
  const resolvedId = normalizeBotShape(explicitShape) ?? BOT_SHAPES[hash % BOT_SHAPES.length].id;
  const def = SHAPE_BY_NAME.get(resolvedId) ?? BOT_SHAPES[0];
  return {
    hash,
    id: def.id,
    alias: def.alias,
    leftEyeX: def.leftEyeX,
    rightEyeX: def.rightEyeX,
    eyeY: def.eyeY
  };
}

/** Normalize a CSS hex color (`#rgb`, `#rrggbb`, `#rrggbbaa`) or fall back safely. */
export function normalizeHexColor(color: string | null | undefined): string {
  if (typeof color !== 'string') {
    return DEFAULT_TINT_COLOR;
  }
  const trimmed = color.trim().toLowerCase();
  const short = /^#([0-9a-f]{3})$/.exec(trimmed);
  if (short) {
    const [r, g, b] = short[1];
    return `#${r}${r}${g}${g}${b}${b}`;
  }
  const full = /^#([0-9a-f]{6})(?:[0-9a-f]{2})?$/.exec(trimmed);
  if (full) {
    return `#${full[1]}`;
  }
  return DEFAULT_TINT_COLOR;
}

function parseHexRgb(hex: string): [number, number, number] {
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16)
  ];
}

function formatHexRgb(r: number, g: number, b: number): string {
  const clamp = (n: number) => Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, '0');
  return `#${clamp(r)}${clamp(g)}${clamp(b)}`;
}

function mixHex(a: string, b: string, t: number): string {
  const [ar, ag, ab] = parseHexRgb(a);
  const [br, bg, bb] = parseHexRgb(b);
  return formatHexRgb(ar + (br - ar) * t, ag + (bg - ag) * t, ab + (bb - ab) * t);
}

export interface GradientMapConfig {
  /** Normalized `#rrggbb` tint colour used to build the ramp. */
  color: string;
  /** Luminance-desaturation color matrix values. */
  luminanceMatrix: string;
  /** Contrast stretch slope before gradient lookup. */
  slope: string;
  /** Contrast stretch intercept before gradient lookup. */
  intercept: string;
  /** 5-stop feFuncR tableValues string. */
  tableR: string;
  /** 5-stop feFuncG tableValues string. */
  tableG: string;
  /** 5-stop feFuncB tableValues string. */
  tableB: string;
  /** Raw 5 hex stops from shadow to highlight. */
  stops: readonly [string, string, string, string, string];
}

/**
 * Compute an sRGB 5-stop gradient-map filter table for an off-white clay render
 * so shadows gain colour saturation rather than muddying toward grey.
 */
export function gradientMapConfig(color: string | null | undefined): GradientMapConfig {
  const base = normalizeHexColor(color);
  const [r, g, b] = parseHexRgb(base);
  // Squaring channels pulls mid-shadows deeper while preserving saturation.
  const saturatedShadow = formatHexRgb((r * r) / 255, (g * g) / 255, (b * b) / 255);
  const stops: [string, string, string, string, string] = [
    mixHex(saturatedShadow, '#000000', 0.38),
    mixHex(base, saturatedShadow, 0.8),
    mixHex(base, saturatedShadow, 0.38),
    base,
    mixHex(base, '#ffffff', 0.42)
  ];
  const channelTable = (index: 0 | 1 | 2) =>
    stops.map((stop) => (parseHexRgb(stop)[index] / 255).toFixed(3)).join(' ');
  return {
    color: base,
    luminanceMatrix: '.3 .59 .11 0 0 .3 .59 .11 0 0 .3 .59 .11 0 0 0 0 0 1 0',
    slope: '2.38',
    intercept: '-1.31',
    tableR: channelTable(0),
    tableG: channelTable(1),
    tableB: channelTable(2),
    stops
  };
}

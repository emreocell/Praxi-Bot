import type {
  InputEvent,
  InputModifier,
  MouseButton
} from "../api/types.ts";

export const MAX_BATCH_EVENTS = 32;
export const MAX_BATCH_BYTES = 16 * 1024;
export const MAX_PENDING_INPUT_EVENTS = 96;
export const MAX_KEY_TEXT_BYTES = 64;
export const MAX_INPUT_TEXT_BYTES = 4096;
export const WHEEL_CLAMP_PX = 1200;

const NAMED_ALLOWED_CODES = new Set<string>([
  "Enter",
  "Tab",
  "Escape",
  "Backspace",
  "Delete",
  "Space",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
  "PageUp",
  "PageDown",
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
  "CapsLock",
  "Minus",
  "Equal",
  "BracketLeft",
  "BracketRight",
  "Backslash",
  "Semicolon",
  "Quote",
  "Backquote",
  "Comma",
  "Period",
  "Slash"
]);

const textEncoder = new TextEncoder();

/** Matches `live_view::dto::allowed_code` on the daemon. */
export function isAllowedKeyCode(code: string): boolean {
  if (/^Key[A-Z]$/.test(code) || /^Digit[0-9]$/.test(code)) {
    return true;
  }
  if (/^F([1-9]|1[0-2])$/.test(code)) {
    return true;
  }
  return NAMED_ALLOWED_CODES.has(code);
}

export interface RectBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Compute the rendered image bounds inside a box sized with
 * `object-fit: contain`. Returns `null` for degenerate dimensions.
 */
export function computeContainedImageRect(
  containerWidth: number,
  containerHeight: number,
  imageWidth: number,
  imageHeight: number
): RectBox | null {
  if (
    !Number.isFinite(containerWidth) ||
    !Number.isFinite(containerHeight) ||
    !Number.isFinite(imageWidth) ||
    !Number.isFinite(imageHeight) ||
    containerWidth <= 0 ||
    containerHeight <= 0 ||
    imageWidth <= 0 ||
    imageHeight <= 0
  ) {
    return null;
  }
  const scale = Math.min(
    containerWidth / imageWidth,
    containerHeight / imageHeight
  );
  const width = imageWidth * scale;
  const height = imageHeight * scale;
  return {
    x: (containerWidth - width) / 2,
    y: (containerHeight - height) / 2,
    width,
    height
  };
}

export interface ClientBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Map DOM client coordinates `(clientX, clientY)` inside an `object-fit: contain`
 * element to normalized `[0, 1]` remote target coordinates.
 * Rejects letterbox bars (`null`) unless `opts.clamp` is true (active pointer capture).
 */
export function mapClientPointToNormalized(
  clientX: number,
  clientY: number,
  elementRect: ClientBox,
  imageWidth: number,
  imageHeight: number,
  opts: { clamp?: boolean } = {}
): { x: number; y: number } | null {
  if (!Number.isFinite(clientX) || !Number.isFinite(clientY)) {
    return null;
  }
  const inner = computeContainedImageRect(
    elementRect.width,
    elementRect.height,
    imageWidth,
    imageHeight
  );
  if (!inner) {
    return null;
  }
  const rawX = (clientX - elementRect.left - inner.x) / inner.width;
  const rawY = (clientY - elementRect.top - inner.y) / inner.height;
  if (!opts.clamp) {
    if (rawX < 0 || rawX > 1 || rawY < 0 || rawY > 1) {
      return null;
    }
    return { x: rawX, y: rawY };
  }
  return {
    x: Math.min(1, Math.max(0, rawX)),
    y: Math.min(1, Math.max(0, rawY))
  };
}

export function extractModifiers(e: {
  altKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
}): InputModifier[] {
  const mods: InputModifier[] = [];
  if (e.altKey) mods.push("alt");
  if (e.ctrlKey) mods.push("ctrl");
  if (e.metaKey) mods.push("meta");
  if (e.shiftKey) mods.push("shift");
  return mods;
}

export function domButtonToInputButton(button: number): MouseButton | null {
  switch (button) {
    case 0:
      return "left";
    case 1:
      return "middle";
    case 2:
      return "right";
    default:
      return null;
  }
}

/** Filter DOM `buttons` bitmask to Left(1) | Right(2) | Middle(4) -> 0..7. */
export function clampButtonsMask(buttons: number): number {
  if (!Number.isFinite(buttons) || buttons < 0) {
    return 0;
  }
  return Math.trunc(buttons) & 0x07;
}

/** Normalize DOM wheel delta into clamped CSS px `[-1200, 1200]`. */
export function normalizeWheelDelta(
  delta: number,
  deltaMode = 0,
  pageSizePx = 800
): number {
  if (!Number.isFinite(delta)) {
    return 0;
  }
  let px = delta;
  if (deltaMode === 1) {
    px = delta * 16;
  } else if (deltaMode === 2) {
    px = delta * pageSizePx;
  }
  return Math.max(-WHEEL_CLAMP_PX, Math.min(WHEEL_CLAMP_PX, px));
}

export function truncateUtf8(str: string, maxBytes: number): string {
  if (textEncoder.encode(str).byteLength <= maxBytes) {
    return str;
  }
  let out = "";
  let bytes = 0;
  for (const ch of str) {
    const chBytes = textEncoder.encode(ch).byteLength;
    if (bytes + chBytes > maxBytes) {
      break;
    }
    out += ch;
    bytes += chBytes;
  }
  return out;
}

/** Split text into non-empty chunks each fitting within 4096 UTF-8 bytes. */
export function chunkTextInput(text: string): string[] {
  if (text.length === 0) {
    return [];
  }
  if (textEncoder.encode(text).byteLength <= MAX_INPUT_TEXT_BYTES) {
    return [text];
  }
  const chunks: string[] = [];
  let current = "";
  let currentBytes = 0;
  for (const ch of text) {
    const chBytes = textEncoder.encode(ch).byteLength;
    if (currentBytes + chBytes > MAX_INPUT_TEXT_BYTES && current.length > 0) {
      chunks.push(current);
      current = "";
      currentBytes = 0;
    }
    if (chBytes <= MAX_INPUT_TEXT_BYTES) {
      current += ch;
      currentBytes += chBytes;
    }
  }
  if (current.length > 0) {
    chunks.push(current);
  }
  return chunks;
}

export interface KeyLikeEvent {
  code: string;
  key: string;
  location: number;
  repeat: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  /** `getModifierState("AltGraph")`; AltGr often arrives as ctrl+alt on Windows. */
  altGraph?: boolean;
}

/**
 * What a US-layout keyboard produces for each physical key (unshifted then
 * shifted). The daemon injects `key` events by physical `code`, so a key whose
 * produced character differs from this table (Turkish `ş` on `Semicolon`, `i`
 * on `Quote`, AltGr `@` on `KeyQ`, ...) must travel as committed `text`
 * instead, otherwise the remote host would type the US character.
 */
const US_PRINTABLE: Record<string, string> = {
  Backquote: "`~",
  Minus: "-_",
  Equal: "=+",
  BracketLeft: "[{",
  BracketRight: "]}",
  Backslash: "\\|",
  Semicolon: ";:",
  Quote: "'\"",
  Comma: ",<",
  Period: ".>",
  Slash: "/?",
  Space: " ",
  Digit0: "0)",
  Digit1: "1!",
  Digit2: "2@",
  Digit3: "3#",
  Digit4: "4$",
  Digit5: "5%",
  Digit6: "6^",
  Digit7: "7&",
  Digit8: "8*",
  Digit9: "9("
};

export function isUsLayoutPrintable(code: string, key: string): boolean {
  if (/^Key[A-Z]$/.test(code)) {
    return key.length === 1 && key.toLowerCase() === code.slice(3).toLowerCase();
  }
  const chars = US_PRINTABLE[code];
  return chars !== undefined && key.length === 1 && chars.includes(key);
}

function isSinglePrintable(key: string): boolean {
  return key.length > 0 && Array.from(key).length === 1;
}

export function classifyKeyDown(
  e: KeyLikeEvent,
  composing: boolean
): InputEvent[] {
  if (composing || e.isComposing || e.key === "Process" || e.keyCode === 229) {
    return [];
  }
  const printable = isSinglePrintable(e.key);
  const altGr = Boolean(e.altGraph) || (e.ctrlKey && e.altKey);
  // Plain typing and AltGr produce characters; ctrl/alt/meta chords are shortcuts.
  const producesText = printable && !e.metaKey && (altGr || (!e.ctrlKey && !e.altKey));
  if (producesText && !isUsLayoutPrintable(e.code, e.key)) {
    return chunkTextInput(e.key).map((text) => ({ type: "text", text }));
  }
  if (isAllowedKeyCode(e.code)) {
    const location =
      Number.isInteger(e.location) && e.location >= 0 && e.location <= 3
        ? e.location
        : 0;
    return [
      {
        type: "key",
        phase: "down",
        code: e.code,
        key: truncateUtf8(e.key || e.code, MAX_KEY_TEXT_BYTES) || e.code,
        location,
        repeat: Boolean(e.repeat),
        modifiers: extractModifiers(e)
      }
    ];
  }
  if (producesText && !e.ctrlKey && !e.altKey) {
    return chunkTextInput(e.key).map((text) => ({ type: "text", text }));
  }
  return [];
}

export function classifyKeyUp(
  e: KeyLikeEvent,
  composing: boolean
): InputEvent[] {
  if (composing || e.isComposing || e.key === "Process" || e.keyCode === 229) {
    return [];
  }
  if (!isAllowedKeyCode(e.code)) {
    return [];
  }
  const location =
    Number.isInteger(e.location) && e.location >= 0 && e.location <= 3
      ? e.location
      : 0;
  return [
    {
      type: "key",
      phase: "up",
      code: e.code,
      key: truncateUtf8(e.key || e.code, MAX_KEY_TEXT_BYTES) || e.code,
      location,
      repeat: false,
      modifiers: extractModifiers(e)
    }
  ];
}

/**
 * Stateful key classifier for one focus session. Never emits a key `up` that
 * was not preceded by a forwarded key `down`, and swallows the `up` of keys that
 * travelled as `text` (so the remote never sees a stray release).
 */
export class KeyForwarder {
  private readonly keys = new Set<string>();
  private readonly textKeys = new Set<string>();

  down(e: KeyLikeEvent, composing: boolean): InputEvent[] {
    const events = classifyKeyDown(e, composing);
    const first = events[0];
    if (first?.type === "key") {
      this.keys.add(e.code);
    } else if (first?.type === "text") {
      this.textKeys.add(e.code);
    }
    return events;
  }

  up(e: KeyLikeEvent, composing: boolean): InputEvent[] {
    if (this.textKeys.delete(e.code)) {
      return [];
    }
    if (!this.keys.has(e.code)) {
      return [];
    }
    const events = classifyKeyUp(e, composing);
    if (events.length > 0) {
      this.keys.delete(e.code);
    }
    return events;
  }

  reset(): void {
    this.keys.clear();
    this.textKeys.clear();
  }
}

/**
 * Bounded live input queue with move-only coalescing, pressed-key/button
 * tracking, and overflow/reset protection.
 */
export class LiveInputQueue {
  private events: InputEvent[] = [];
  private pressedCodes = new Set<string>();
  private pressedButtons = 0;
  private composing = false;
  readonly maxPending: number;

  constructor(maxPending = MAX_PENDING_INPUT_EVENTS) {
    this.maxPending = maxPending;
  }

  get length(): number {
    return this.events.length;
  }

  get hasPressedState(): boolean {
    return this.pressedCodes.size > 0 || this.pressedButtons !== 0;
  }

  get isComposing(): boolean {
    return this.composing;
  }

  setComposing(active: boolean): void {
    this.composing = active;
  }

  enqueue(event: InputEvent): { dropped: boolean } {
    if (event.type === "key") {
      if (event.phase === "down") {
        this.pressedCodes.add(event.code);
      } else {
        this.pressedCodes.delete(event.code);
      }
    } else if (event.type === "pointer") {
      this.pressedButtons = event.buttons;
      const last = this.events[this.events.length - 1];
      if (
        event.phase === "move" &&
        last?.type === "pointer" &&
        last.phase === "move"
      ) {
        // Coalesce consecutive pointer moves only; never coalesce clicks, wheel, keys or text.
        this.events[this.events.length - 1] = event;
        return { dropped: false };
      }
    } else if (event.type === "reset") {
      this.pressedCodes.clear();
      this.pressedButtons = 0;
      this.composing = false;
    }

    if (this.events.length >= this.maxPending) {
      this.reset();
      return { dropped: true };
    }
    this.events.push(event);
    return { dropped: false };
  }

  /** Replace pending non-reset queue with a single `reset` event and clear held state. */
  reset(): void {
    this.pressedCodes.clear();
    this.pressedButtons = 0;
    this.composing = false;
    this.events = [{ type: "reset" }];
  }

  clear(): void {
    this.pressedCodes.clear();
    this.pressedButtons = 0;
    this.composing = false;
    this.events = [];
  }

  /** Take at most one daemon-valid batch (<= 32 events and <= 16 KiB JSON). */
  drainBatch(envelopeFixedBytes = 512): InputEvent[] {
    if (this.events.length === 0) {
      return [];
    }
    const batch: InputEvent[] = [];
    let approxBytes = envelopeFixedBytes;
    while (batch.length < MAX_BATCH_EVENTS && batch.length < this.events.length) {
      const candidate = this.events[batch.length];
      const eventBytes = textEncoder.encode(JSON.stringify(candidate)).byteLength + 2;
      if (batch.length > 0 && approxBytes + eventBytes > MAX_BATCH_BYTES) {
        break;
      }
      batch.push(candidate);
      approxBytes += eventBytes;
    }
    this.events.splice(0, batch.length);
    return batch;
  }
}

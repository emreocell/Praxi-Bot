/**
 * The bot's own pointer, drawn over the live screen and over the desktop overlay.
 *
 * It is a marker, never an input: nothing here moves the owner's pointer or sends an event.
 * The live view stamps the bot's last position on its frames as fractions of the frame
 * (`bot_pointer: {x, y, click_seq}`); the desktop overlay places it at the centre of the target
 * the bot is working on. The maths is here so it can be tested without a screen.
 */

export interface BotCursorPose {
  /** Fraction of the frame width from the left, 0..1. */
  x: number;
  /** Fraction of the frame height from the top, 0..1. */
  y: number;
  /** Counts the bot's clicks; a ripple plays when it grows. */
  clickSeq: number;
}

export interface PlacedRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A `bot_pointer` field off the wire, or null when it is absent or malformed (a bad value is never drawn). */
export function parseBotPointer(value: unknown): BotCursorPose | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  const { x, y, click_seq: seq } = v;
  if (
    typeof x !== "number" ||
    typeof y !== "number" ||
    !Number.isFinite(x) ||
    !Number.isFinite(y) ||
    x < 0 ||
    x > 1 ||
    y < 0 ||
    y > 1
  ) {
    return null;
  }
  const clickSeq = typeof seq === "number" && Number.isSafeInteger(seq) && seq >= 0 ? seq : 0;
  return { x, y, clickSeq };
}

/** Where the cursor tip goes inside the stage, in px: the pose mapped onto the picture's rectangle. */
export function placeCursor(
  pose: Pick<BotCursorPose, "x" | "y">,
  picture: PlacedRect
): { left: number; top: number } {
  return {
    left: picture.x + pose.x * picture.width,
    top: picture.y + pose.y * picture.height
  };
}

/** Whether a click ripple is due: the counter grew since the last frame that was drawn. The first
 * frame ever seen never ripples (it would replay a click from before the viewer opened). */
export function rippleDue(previous: number | null, current: number): boolean {
  return previous !== null && current > previous;
}

/** The centre of a rectangle, where the bot's pointer rests while it works on that target. */
export function centerOf(rect: PlacedRect): { x: number; y: number } {
  return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
}

/**
 * Whether the viewer draws the bot's pointer: the frame carries one (the host leaves it out when
 * the owner switched the cursor off) and the owner is not driving (their own pointer is the only
 * one then).
 */
export function showBotCursor(opts: { pose: BotCursorPose | null; ownerDriving: boolean }): boolean {
  return opts.pose !== null && !opts.ownerDriving;
}

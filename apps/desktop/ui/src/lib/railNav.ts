/**
 * The left rail's bot list: one status dot per bot and arrow-key movement between its rows.
 */

/** What the dot on a bot's avatar says: it is working, it waits for me, or it is idle. */
export type BotDotStatus = "working" | "needsYou" | "idle";

/** The avatar state the rail already computes (see App.tsx) -> the dot. An error reads as idle: nothing runs. */
export function botDotStatus(avatarState: string): BotDotStatus {
  if (avatarState === "waiting-approval") return "needsYou";
  if (avatarState === "working" || avatarState === "thinking") return "working";
  return "idle";
}

/**
 * Index of the row to focus for a key in a list of `count` rows, or null when the key is not a move.
 * Up / Down stop at the ends (no wrap: the list sits between the header and the footer), Home / End jump.
 */
export function railNavIndex(current: number, count: number, key: string): number | null {
  if (count <= 0) return null;
  switch (key) {
    case "ArrowDown":
      return current < 0 ? 0 : Math.min(count - 1, current + 1);
    case "ArrowUp":
      return current < 0 ? count - 1 : Math.max(0, current - 1);
    case "Home":
      return 0;
    case "End":
      return count - 1;
    default:
      return null;
  }
}

import { useRef } from "react";
import { rippleDue } from "../lib/botCursor";
import styles from "./BotCursor.module.css";

interface BotCursorProps {
  /** Position of the arrow tip inside the positioned parent, in px. */
  left: number;
  top: number;
  /** The bot's click counter: a ripple plays under the tip each time it grows. Clicks that happened
   * before the cursor appeared are not replayed. Leave out where the caller draws its own ripples. */
  clickSeq?: number | null;
  /** Glide between positions and animate the ripple; off for reduced motion. */
  animate: boolean;
  testId?: string;
}

/**
 * The bot's own pointer, shown over the live screen and over the desktop (the overlay window).
 * Decorative: hidden from assistive technology and never takes pointer events.
 */
export function BotCursor({ left, top, clickSeq = null, animate, testId = "bot-cursor" }: BotCursorProps) {
  const seenAtMount = useRef(clickSeq);
  const rippleKey =
    clickSeq !== null && seenAtMount.current !== null && rippleDue(seenAtMount.current, clickSeq)
      ? clickSeq
      : null;
  return (
    <span
      className={[styles.cursor, animate ? styles.glide : ""].filter(Boolean).join(" ")}
      style={{ left: `${Math.round(left)}px`, top: `${Math.round(top)}px` }}
      aria-hidden="true"
      data-testid={testId}
    >
      {rippleKey !== null ? (
        <span key={rippleKey} className={styles.rippleAnchor}>
          <span className={[styles.ripple, animate ? styles.rippleAnimated : ""].filter(Boolean).join(" ")} />
        </span>
      ) : null}
      <svg className={styles.arrow} viewBox="0 0 24 24" focusable="false">
        <path d="M2 2 L2 19 L7 14.5 L10.5 22 L14 20.5 L10.5 13 L17.5 13 Z" />
      </svg>
    </span>
  );
}

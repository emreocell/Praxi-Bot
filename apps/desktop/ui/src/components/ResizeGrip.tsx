import { useRef, type KeyboardEvent, type PointerEvent } from "react";
import styles from "./ResizeGrip.module.css";

/**
 * Vertical drag handle on the edge of a column (card 198-B).  Pointer drags report the pointer's
 * x (frame-coalesced); arrow keys report -1 / +1 steps.  The page-wide `data-resizing` flag
 * freezes transitions and text selection while a drag is on.
 */
export function ResizeGrip({
  side,
  label,
  value,
  min,
  max,
  onDrag,
  onStep
}: {
  /** Which edge of its column the grip sits on. */
  side: "start" | "end";
  label: string;
  value: number;
  min: number;
  max: number;
  /** Pointer x in client px. */
  onDrag: (clientX: number) => void;
  /** Arrow key: -1 = pointer-left, +1 = pointer-right. */
  onStep: (dir: -1 | 1) => void;
}) {
  const frame = useRef<number | null>(null);
  const pending = useRef(0);

  const flush = () => {
    frame.current = null;
    onDrag(pending.current);
  };

  const down = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    document.documentElement.dataset.resizing = "true";
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
    pending.current = e.clientX;
    if (frame.current === null) frame.current = requestAnimationFrame(flush);
  };
  const up = (e: PointerEvent<HTMLDivElement>) => {
    if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
    e.currentTarget.releasePointerCapture(e.pointerId);
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current);
      flush();
    }
    delete document.documentElement.dataset.resizing;
  };
  const key = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      onStep(e.key === "ArrowLeft" ? -1 : 1);
    }
  };

  return (
    <div
      className={`${styles.grip} ${side === "end" ? styles.end : styles.start}`}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={Math.round(value)}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onPointerCancel={up}
      onKeyDown={key}
    />
  );
}

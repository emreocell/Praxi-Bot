import type { CSSProperties, ReactNode } from "react";
import styles from "./Skeleton.module.css";

/**
 * A quiet placeholder for something that is loading: a block with a slow highlight that passes over it,
 * in the shape of what will arrive.  Use it instead of a spinner for the first load of a list, a card or a
 * panel; keep one small spinner only for a refresh the person asked for.  It is hidden from assistive
 * technology: the container that loads says so (`aria-busy` and a label, or a `role="status"` line).
 * The highlight stops with prefers-reduced-motion and the app's own switch (global.css).
 */
export function Skeleton({
  width,
  height,
  shape = "block",
  className
}: {
  /** CSS length or percentage; default: the width of the container. */
  width?: string;
  /** CSS length; default: one text line (`--size-control-sm` for blocks). */
  height?: string;
  shape?: "block" | "line" | "circle" | "pill";
  className?: string;
}) {
  const style: CSSProperties = {};
  if (width) style.width = width;
  if (height) style.height = height;
  return <span className={[styles.skeleton, styles[shape], className ?? ""].filter(Boolean).join(" ")} style={style} aria-hidden="true" />;
}

/** `count` text-like lines; the last one is shorter, as a paragraph ends. */
export function SkeletonLines({ count = 3, className }: { count?: number; className?: string }) {
  return (
    <span className={[styles.lines, className ?? ""].filter(Boolean).join(" ")} aria-hidden="true">
      {Array.from({ length: count }, (_, i) => (
        <Skeleton key={i} shape="line" width={i === count - 1 && count > 1 ? "62%" : undefined} />
      ))}
    </span>
  );
}

/** The container of a loading placeholder: one polite status line for assistive technology, the shapes for the eyes. */
export function SkeletonRegion({ label, className, children }: { label: string; className?: string; children: ReactNode }) {
  return (
    <div className={[styles.region, className ?? ""].filter(Boolean).join(" ")} role="status" aria-busy="true">
      <span className={styles.srOnly}>{label}</span>
      {children}
    </div>
  );
}

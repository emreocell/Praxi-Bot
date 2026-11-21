import { type RefObject, useEffect, useState } from "react";
import { type ScrollEdges, scrollEdges } from "../lib/scrollEdges";

/** Tracks whether a scrolling element has content above / below the visible part. */
export function useScrollEdges(ref: RefObject<HTMLElement | null>): ScrollEdges {
  const [edges, setEdges] = useState<ScrollEdges>({ top: false, bottom: false });
  useEffect(() => {
    const el = ref.current;
    if (!el) return undefined;
    const update = () => {
      const next = scrollEdges(el.scrollTop, el.clientHeight, el.scrollHeight);
      setEdges((prev) => (prev.top === next.top && prev.bottom === next.bottom ? prev : next));
    };
    update();
    el.addEventListener("scroll", update, { passive: true });
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    observer?.observe(el);
    for (const child of Array.from(el.children)) observer?.observe(child);
    return () => {
      el.removeEventListener("scroll", update);
      observer?.disconnect();
    };
  }, [ref]);
  return edges;
}

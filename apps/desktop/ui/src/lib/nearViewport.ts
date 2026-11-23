/** One observer for all code blocks, rather than an observer per historical message. */
const waiting = new Map<Element, () => void>();
let observer: IntersectionObserver | undefined;

export function whenNearViewport(element: Element, ready: () => void): () => void {
  if (typeof IntersectionObserver === "undefined") {
    ready();
    return () => {};
  }
  observer ??= new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      const callback = waiting.get(entry.target);
      waiting.delete(entry.target);
      observer?.unobserve(entry.target);
      callback?.();
    }
  }, { rootMargin: "600px" });
  waiting.set(element, ready);
  observer.observe(element);
  return () => {
    waiting.delete(element);
    observer?.unobserve(element);
    if (waiting.size === 0) {
      observer?.disconnect();
      observer = undefined;
    }
  };
}

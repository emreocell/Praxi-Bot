export interface GazeBounds {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

export interface GazeOffset {
  /** Horizontal eye shift in percentage of the avatar box (`-maxX..maxX`). */
  readonly xPct: number;
  /** Vertical eye shift in percentage of the avatar box (`-maxY..maxY`). */
  readonly yPct: number;
}

/**
 * Pure helper computing a softly clamped eye-gaze offset (in % of the avatar's box)
 * toward a pointer position. Smoothly attenuates near center so eyes don't jitter.
 */
export function computeEyeGazeOffset(
  pointerX: number,
  pointerY: number,
  rect: GazeBounds | null | undefined,
  maxXPct = 5.2,
  maxYPct = 4.0
): GazeOffset {
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    return { xPct: 0, yPct: 0 };
  }
  const cx = rect.left + rect.width / 2;
  const cy = rect.top + rect.height / 2;
  const vx = pointerX - cx;
  const vy = pointerY - cy;
  const dist = Math.hypot(vx, vy);
  if (dist < 1) {
    return { xPct: 0, yPct: 0 };
  }
  // Smooth ease-out curve so subtle pointer movements feel natural and non-twitchy.
  const reach = Math.max(150, rect.width * 2.2);
  const t = Math.min(1, dist / reach);
  const strength = t * (2 - t);
  return {
    xPct: Number((((vx / dist) * maxXPct * strength)).toFixed(2)),
    yPct: Number((((vy / dist) * maxYPct * strength)).toFixed(2))
  };
}

export interface GazeRegistration {
  gazeEl: HTMLElement;
}

const entries = new Set<GazeRegistration>();
let activeListeners = false;
let rafId: number | null = null;
let lastPointer = { x: 0, y: 0 };
let reducedQuery: MediaQueryList | null = null;
let listeningMedia = false;

function isReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return true;
  }
  if (!reducedQuery) {
    reducedQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  }
  return reducedQuery.matches;
}

function resetGaze(el: HTMLElement): void {
  el.style.removeProperty("--gaze-x");
  el.style.removeProperty("--gaze-y");
}

function clearAllGaze(): void {
  for (const entry of entries) {
    resetGaze(entry.gazeEl);
  }
}

function applyPointerGaze(): void {
  rafId = null;
  if (isReducedMotion()) {
    return;
  }
  const { x, y } = lastPointer;
  for (const entry of entries) {
    const rect = entry.gazeEl.getBoundingClientRect();
    const { xPct, yPct } = computeEyeGazeOffset(x, y, rect);
    entry.gazeEl.style.setProperty("--gaze-x", `${xPct}%`);
    entry.gazeEl.style.setProperty("--gaze-y", `${yPct}%`);
  }
}

function onPointerMove(e: PointerEvent): void {
  lastPointer = { x: e.clientX, y: e.clientY };
  if (rafId === null) {
    rafId = window.requestAnimationFrame(applyPointerGaze);
  }
}

function onPointerLeave(): void {
  clearAllGaze();
}

function syncControllers(): void {
  if (typeof window === "undefined") {
    return;
  }
  const reduced = isReducedMotion();
  if (reduced) {
    clearAllGaze();
  }
  if (entries.size > 0 && !listeningMedia) {
    listeningMedia = true;
    reducedQuery?.addEventListener?.("change", syncControllers);
  } else if (entries.size === 0 && listeningMedia) {
    listeningMedia = false;
    reducedQuery?.removeEventListener?.("change", syncControllers);
  }
  const wantActive = entries.size > 0 && !reduced;
  if (wantActive && !activeListeners) {
    activeListeners = true;
    window.addEventListener("pointermove", onPointerMove, { passive: true });
    document.documentElement.addEventListener("pointerleave", onPointerLeave, { passive: true });
  } else if (!wantActive && activeListeners) {
    activeListeners = false;
    window.removeEventListener("pointermove", onPointerMove);
    document.documentElement.removeEventListener("pointerleave", onPointerLeave);
    if (rafId !== null) {
      window.cancelAnimationFrame(rafId);
      rafId = null;
    }
  }
}

/**
 * Subscribe a featured (`hero` / `panel` / `lg`) avatar's gaze layer to gentle
 * pointer-following eye movement. Never called for list items; stops automatically
 * under `prefers-reduced-motion`.
 */
export function registerCharacterGaze(reg: GazeRegistration): () => void {
  entries.add(reg);
  syncControllers();
  return () => {
    resetGaze(reg.gazeEl);
    entries.delete(reg);
    syncControllers();
  };
}

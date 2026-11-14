type IdleGesture = "hop" | "tilt" | "stretch";

const IDLE_LOOKS: readonly [number, number][] = [
  [3.2, -2.4],
  [3.2, 2.2],
  [-3.2, 0],
  [0, 0],
  [0, 0],
  [-2.6, -2.2],
  [0, 2.6],
  [2.4, 0]
];

const GESTURE_CLASSES: readonly IdleGesture[] = ["hop", "tilt", "stretch"];

export interface GazeRegistration {
  gazeEl: SVGGElement;
  characterEl: HTMLSpanElement;
  isLive: boolean;
  motionClasses: Record<IdleGesture, string>;
}

const entries = new Set<GazeRegistration>();
let activeListeners = false;
let idleTimer: number | null = null;
let rafId: number | null = null;
let lastPointer = { x: 0, y: 0, movedAt: 0 };
let reducedQuery: MediaQueryList | null = null;

function isReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return true;
  }
  if (!reducedQuery) {
    reducedQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  }
  return reducedQuery.matches;
}

function clearGazeTransforms(): void {
  for (const entry of entries) {
    entry.gazeEl.style.transform = "";
    for (const key of GESTURE_CLASSES) {
      const cls = entry.motionClasses[key];
      if (cls) {
        entry.characterEl.classList.remove(cls);
      }
    }
  }
}

function applyPointerGaze(): void {
  rafId = null;
  if (isReducedMotion()) {
    return;
  }
  const { x, y } = lastPointer;
  for (const entry of entries) {
    const rect = entry.gazeEl.ownerSVGElement?.getBoundingClientRect();
    if (!rect || rect.width === 0 || rect.height === 0) {
      continue;
    }
    const cx = rect.left + rect.width / 2;
    const cy = rect.top + rect.height / 2;
    const vx = x - cx;
    const vy = y - cy;
    const dist = Math.hypot(vx, vy) || 1;
    const scale = Math.min(1, dist / 140);
    const dx = (vx / dist) * 3.6 * scale;
    const dy = (vy / dist) * 2.8 * scale;
    entry.gazeEl.style.transform = `translate(${dx.toFixed(1)}px, ${dy.toFixed(1)}px)`;
  }
}

function onPointerMove(e: PointerEvent): void {
  lastPointer = { x: e.clientX, y: e.clientY, movedAt: Date.now() };
  if (rafId === null) {
    rafId = window.requestAnimationFrame(applyPointerGaze);
  }
}

function onIdleTick(): void {
  if (isReducedMotion() || entries.size === 0) {
    return;
  }
  if (Date.now() - lastPointer.movedAt <= 2500) {
    return;
  }
  const pool = [...entries];
  const pick = pool[Math.floor(Math.random() * pool.length)];
  if (!pick) {
    return;
  }
  if (Math.random() < 0.55) {
    const [dx, dy] = IDLE_LOOKS[Math.floor(Math.random() * IDLE_LOOKS.length)];
    pick.gazeEl.style.transform = `translate(${dx}px, ${dy}px)`;
  }
  if (pick.isLive && Math.random() < 0.3) {
    const move = GESTURE_CLASSES[Math.floor(Math.random() * GESTURE_CLASSES.length)];
    const targetCls = pick.motionClasses[move];
    if (!targetCls) {
      return;
    }
    for (const k of GESTURE_CLASSES) {
      const c = pick.motionClasses[k];
      if (c) pick.characterEl.classList.remove(c);
    }
    // Restart one-shot keyframe animation if the same gesture recurs.
    void pick.characterEl.offsetWidth;
    pick.characterEl.classList.add(targetCls);
    window.setTimeout(() => {
      pick.characterEl.classList.remove(targetCls);
    }, 1400);
  }
}

let listeningMedia = false;

function syncControllers(): void {
  if (typeof window === "undefined") {
    return;
  }
  const reduced = isReducedMotion();
  if (reduced) {
    clearGazeTransforms();
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
    idleTimer = window.setInterval(onIdleTick, 1900);
  } else if (!wantActive && activeListeners) {
    activeListeners = false;
    window.removeEventListener("pointermove", onPointerMove);
    if (idleTimer !== null) {
      window.clearInterval(idleTimer);
      idleTimer = null;
    }
    if (rafId !== null) {
      window.cancelAnimationFrame(rafId);
      rafId = null;
    }
  }
}

/**
 * Subscribe a mounted character's eye-gaze group to cursor tracking and ambient
 * idle look/gesture motion. Automatically stops under `prefers-reduced-motion`.
 */
export function registerCharacterGaze(reg: GazeRegistration): () => void {
  entries.add(reg);
  syncControllers();
  return () => {
    entries.delete(reg);
    syncControllers();
  };
}

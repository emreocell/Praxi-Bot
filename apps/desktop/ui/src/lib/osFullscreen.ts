import { getCurrentWindow } from "@tauri-apps/api/window";

/** The same test as `isTauriRuntime` in overlay/events.ts, kept here so this module loads without the app's event bridge (and in plain Node tests). */
function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);
}

/**
 * True fullscreen of the app window (F11 style), for the live view's toolbar. The desktop shell asks
 * its own window; a plain browser falls back to the Fullscreen API. A failure (a refused request,
 * a missing grant) leaves the window as it is and returns the state that is true afterwards.
 */
export async function isOsFullscreen(): Promise<boolean> {
  try {
    if (isTauriRuntime()) {
      return await getCurrentWindow().isFullscreen();
    }
  } catch {
    return false;
  }
  return typeof document !== "undefined" && document.fullscreenElement !== null;
}

export async function setOsFullscreen(on: boolean): Promise<boolean> {
  try {
    if (isTauriRuntime()) {
      await getCurrentWindow().setFullscreen(on);
    } else if (on) {
      await document.documentElement.requestFullscreen();
    } else if (document.fullscreenElement) {
      await document.exitFullscreen();
    }
  } catch {
    // Refused (no user gesture, no permission): keep the current state.
  }
  return isOsFullscreen();
}

/**
 * Calls `onChange` when the window enters or leaves fullscreen by any means (F11, the OS, Esc), and
 * only when the state really changed. The frameless desktop window may fire neither `resize` nor
 * `fullscreenchange` on the page, so the shell's own resize event is followed too (as the title bar
 * does for maximize). Events that arrive while a check is running are folded into one more check
 * after it, so the last event always wins and the checks never pile up.
 */
export function watchOsFullscreen(onChange: (on: boolean) => void): () => void {
  if (typeof window === "undefined") {
    return () => {};
  }
  let stopped = false;
  let running = false;
  let again = false;
  let last: boolean | undefined;
  let unlisten: (() => void) | undefined;
  const refresh = async () => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    try {
      do {
        again = false;
        const on = await isOsFullscreen();
        if (!stopped && on !== last) {
          last = on;
          onChange(on);
        }
      } while (again && !stopped);
    } finally {
      running = false;
    }
  };
  const onEvent = () => void refresh();
  window.addEventListener("resize", onEvent);
  if (typeof document !== "undefined") {
    document.addEventListener("fullscreenchange", onEvent);
  }
  if (isTauriRuntime()) {
    try {
      void getCurrentWindow()
        .onResized(onEvent)
        .then((fn) => {
          if (stopped) fn();
          else unlisten = fn;
        })
        .catch(() => {});
    } catch {
      // No window handle: the page's own events still work.
    }
  }
  return () => {
    stopped = true;
    window.removeEventListener("resize", onEvent);
    if (typeof document !== "undefined") {
      document.removeEventListener("fullscreenchange", onEvent);
    }
    unlisten?.();
  };
}

/** What the fullscreen controller needs from the window; the real one is `osFullscreenBackend`. */
export interface FullscreenBackend {
  /** Asks for the mode and returns the state that is true afterwards. */
  set(on: boolean): Promise<boolean>;
  read(): Promise<boolean>;
  watch(onChange: (on: boolean) => void): () => void;
}

export const osFullscreenBackend: FullscreenBackend = {
  set: setOsFullscreen,
  read: isOsFullscreen,
  watch: watchOsFullscreen
};

export interface FullscreenController {
  /** Start following the window; call `disable` (once) to stop. Idempotent while enabled. */
  enable(): void;
  /** Stop following and leave a fullscreen this controller entered. Safe to call when not enabled. */
  disable(): void;
  toggle(): void;
  exit(): void;
}

/**
 * Owns "the live view's fullscreen" so it can be tested without React. Rules:
 * - only a fullscreen entered through `toggle` is undone by `disable` (leaving the view, or the
 *   view unmounting); a window that was already fullscreen when the view opened stays that way;
 * - "entered" is recorded only once the window really is fullscreen, and cleared when it leaves;
 * - an answer that arrives after `disable` never touches the state, and a fullscreen it just
 *   produced is undone at once, so a view that went away mid-request cannot strand the window.
 */
export function createFullscreenController(
  backend: FullscreenBackend,
  onChange: (on: boolean) => void
): FullscreenController {
  let enabled = false;
  let generation = 0;
  let current = false;
  let enteredHere = false;
  let stopWatching: (() => void) | undefined;

  const publish = (on: boolean) => {
    current = on;
    if (!on) {
      enteredHere = false;
    }
    onChange(on);
  };

  const ask = (on: boolean) => {
    const mine = generation;
    void backend.set(on).then(
      (now) => {
        if (mine !== generation || !enabled) {
          // The view went away while the window was changing: do not leave it stranded.
          if (now && on) {
            void backend.set(false).catch(() => {});
          }
          return;
        }
        if (on && now) {
          enteredHere = true;
        }
        publish(now);
      },
      () => {}
    );
  };

  return {
    enable() {
      if (enabled) {
        return;
      }
      enabled = true;
      generation += 1;
      const mine = generation;
      void backend.read().then(
        (on) => {
          if (mine === generation && enabled) {
            publish(on);
          }
        },
        () => {}
      );
      stopWatching = backend.watch((on) => {
        if (mine === generation && enabled) {
          publish(on);
        }
      });
    },
    disable() {
      if (!enabled) {
        return;
      }
      enabled = false;
      generation += 1;
      stopWatching?.();
      stopWatching = undefined;
      const undo = enteredHere;
      enteredHere = false;
      if (current) {
        current = false;
        onChange(false);
      }
      if (undo) {
        void backend.set(false).catch(() => {});
      }
    },
    toggle() {
      if (enabled) {
        ask(!current);
      }
    },
    exit() {
      if (enabled) {
        ask(false);
      }
    }
  };
}

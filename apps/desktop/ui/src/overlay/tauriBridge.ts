/**
 * Reference-counted Tauri -> window event bridge.
 *
 * Every `subscribeOverlayEvents` caller listens to the window `CustomEvent`s.
 * The Tauri IPC listeners that republish onto the window must exist exactly
 * once per window, otherwise N subscribers cause N republishes and every
 * subscriber receives each event N times.  The first `acquire()` installs the
 * listeners, the last `release` removes them.
 *
 * Kept free of Tauri imports (the `listen` function is injected) so it can be
 * unit-tested under plain Node.
 */

export type Unlisten = () => void;
export type ListenFn = (
  event: string,
  handler: (e: { payload: unknown }) => void
) => Promise<Unlisten>;

export interface BridgeRoute {
  /** Tauri wire event name. */
  wire: string;
  /** Called with the payload; republishes onto the window. */
  publish: (payload: unknown) => void;
}

export interface TauriBridge {
  /** Take a reference; the returned function releases it (idempotent). */
  acquire: () => () => void;
  /** Current number of references (for tests). */
  refs: () => number;
}

export function createTauriBridge(
  listen: ListenFn,
  routes: readonly BridgeRoute[]
): TauriBridge {
  let refs = 0;
  let generation = 0;
  let unlistens: Unlisten[] = [];

  const start = () => {
    const mine = ++generation;
    for (const route of routes) {
      void listen(route.wire, (e) => {
        if (mine === generation && refs > 0) route.publish(e.payload);
      })
        .then((u) => {
          // Released (or restarted) before the async registration finished.
          if (mine !== generation || refs === 0) u();
          else unlistens.push(u);
        })
        .catch(() => {
          /* listener registration failed; nothing to clean up */
        });
    }
  };

  const stop = () => {
    generation++;
    const done = unlistens;
    unlistens = [];
    for (const u of done) u();
  };

  return {
    acquire: () => {
      refs++;
      if (refs === 1) start();
      let released = false;
      return () => {
        if (released) return;
        released = true;
        refs--;
        if (refs === 0) stop();
      };
    },
    refs: () => refs
  };
}

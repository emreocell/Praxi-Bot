/**
 * Loads one thing for the SELECTED computer.
 *
 * A result is applied only while it still belongs to the current target and is the newest
 * request: a slow answer from the host that arrives after the user went back to this computer
 * is dropped, and switching target clears what the previous one showed.
 */
export interface ScopedLoader<T> {
  /** Declare the current target; a change clears the shown value. */
  switchTo: (target: unknown) => void;
  /** Fetch for `target`; resolves `true` when the result was applied. */
  load: (target: unknown, fetcher: () => Promise<T>) => Promise<boolean>;
}

export function createScopedLoader<T>(
  apply: (value: T | null) => void,
  onError?: (error: unknown) => void
): ScopedLoader<T> {
  let scope: unknown = null;
  let seq = 0;
  const switchTo = (target: unknown) => {
    if (target !== scope) {
      scope = target;
      seq += 1;
      apply(null);
    }
  };
  return {
    switchTo,
    async load(target, fetcher) {
      switchTo(target);
      const mine = ++seq;
      try {
        const value = await fetcher();
        if (mine !== seq || target !== scope) {
          return false;
        }
        apply(value);
        return true;
      } catch (e) {
        if (mine === seq && target === scope) {
          onError?.(e);
        }
        return false;
      }
    }
  };
}

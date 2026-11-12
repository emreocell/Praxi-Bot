/** `base_url` of the connection the mock mode hands out. */
export const MOCK_BASE_URL = "mock://praxi";

/**
 * Mock mode: design review, screenshots and demos without a daemon.  Opt-in
 * with `?mock=1` in dev builds or `VITE_MOCK=1` at build time.
 */
export function isMockMode(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  // Use direct `import.meta.env` access so Vite can replace `VITE_MOCK` at build time.
  if (import.meta.env.VITE_MOCK === "1") {
    return true;
  }
  return (
    import.meta.env.DEV &&
    new URLSearchParams(window.location.search).get("mock") === "1"
  );
}


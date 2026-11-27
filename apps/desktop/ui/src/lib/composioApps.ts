import type { ComposioApp, ComposioToolkit } from "../api/types";

/** Hosts that serve a hosted connect link. */
const LINK_HOSTS = new Set(["connect.composio.dev", "app.composio.dev"]);
const DASHBOARD_HOSTS = new Set(["platform.composio.dev", "dashboard.composio.dev"]);
/** Printable ASCII without the backslash: no space, control, escape-like or look-alike character. */
const PLAIN_URL = /^[\x21-\x5b\x5d-\x7e]+$/;

/**
 * Whether `href` may be opened in the system browser. A connect link is a hosted link:
 * `https://<host>/link/<token>` with the host exactly `connect.composio.dev` or `app.composio.dev`
 * (no user, port or escapes), a token of `^[A-Za-z0-9_-]{1,128}$`, and only a query or fragment
 * after it. With `dashboard`, the key page: https on the platform or dashboard host.
 *
 * The same rule, with the same cases (`crates/composio/src/link_vectors.json`), guards the
 * daemon (`is_composio_https_url`) and the desktop shell's browser opener.
 */
export function isComposioLink(href: string, dashboard = false): boolean {
  const match = PLAIN_URL.test(href) ? /^https:\/\/([^/]*)(\/[^?#]*)?/.exec(href) : null;
  if (!match) {
    return false;
  }
  const [, host = "", path = ""] = match;
  if (dashboard) {
    return DASHBOARD_HOSTS.has(host);
  }
  const token = /^\/link\/([A-Za-z0-9_-]{1,128})$/.exec(path)?.[1];
  return LINK_HOSTS.has(host) && token !== undefined && isParsedAs(href, host);
}

/** The URL parser agrees on the host (no userinfo, port or escape slipped through the split). */
function isParsedAs(href: string, host: string): boolean {
  try {
    const url = new URL(href);
    return url.protocol === "https:" && url.host === host;
  } catch {
    return false;
  }
}

/** Slugs of apps whose sign-in is not finished. */
export function pendingSlugs(apps: readonly ComposioApp[]): string[] {
  return apps.filter((a) => a.state === "pending").map((a) => a.slug);
}

/** Replace (or add) one app in the list, keeping the order. */
export function upsertApp(apps: readonly ComposioApp[], app: ComposioApp): ComposioApp[] {
  const i = apps.findIndex((a) => a.slug === app.slug);
  if (i < 0) {
    return [...apps, app];
  }
  const next = [...apps];
  next[i] = app;
  return next;
}

/** The catalog without the apps already in the connected list. */
export function withoutConnected(
  items: readonly ComposioToolkit[],
  apps: readonly ComposioApp[]
): ComposioToolkit[] {
  const mine = new Set(apps.map((a) => a.slug));
  return items.filter((t) => !mine.has(t.slug));
}

/** Append a page, dropping slugs that are already listed. */
export function appendPage(
  items: readonly ComposioToolkit[],
  page: readonly ComposioToolkit[]
): ComposioToolkit[] {
  const seen = new Set(items.map((t) => t.slug));
  const next = [...items];
  for (const toolkit of page) {
    if (!seen.has(toolkit.slug)) {
      seen.add(toolkit.slug);
      next.push(toolkit);
    }
  }
  return next;
}

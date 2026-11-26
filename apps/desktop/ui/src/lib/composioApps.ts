import type { ComposioApp, ComposioToolkit } from "../api/types";

/**
 * Whether `href` is an https link on Composio's own domain, without
 * credentials or a custom port.  Connect links and the key page are opened in
 * the system browser only when this holds.
 */
export function isComposioLink(href: string, dashboard = false): boolean {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return false;
  }
  const host = href.match(/^https:\/\/([^/?#]+)/)?.[1];
  const hostedLink = /^https:\/\/app\.composio\.dev\/link\/[a-zA-Z0-9_-]{1,128}(?:[?#]|$)/.test(href);
  return (
    url.protocol === "https:" &&
    url.username === "" &&
    url.password === "" &&
    url.port === "" &&
    !/[\p{Cc}\\]/u.test(href) &&
    (dashboard
      ? host === "platform.composio.dev" || host === "dashboard.composio.dev"
      : host === "connect.composio.dev" || host === "backend.composio.dev" || hostedLink)
  );
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

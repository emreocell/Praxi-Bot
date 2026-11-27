import { invoke } from "@tauri-apps/api/core";
import { isComposioLink } from "./composioApps";
import { isGoogleConsoleLink } from "./googleSetup";
import { sanitizeUrl } from "./markdown";
import type { RelayApi } from "./mcpSignIn";

/**
 * Open a link in the system browser, never inside the app window. Only
 * http(s) / mailto pass `sanitizeUrl`; anything else is ignored.
 */
export function openExternal(href: string): boolean {
  const url = sanitizeUrl(href);
  if (!url || typeof window === "undefined") {
    return false;
  }
  window.open(url, "_blank", "noopener,noreferrer");
  return true;
}

/** Open a Composio link (connect page, key page) in the system browser; any other URL is refused. */
export async function openComposioLink(href: string, dashboard = false): Promise<boolean> {
  if (!isComposioLink(href, dashboard)) return false;
  if (typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window)) {
    // Plain browser (dev server, design review): the page opens in a new tab.
    window.open(href, "_blank", "noopener,noreferrer");
    return true;
  }
  try {
    await invoke("open_composio_link", { href, dashboard });
    return true;
  } catch {
    return false;
  }
}

/** A plain browser tab (dev server, a web client of a server), where no Tauri command exists. */
function browserOnly(): boolean {
  return typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
}

/**
 * Open a sign-in page in the system browser with the app's own opener. Only https addresses; `false`
 * when the page could not be opened, so the caller can show the address instead.
 *
 * The opener accepts any https host, so what goes in decides how safe it is. Only two kinds of
 * address may be passed: the `authorization_url` the daemon returned for a sign-in the owner started
 * (the sign-in panel shows its origin and asks first when it is not the app's own site), and a page
 * of the setup guide (`openGoogleConsoleLink`). Never an address read from a tool result, a catalog
 * `homepage` or any other text a server supplies.
 */
export async function openSignInLink(href: string): Promise<boolean> {
  if (!/^https:\/\//i.test(href)) return false;
  if (browserOnly()) {
    window.open(href, "_blank", "noopener,noreferrer");
    return true;
  }
  try {
    await invoke("open_sign_in_link", { href });
    return true;
  } catch {
    return false;
  }
}

/** A page of the Google Cloud console (the setup guide); any other address is refused. */
export async function openGoogleConsoleLink(href: string): Promise<boolean> {
  return isGoogleConsoleLink(href) ? openSignInLink(href) : false;
}

/**
 * Catching a sign-in redirect on this computer, for a daemon that runs on another one. `null` in a
 * plain browser tab: it cannot listen, so the sign-in asks for the address instead.
 */
export function oauthRelay(): RelayApi | null {
  if (browserOnly()) return null;
  return {
    listen: () => invoke<{ id: number; port: number }>("oauth_relay_listen"),
    wait: (id, state) => invoke<string>("oauth_relay_wait", { id, state }),
    cancel: (id) => invoke<void>("oauth_relay_cancel", { id })
  };
}

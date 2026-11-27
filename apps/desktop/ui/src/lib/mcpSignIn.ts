/**
 * Signing in to an HTTP MCP app (Google, Linear, ...), as a small state machine.
 *
 * The sign-in page opens in the browser of THIS computer. Where the daemon runs decides how the
 * answer gets back to it:
 *
 *  - `local`: the daemon is on this computer and listens for the redirect itself; the flow polls
 *    until it finishes.
 *  - `relay`: the daemon is on a server. The app listens on its own loopback port, the daemon is
 *    told that port, and the redirect the browser lands on here is forwarded to the daemon over the
 *    paired channel. Nothing is copied between machines.
 *  - `paste`: a server, but this window cannot listen (a plain browser tab, a port that would not
 *    open). The panel says so and asks for the address of the page the browser ends on.
 *
 * The flow only moves the redirect along; the daemon checks its `state` and address and decides
 * whether the person is signed in. Nothing here is a credential.
 */

import type { McpAuthStart, McpAuthState, McpCatalogEntry, McpServer } from "../api/types.ts";
import { humanError } from "./humanError.ts";

export type SignInMode = "local" | "relay" | "paste";

export type SignInReason =
  /** This desktop is not yet confirmed as an administrator on the server. */
  | "admin_needed"
  /** The owner key is missing (start Praxi from the desktop app). */
  | "owner_needed"
  /** No answer from the daemon / the server. */
  | "unreachable"
  /** Another sign-in of this app is already running. */
  | "already_running"
  /** The daemon is not waiting for a redirect (it expired or was cancelled). */
  | "not_waiting"
  /** The address is not this sign-in's redirect (wrong page, wrong sign-in, old attempt). */
  | "wrong_address"
  /** The person said no on the sign-in page, or the provider refused. */
  | "denied"
  /** The provider did not accept the one-time code. */
  | "code_rejected"
  /** The provider refused the access this app asks for. */
  | "scope_refused"
  /** The provider's sign-in service could not be found or reached. */
  | "provider_unreachable"
  /** The sign-in answered with another provider than the one the client belongs to. */
  | "issuer_mismatch"
  /** The redirect could not be caught on this computer (port in use, blocked). */
  | "relay_failed"
  | "timeout"
  | "unknown";

export type SignInStep =
  | { step: "starting" }
  /** The page is (or should be) open; waiting for the person to finish there. */
  | {
      step: "browser";
      url: string;
      opened: boolean;
      mode: SignInMode;
      redirectUri: string;
      /** Where the page is (`scheme://host`), shown to the person before they open it. */
      origin: string;
      /** The page is not on the app's own site: it is opened only after an explicit confirm. */
      needsConfirm: boolean;
      /** The address the person pasted last is not this sign-in's (it was not sent on). */
      wrongPaste?: boolean;
    }
  /** The redirect arrived; the daemon is exchanging the code. */
  | { step: "finishing" }
  | { step: "done" }
  | { step: "failed"; reason: SignInReason; detail?: string }
  | { step: "cancelled" }
  | { step: "expired" };

export type SignInState = SignInStep & { name: string };

/** Catching a redirect on this computer (the Tauri commands behind it). */
export interface RelayApi {
  listen(): Promise<{ id: number; port: number }>;
  wait(id: number, state: string): Promise<string>;
  cancel(id: number): Promise<void>;
}

export interface SignInDeps {
  /** The daemon runs on another computer than this app. */
  remote: boolean;
  /**
   * The hosts the sign-in page is expected on: the issuer the app is configured with or, without
   * one, the host of the app's own address. A page anywhere else is not opened by itself.
   */
  expectedHosts: readonly string[];
  start(relayPort?: number): Promise<McpAuthStart>;
  status(): Promise<McpAuthState>;
  /** Hand the redirect address to the daemon; resolves with the final state. */
  complete(redirectUrl: string): Promise<McpAuthState>;
  cancel(): Promise<McpAuthState>;
  /** Open `url` in the system browser; `false` = it could not be opened. */
  open(url: string): Promise<boolean>;
  /** `null` where this window cannot listen (a plain browser). */
  relay: RelayApi | null;
  sleep(ms: number): Promise<void>;
  now(): number;
}

/** What the caller flips while the flow runs. */
export interface SignInControl {
  cancelled: boolean;
  /** An address the person pasted (the `paste` mode, or a fallback while relaying). */
  pasted: string | null;
}

export const SIGN_IN_POLL_MS = 1000;
/** The most a sign-in is waited for, whatever the daemon says. */
export const SIGN_IN_MAX_MS = 10 * 60 * 1000;

/** The `state` of an authorization URL (the one the daemon will compare). */
export function stateOf(authorizationUrl: string): string | null {
  try {
    return new URL(authorizationUrl).searchParams.get("state") || null;
  } catch {
    return null;
  }
}

/** `scheme://host[:port]` and the bare host of an address; `null` when it is not a URL. */
export function originOf(address: string): { origin: string; host: string } | null {
  try {
    const url = new URL(address);
    return { origin: url.origin, host: url.hostname.toLowerCase() };
  } catch {
    return null;
  }
}

/** The host of an address or of a bare host name, lower case; `null` when there is none. */
export function hostOf(addressOrHost: string): string | null {
  return originOf(addressOrHost.includes("://") ? addressOrHost : `https://${addressOrHost}`)?.host ?? null;
}

/**
 * The same site: equal hosts, or one is a subdomain of the other (`accounts.example.com` and
 * `example.com`). The shorter name must have two labels, so `com` or `app` never match everything.
 */
export function sameSite(a: string, b: string): boolean {
  if (a === b) return true;
  const [short, long] = a.length <= b.length ? [a, b] : [b, a];
  return short.includes(".") && long.endsWith(`.${short}`);
}

/** Whether `url` is on one of the `expected` hosts. Nothing expected, or an unreadable URL: no. */
export function isExpectedSignInPage(url: string, expected: readonly string[]): boolean {
  const host = originOf(url)?.host;
  if (!host) return false;
  return expected.some((e) => {
    const wanted = hostOf(e);
    return wanted !== null && sameSite(host, wanted);
  });
}

/**
 * The hosts the sign-in page of an app is expected on. A client registered for an issuer (Google)
 * is only ever sent there, so the issuer alone counts; an app discovered at sign-in is expected
 * on the site of its own address (the server's or the catalog's). Anything else is confirmed.
 */
export function expectedSignInHosts(
  server: Pick<McpServer, "url" | "oauth"> | undefined,
  entry: McpCatalogEntry | undefined
): string[] {
  const issuer = server?.oauth?.issuer ?? entry?.install.oauth_client?.issuer;
  if (issuer) return [issuer];
  return [server?.url, entry?.install.url].filter((u): u is string => typeof u === "string" && u !== "");
}

/**
 * Whether a pasted address can be this sign-in's redirect at all: the same address as
 * `redirectUri` up to the query. A quick check so a wrong paste fails at once; the daemon still
 * compares the address and the `state` itself.
 */
export function looksLikeRedirect(pasted: string, redirectUri: string): boolean {
  const base = redirectUri.split(/[?#]/)[0];
  return base !== "" && pasted.trim().startsWith(`${base}?`);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error ?? "");
}

/**
 * The reason behind a failure, from the daemon's stable codes first and its fixed sentences next
 * (the daemon never forwards a provider's own words, so these are all it can say).
 */
export function signInReason(error: unknown): SignInReason {
  const code = (error as { code?: unknown } | null)?.code;
  const status = (error as { status?: unknown } | null)?.status;
  if (code === "host_admin_required") return "admin_needed";
  if (code === "owner_key_required" || code === "host_admin_forbidden") return "owner_needed";
  if (code === "network") return "unreachable";
  const text = messageOf(error).toLowerCase();
  if (status === 409 || text.includes("already in progress")) return "already_running";
  if (text.includes("no sign-in is waiting")) return "not_waiting";
  if (text.includes("wrong `state`") || text.includes("not this sign-in's redirect")) {
    return "wrong_address";
  }
  if (text.includes("no authorization code") || text.includes("unexpected request")) {
    return "wrong_address";
  }
  if (text.includes("scope was refused")) return "scope_refused";
  if (text.includes("code was not accepted")) return "code_rejected";
  if (text.includes("issuer does not match")) return "issuer_mismatch";
  if (text.includes("discovery failed") || text.includes("could not be reached")) {
    return "provider_unreachable";
  }
  if (text.includes("refused the request") || text.includes("client registration failed")) {
    return "denied";
  }
  if (text.includes("time limit") || text.includes("did not finish")) return "timeout";
  if (text.includes("cancelled")) return "not_waiting";
  return "unknown";
}

/** Whether trying the same sign-in again can help (it cannot when the person must act elsewhere first). */
export function signInRetryable(reason: SignInReason): boolean {
  return reason !== "admin_needed" && reason !== "owner_needed" && reason !== "issuer_mismatch";
}

/** Run one sign-in to the end. Never throws: every outcome is reported and returned. */
export async function runSignIn(
  deps: SignInDeps,
  name: string,
  report: (state: SignInState) => void,
  control: SignInControl,
  opts: { pollMs?: number; maxMs?: number } = {}
): Promise<SignInState> {
  const pollMs = opts.pollMs ?? SIGN_IN_POLL_MS;
  const finish = (step: SignInStep): SignInState => {
    const state = { ...step, name } as SignInState;
    report(state);
    return state;
  };
  const failed = (error: unknown, reason?: SignInReason): SignInState =>
    finish({
      step: "failed",
      reason: reason ?? signInReason(error),
      // The redacted text the Details disclosure shows (no keys, no profile paths).
      detail: humanError(error).details || undefined
    });

  finish({ step: "starting" });
  let relay: { id: number; port: number } | null = null;
  if (deps.remote && deps.relay) {
    try {
      relay = await deps.relay.listen();
    } catch {
      // Cannot listen here: the paste step below says so plainly instead of failing.
      relay = null;
    }
  }
  const stopRelay = async () => {
    if (relay && deps.relay) {
      await deps.relay.cancel(relay.id).catch(() => undefined);
    }
  };
  let started: McpAuthStart;
  try {
    started = await deps.start(relay?.port);
  } catch (error) {
    await stopRelay();
    return failed(error);
  }
  const mode: SignInMode = !deps.remote ? "local" : relay && started.relayed ? "relay" : "paste";
  if (mode !== "relay") {
    await stopRelay();
  }
  // An authorization server the app was not set up with is the server's say-so (discovery): it is
  // shown and opened only when the person confirms.
  const needsConfirm = !isExpectedSignInPage(started.authorization_url, deps.expectedHosts);
  const opened = needsConfirm ? false : await deps.open(started.authorization_url).catch(() => false);
  const browserStep = {
    step: "browser" as const,
    url: started.authorization_url,
    opened,
    redirectUri: started.redirect_uri,
    origin: originOf(started.authorization_url)?.origin ?? "",
    needsConfirm
  };
  finish({ ...browserStep, mode });

  const deadline = deps.now() + Math.min(started.expires_in * 1000, opts.maxMs ?? SIGN_IN_MAX_MS);
  const stop = async (step: "cancelled" | "expired"): Promise<SignInState> => {
    await stopRelay();
    await deps.cancel().catch(() => undefined);
    return finish({ step });
  };

  if (mode === "local") {
    for (;;) {
      await deps.sleep(pollMs);
      if (control.cancelled) return stop("cancelled");
      if (deps.now() > deadline) return stop("expired");
      try {
        const state = await deps.status();
        if (state !== "signing_in") {
          return state === "signed_in"
            ? finish({ step: "done" })
            : failed(new Error("sign-in did not finish"), "denied");
        }
      } catch (error) {
        return failed(error);
      }
    }
  }

  // relay / paste: a redirect address has to come in, from the listener or from the person.
  let redirect: string | null = null;
  let waitFailed: unknown = null;
  let waiting = mode === "relay";
  let shownMode = mode;
  if (relay && deps.relay) {
    const wanted = stateOf(started.authorization_url);
    if (wanted === null) {
      await stopRelay();
      return failed(new Error("the authorization URL carries no state"), "relay_failed");
    }
    deps.relay.wait(relay.id, wanted).then(
      (url) => {
        redirect = url;
        waiting = false;
      },
      (error: unknown) => {
        waitFailed = error;
        waiting = false;
      }
    );
  }
  for (;;) {
    if (control.cancelled) return stop("cancelled");
    if (redirect === null && control.pasted) {
      const pasted = control.pasted;
      control.pasted = null;
      if (looksLikeRedirect(pasted, started.redirect_uri)) {
        redirect = pasted;
      } else {
        // Not this sign-in's address: say so at once, keep waiting for the right one.
        finish({ ...browserStep, mode: shownMode, wrongPaste: true });
      }
    }
    if (redirect !== null) break;
    if (mode === "relay" && !waiting && waitFailed !== null) {
      const why = messageOf(waitFailed);
      if (why === "cancelled") return stop("cancelled");
      if (why === "timeout") return stop("expired");
      // The listener died (not cancelled, not late; too many stray requests too): the person
      // pastes the address instead, and the panel asks for it.
      waitFailed = null;
      shownMode = "paste";
      finish({ ...browserStep, mode: shownMode });
    }
    if (deps.now() > deadline) return stop("expired");
    await deps.sleep(pollMs);
  }

  finish({ step: "finishing" });
  await stopRelay();
  try {
    const state = await deps.complete(redirect);
    return state === "signed_in"
      ? finish({ step: "done" })
      : failed(new Error("sign-in did not finish"), "denied");
  } catch (error) {
    return failed(error);
  }
}

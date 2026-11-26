/** Launch a toolkit consent page after the separate Composio account setup. */

export type FlowStep =
  /** Asking Composio for the app's consent link. */
  | "app"
  /** The app's consent page is open: waiting for the person to finish there. */
  | "opened"
  | "failed"
  | "cancelled"
  | "timed_out";

export interface FlowState {
  step: FlowStep;
  slug: string;
  name: string;
  /** The daemon created a pending account that can be cancelled. */
  hasLink: boolean;
  /** Stable reason when `step` is `failed`. */
  reason?: FlowReason;
  /**
   * The daemon's own words for a failed app-link request (already translated and with secrets
   * removed by the human-error layer): what happened, what to do, and the raw text for a
   * "Details" disclosure. Never the primary message: that is the reason's text.
   */
  failure?: FailureText;
}

/** The real reason of a failure, as the human-error layer words it. */
export interface FailureText {
  title: string;
  hint: string;
  details: string;
}

export type FlowReason =
  /** Composio refused the saved login even after a fresh sign-in, or the sign-in is needed again. */
  | "login_expired"
  | "rate_limited"
  /** Composio (or the way to it) failed: not the person's doing. */
  | "upstream"
  /** This computer's owner confirmation is missing: only the desktop app on this computer can connect apps. */
  | "owner_key"
  /** Composio does not offer this app (or Praxi does not connect it): a retry cannot help. */
  | "unknown_app"
  /** Praxi refused the request (for example an account that does not match): a retry cannot help. */
  | "app_refused"
  /** The daemon cannot do it right now (Composio off, audit log unavailable). */
  | "unavailable"
  | "app_link"
  | "app_open";

/** What a failed "give me the app's consent link" call means, in the flow's terms. */
export type LinkFailure =
  | "login"
  | "rate_limited"
  | "upstream"
  | "owner"
  | "unknown_app"
  | "refused"
  | "unavailable"
  | "other";

/** Whether pressing "Try again" with the same request can ever give a different answer. */
export function canRetry(reason: FlowReason | undefined): boolean {
  return reason !== "login_expired" && reason !== "owner_key" && reason !== "unknown_app" && reason !== "app_refused";
}

function reasonOf(why: Exclude<LinkFailure, "login">): FlowReason {
  switch (why) {
    case "owner":
      return "owner_key";
    case "unknown_app":
      return "unknown_app";
    case "refused":
      return "app_refused";
    case "other":
      return "app_link";
    default:
      return why;
  }
}

export interface FlowDeps {
  configured(): boolean;
  loginExpired(): boolean;
  describe?(error: unknown): FailureText;
  appConnect(slug: string): Promise<{ redirect_url: string }>;
  open(url: string): Promise<boolean>;
}

/** A flag the caller flips to stop a running flow. */
export interface FlowCancel {
  cancelled: boolean;
}

export async function runConnectFlow(
  deps: FlowDeps,
  slug: string,
  name: string,
  report: (state: FlowState) => void,
  cancel: FlowCancel
): Promise<FlowState> {
  let hasLink = false;
  const done = (step: FlowStep, reason?: FlowReason, failure?: FailureText): FlowState => {
    const state = { slug, name, step, hasLink, reason, failure };
    report(state);
    return state;
  };
  if (cancel.cancelled) return done("cancelled");
  if (!deps.configured() || deps.loginExpired()) return done("failed", "login_expired");
  done("app");
  try {
    const link = await deps.appConnect(slug);
    hasLink = true;
    if (cancel.cancelled) return done("cancelled");
    const opened = await deps.open(link.redirect_url);
    if (cancel.cancelled) return done("cancelled");
    return opened ? done("opened") : done("failed", "app_open");
  } catch (e) {
    if (cancel.cancelled) return done("cancelled");
    const why = classifyLinkFailure(e);
    return done("failed", why === "login" ? "login_expired" : reasonOf(why), deps.describe?.(e));
  }
}

/**
 * Sort the error of the "consent link" request by the daemon's stable code (never by its text).
 * `composio_login_expired` / `composio_not_configured` mean a Composio sign-in fixes it.
 */
export function classifyLinkFailure(error: unknown): LinkFailure {
  const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined;
  switch (code) {
    case "composio_login_expired":
    case "composio_not_configured":
      return "login";
    case "composio_rate_limited":
      return "rate_limited";
    case "composio_error":
    case "network":
    case "timeout":
      return "upstream";
    case "owner_key_required":
      return "owner";
    // The daemon answers 404 `not_found` only for an app Composio does not list.
    case "not_found":
      return "unknown_app";
    case "bad_composio_request":
      return "refused";
    case "unavailable":
      return "unavailable";
    default:
      return "other";
  }
}

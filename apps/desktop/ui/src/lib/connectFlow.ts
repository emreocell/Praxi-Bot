/**
 * One continuous "Connect <app>" flow for a Composio app (card 279).
 *
 * With a Composio credential the app's own consent page opens at once. Without one, or when
 * Composio no longer accepts the saved one, the Composio sign-in opens first and, when it
 * finishes, the SAME flow carries on to the app's consent page without the person pressing
 * anything again. The flow only OPENS pages; an app counts as connected when Composio says so
 * (its state in the app list), never from here.
 */

export type FlowStep =
  /** The Composio sign-in is open in the browser (first time only). */
  | "login"
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
  /** Whether the Composio sign-in is part of this flow (two progress dots instead of one). */
  firstTime: boolean;
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
  | "login_open"
  | "login_failed"
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
  return reason !== "owner_key" && reason !== "unknown_app" && reason !== "app_refused";
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
  /** A Composio credential exists (the browser login of #137, or a pasted project key). */
  configured(): boolean;
  /** Composio refused the saved credential: a fresh sign-in is needed, a retry cannot help. */
  loginExpired(): boolean;
  /** Sort the error of `appConnect` (the daemon codes `composio_login_expired` etc.). */
  classify(error: unknown): LinkFailure;
  /** Word an error for the person (optional: the human-error layer in the app). */
  describe?(error: unknown): FailureText;
  loginStart(): Promise<{ login_url: string }>;
  loginStatus(): Promise<{ state: "idle" | "pending" | "linked" | "failed" }>;
  appConnect(slug: string): Promise<{ redirect_url: string }>;
  /** Open `url` in the system browser; `login` selects the dashboard host rule. */
  open(url: string, login: boolean): Promise<boolean>;
  sleep(ms: number): Promise<void>;
  now(): number;
}

export const LOGIN_POLL_MS = 2000;
/** The login is given up after this long. */
export const LOGIN_GIVE_UP_MS = 10 * 60 * 1000;

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
  let firstTime = !deps.configured() || deps.loginExpired();
  const done = (state: FlowState): FlowState => {
    report(state);
    return state;
  };
  const base = () => ({ slug, name, firstTime });
  /** The Composio sign-in; returns the end state when the flow must stop, else null. */
  const signIn = async (): Promise<FlowState | null> => {
    done({ ...base(), step: "login" });
    try {
      const { login_url } = await deps.loginStart();
      if (!(await deps.open(login_url, true))) {
        return done({ ...base(), step: "failed", reason: "login_open" });
      }
    } catch {
      return done({ ...base(), step: "failed", reason: "login_failed" });
    }
    const started = deps.now();
    for (;;) {
      if (cancel.cancelled) {
        return done({ ...base(), step: "cancelled" });
      }
      if (deps.now() - started > LOGIN_GIVE_UP_MS) {
        return done({ ...base(), step: "timed_out" });
      }
      await deps.sleep(LOGIN_POLL_MS);
      if (cancel.cancelled) {
        return done({ ...base(), step: "cancelled" });
      }
      try {
        const { state } = await deps.loginStatus();
        if (state === "linked") {
          return null;
        }
        if (state === "failed") {
          return done({ ...base(), step: "failed", reason: "login_failed" });
        }
      } catch {
        // A missed poll is not the end of the sign-in.
      }
    }
  };
  if (firstTime) {
    const stopped = await signIn();
    if (stopped) {
      return stopped;
    }
  }
  let signedInHere = firstTime;
  for (;;) {
    if (cancel.cancelled) {
      return done({ ...base(), step: "cancelled" });
    }
    done({ ...base(), step: "app" });
    try {
      const link = await deps.appConnect(slug);
      if (cancel.cancelled) {
        return done({ ...base(), step: "cancelled" });
      }
      if (!(await deps.open(link.redirect_url, false))) {
        return done({ ...base(), step: "failed", reason: "app_open" });
      }
      return done({ ...base(), step: "opened" });
    } catch (e) {
      const why = deps.classify(e);
      if (why === "login" && !signedInHere) {
        // Composio no longer accepts the saved login: renew it in this same flow.
        signedInHere = true;
        firstTime = true;
        const stopped = await signIn();
        if (stopped) {
          return stopped;
        }
        continue;
      }
      return done({
        ...base(),
        step: "failed",
        reason: why === "login" ? "login_expired" : reasonOf(why),
        failure: deps.describe?.(e)
      });
    }
  }
}

/** Which of the two progress dots is current (1 = Composio sign-in, 2 = the app). */
export function progressDot(state: FlowState): 1 | 2 {
  return state.step === "login" ? 1 : 2;
}

/** Whether the sheet is still busy with this flow. */
export function flowActive(state: FlowState | null): boolean {
  return state !== null && (state.step === "login" || state.step === "app");
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

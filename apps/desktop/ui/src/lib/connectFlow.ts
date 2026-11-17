/**
 * One continuous "Connect <app>" flow for a Composio app (card 279).
 *
 * With a Composio credential the app's own consent page opens at once. Without one, the
 * one-time Composio sign-in opens first and, when it finishes, the SAME flow carries on to the
 * app's consent page without the person pressing anything again. The flow only OPENS pages; an
 * app counts as connected when Composio says so (its state in the app list), never from here.
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
  reason?: "login_open" | "login_failed" | "app_link" | "app_open";
}

export interface FlowDeps {
  /** A Composio credential exists (the browser login of #137, or a pasted project key). */
  configured(): boolean;
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
  const firstTime = !deps.configured();
  const done = (state: FlowState): FlowState => {
    report(state);
    return state;
  };
  const base = { slug, name, firstTime };
  if (firstTime) {
    done({ ...base, step: "login" });
    try {
      const { login_url } = await deps.loginStart();
      if (!(await deps.open(login_url, true))) {
        return done({ ...base, step: "failed", reason: "login_open" });
      }
    } catch {
      return done({ ...base, step: "failed", reason: "login_failed" });
    }
    const started = deps.now();
    for (;;) {
      if (cancel.cancelled) {
        return done({ ...base, step: "cancelled" });
      }
      if (deps.now() - started > LOGIN_GIVE_UP_MS) {
        return done({ ...base, step: "timed_out" });
      }
      await deps.sleep(LOGIN_POLL_MS);
      if (cancel.cancelled) {
        return done({ ...base, step: "cancelled" });
      }
      try {
        const { state } = await deps.loginStatus();
        if (state === "linked") {
          break;
        }
        if (state === "failed") {
          return done({ ...base, step: "failed", reason: "login_failed" });
        }
      } catch {
        // A missed poll is not the end of the sign-in.
      }
    }
  }
  if (cancel.cancelled) {
    return done({ ...base, step: "cancelled" });
  }
  done({ ...base, step: "app" });
  let link: { redirect_url: string };
  try {
    link = await deps.appConnect(slug);
  } catch {
    return done({ ...base, step: "failed", reason: "app_link" });
  }
  if (cancel.cancelled) {
    return done({ ...base, step: "cancelled" });
  }
  if (!(await deps.open(link.redirect_url, false))) {
    return done({ ...base, step: "failed", reason: "app_open" });
  }
  return done({ ...base, step: "opened" });
}

/** Which of the two progress dots is current (1 = Composio sign-in, 2 = the app). */
export function progressDot(state: FlowState): 1 | 2 {
  return state.step === "login" ? 1 : 2;
}

/** Whether the sheet is still busy with this flow. */
export function flowActive(state: FlowState | null): boolean {
  return state !== null && (state.step === "login" || state.step === "app");
}

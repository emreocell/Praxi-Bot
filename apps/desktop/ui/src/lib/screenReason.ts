import { noScreenDetailKey } from "./computerLabel.ts";

/** The exact command that adds the desktop session to a Linux server (run on the server). */
export const DESKTOP_SESSION_COMMAND = "sudo ./install-linux.sh --update --with-desktop-session";

/** The command that grants this computer's screen view to every desktop (run on the server). */
export const SCREEN_GRANT_COMMAND = "bot host screen --all-desktops";

/** Target-level reasons that say nothing about the cause: the host's own note says more. */
const GENERIC_REASONS = new Set(["unavailable", "unsupported"]);

/** Codes after which the cure is a desktop session on the server. */
const NEEDS_SESSION = new Set(["no_display", "desktop_off"]);

const FIX_KEY: Record<string, string> = {
  desktop_off: "addSession",
  no_display: "addSession",
  wayland: "wayland",
  screen_unavailable: "locked",
  start_failed: "restart",
  starting: "wait",
  timed_out: "wait",
  unsupported_platform: "platform"
};

export interface ScreenExplanation {
  /** i18n key of the sentence that says what is wrong. */
  detailKey: string;
  /** i18n key of the sentence that says what to do. */
  fixKey: string;
  /** A command for the server, when one cures it. */
  command: string | null;
}

/**
 * The real reason a screen cannot be shown and how to fix it. `reason` is the target's own typed
 * reason, `noScreen` the host's "why there is no screen" code. Returns `null` when the target's
 * reason is specific (the viewer words it itself) and the host has nothing more to say.
 */
export function explainScreen(
  reason: string | null,
  noScreen: string | null
): ScreenExplanation | null {
  if (noScreen) {
    return {
      detailKey: noScreenDetailKey(noScreen),
      fixKey: `liveView.noScreenFix.${FIX_KEY[noScreen] ?? "generic"}`,
      command: NEEDS_SESSION.has(noScreen) ? DESKTOP_SESSION_COMMAND : null
    };
  }
  if (reason === null || GENERIC_REASONS.has(reason)) {
    return {
      detailKey: "liveView.noScreen.unknown",
      fixKey: "liveView.noScreenFix.generic",
      command: DESKTOP_SESSION_COMMAND
    };
  }
  return null;
}

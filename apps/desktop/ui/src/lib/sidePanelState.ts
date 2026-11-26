import type { NeedsYouEntry } from "../state/timeline.ts";
import { PANEL_MIN, panelMax } from "./shellLayout.ts";

export type LiveViewPresence = "idle" | "starting" | "live";
export type SidePanelMode = "hidden" | "docked" | "overlay";
export type SidePanelReason =
  | "no-bot"
  | "closed"
  | "owner-docked"
  | "owner-overlay-screen"
  | "owner-overlay-narrow"
  | "attention-needed";

/**
 * Below this window width the side panel overlays instead of squeezing the main pane.  Down to here the
 * rail is icons only (see `RAIL_NARROW_BELOW`), so a docked panel still leaves the chat `MAIN_MIN`
 * and the composer is never covered.
 */
export const PANEL_DOCK_MIN_WIDTH = 720;

export interface SidePanelInput {
  /** Whether a bot route (`chat` or `bot`) is active. */
  botInView: boolean;
  /** Explicit owner toggle (bot chip, Back button, or mock `?panel=1`). */
  ownerOpen: boolean;
  /** True when a pending approval/question cannot be answered inline in the current view. */
  unanswerableNeed: boolean;
  /** Owner dismissed the current auto-opened attention state. */
  attentionDismissed?: boolean;
  /** Current live-view stream/session presence. */
  liveViewState: LiveViewPresence;
  /** Current viewport width in px. */
  viewportWidth: number;
  /** Stored preferred panel width in px. */
  storedPanelWidth: number;
  /** Fitted left rail width in px. */
  railWidth: number;
  /** Whether floating chrome (`data-float="on"`) is enabled. */
  float: boolean;
}

export interface SidePanelState {
  mode: SidePanelMode;
  visible: boolean;
  /** Panel target width in px, clamped cleanly to the viewport. */
  width: number;
  /** Width taken in document flow (`0` when hidden or overlay, `width` when docked). */
  flowWidth: number;
  reason: SidePanelReason;
}

/**
 * Pure state machine for the adaptive right side panel.
 *
 * Invariants:
 * - Closed by default; opens only on explicit owner action or an unanswerable-inline need.
 * - Arriving inline approvals or starting/stopping live view never resize the chat column or
 *   move the screen frame (`flowWidth` stays `0` in overlay mode).
 * - Below `PANEL_DOCK_MIN_WIDTH`, or whenever the full-screen live view is open, the panel slides
 *   in over the right edge as an overlay instead of reflowing the main pane. A preview stream
 *   started by the computer card does not count: opening the panel never flips its mode afterwards.
 */
export function deriveSidePanelState(input: SidePanelInput): SidePanelState {
  const max = panelMax(input.viewportWidth, input.railWidth, input.float);
  const clamped = Math.min(Math.max(PANEL_MIN, input.storedPanelWidth), max);
  const width = Math.min(clamped, Math.max(PANEL_MIN, input.viewportWidth - 24));

  if (!input.botInView) {
    return { mode: "hidden", visible: false, width, flowWidth: 0, reason: "no-bot" };
  }

  const attentionOpen = input.unanswerableNeed && !input.attentionDismissed;
  if (!input.ownerOpen && !attentionOpen) {
    return { mode: "hidden", visible: false, width, flowWidth: 0, reason: "closed" };
  }

  if (attentionOpen && !input.ownerOpen) {
    return {
      mode: "overlay",
      visible: true,
      width,
      flowWidth: 0,
      reason: "attention-needed"
    };
  }

  if (input.liveViewState !== "idle") {
    return {
      mode: "overlay",
      visible: true,
      width,
      flowWidth: 0,
      reason: "owner-overlay-screen"
    };
  }

  if (input.viewportWidth < PANEL_DOCK_MIN_WIDTH) {
    return {
      mode: "overlay",
      visible: true,
      width,
      flowWidth: 0,
      reason: "owner-overlay-narrow"
    };
  }

  return {
    mode: "docked",
    visible: true,
    width,
    flowWidth: width,
    reason: "owner-docked"
  };
}

/**
 * The full-screen live view ("Büyüt") takes the whole app window: the bot panel steps aside when the
 * view opens and comes back, if it was open, when the view closes. While the view is open the owner
 * can still bring the panel in from the view's own menu; closing the view then restores the state it
 * had before the view opened, not the one it had a moment ago.
 */
export function screenPanelStep(input: {
  wasScreenOpen: boolean;
  screenOpen: boolean;
  panelOpen: boolean;
  /** The panel's state before the view opened (`null` while the view is closed). */
  stash: boolean | null;
}): { panelOpen: boolean; stash: boolean | null } {
  if (input.screenOpen && !input.wasScreenOpen) {
    return { panelOpen: false, stash: input.panelOpen };
  }
  if (!input.screenOpen && input.wasScreenOpen) {
    return { panelOpen: input.stash ?? input.panelOpen, stash: null };
  }
  return { panelOpen: input.panelOpen, stash: input.stash };
}

/** What can ask the side panel to go away. */
export type PanelDismissTrigger =
  | "close-button"
  | "toggle"
  | "escape"
  | "outside-pointer"
  | "focus-moved"
  | "chat-activity";

/**
 * The one rule for dismissing the right-hand panel: only the owner's explicit acts close it, that
 * is its own X, the control that opened it (bot chip, or the info button of the live view) and
 * Escape. Clicking or focusing anything else (the chat, the composer, the screen share) never does:
 * the owner reads the bot's activity while typing.
 */
export function panelDismissedBy(trigger: PanelDismissTrigger): boolean {
  return trigger === "close-button" || trigger === "toggle" || trigger === "escape";
}

/**
 * Whether any pending approval/question for the active bot cannot be answered inline on the
 * current route (for example on `route.kind === "bot"` before opening a chat, or when a
 * waiting run has no inline/pinned card in `ChatView`).
 */
export function hasUnanswerableNeed(opts: {
  routeKind: "home" | "myBots" | "onboarding" | "bot" | "chat" | "needs" | "notifications" | "schedules" | "workflows";
  selectedBotId?: string;
  activeChatId?: string;
  needs: readonly NeedsYouEntry[];
}): boolean {
  if (!opts.selectedBotId) return false;
  const botNeeds = opts.needs.filter((n) => n.botId === opts.selectedBotId);
  if (botNeeds.length === 0) return false;
  if (opts.routeKind === "bot") {
    return true;
  }
  if (opts.routeKind === "chat") {
    // In `ChatView`, `pendingForChat` renders inline cards for `activeChatId` and pins cards for
    // the bot's other chats right above the composer — all answerable inline without opening the panel.
    return false;
  }
  return false;
}

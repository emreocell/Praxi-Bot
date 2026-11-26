import assert from "node:assert/strict";
import { test } from "node:test";
import {
  screenPanelStep,
  deriveSidePanelState,
  hasUnanswerableNeed,
  panelDismissedBy,
  PANEL_DOCK_MIN_WIDTH,
  type SidePanelInput
} from "./sidePanelState.ts";

const BASE: SidePanelInput = {
  botInView: true,
  ownerOpen: false,
  unanswerableNeed: false,
  attentionDismissed: false,
  liveViewState: "idle",
  viewportWidth: 1280,
  storedPanelWidth: 320,
  railWidth: 264,
  float: true
};

test("default state is hidden with zero flow width", () => {
  const s = deriveSidePanelState(BASE);
  assert.equal(s.mode, "hidden");
  assert.equal(s.visible, false);
  assert.equal(s.flowWidth, 0);
  assert.equal(s.reason, "closed");
});

test("arriving inline-answerable approval keeps panel hidden and flowWidth 0", () => {
  const inline = hasUnanswerableNeed({
    routeKind: "chat",
    selectedBotId: "bot-1",
    activeChatId: "chat-1",
    needs: [
      {
        kind: "approval",
        callId: "c1",
        runId: "r1",
        chatId: "chat-1",
        botId: "bot-1",
        title: "Run shell",
        sinceMs: 0
      }
    ]
  });
  assert.equal(inline, false);
  const s = deriveSidePanelState({ ...BASE, unanswerableNeed: inline });
  assert.equal(s.mode, "hidden");
  assert.equal(s.flowWidth, 0);
});

test("unanswerable need opens panel as overlay with flowWidth 0 (no chat or screen reflow)", () => {
  const cantAnswer = hasUnanswerableNeed({
    routeKind: "bot",
    selectedBotId: "bot-1",
    needs: [
      {
        kind: "question",
        callId: "q1",
        runId: "r1",
        chatId: "chat-9",
        botId: "bot-1",
        title: "Choose folder",
        sinceMs: 0
      }
    ]
  });
  assert.equal(cantAnswer, true);
  const s = deriveSidePanelState({ ...BASE, unanswerableNeed: cantAnswer });
  assert.equal(s.mode, "overlay");
  assert.equal(s.visible, true);
  assert.equal(s.flowWidth, 0);
  assert.equal(s.reason, "attention-needed");

  // Dismissing returns panel to hidden while keeping flowWidth 0.
  const dismissed = deriveSidePanelState({
    ...BASE,
    unanswerableNeed: cantAnswer,
    attentionDismissed: true
  });
  assert.equal(dismissed.mode, "hidden");
  assert.equal(dismissed.flowWidth, 0);
});

test("while the full-screen live view is open the panel overlays and never takes flow width", () => {
  for (const ownerOpen of [false, true]) {
    const starting = deriveSidePanelState({ ...BASE, ownerOpen, liveViewState: "starting" });
    const live = deriveSidePanelState({ ...BASE, ownerOpen, liveViewState: "live" });
    assert.equal(starting.flowWidth, 0);
    assert.equal(live.flowWidth, 0);
    if (ownerOpen) {
      assert.equal(starting.mode, "overlay");
      assert.equal(live.mode, "overlay");
    }
  }
});

test("below PANEL_DOCK_MIN_WIDTH the panel stays overlay even without big screen", () => {
  const narrow = deriveSidePanelState({
    ...BASE,
    ownerOpen: true,
    liveViewState: "idle",
    viewportWidth: PANEL_DOCK_MIN_WIDTH - 1
  });
  assert.equal(narrow.mode, "overlay");
  assert.equal(narrow.flowWidth, 0);
  assert.equal(narrow.reason, "owner-overlay-narrow");

  const wide = deriveSidePanelState({
    ...BASE,
    ownerOpen: true,
    liveViewState: "idle",
    viewportWidth: PANEL_DOCK_MIN_WIDTH
  });
  assert.equal(wide.mode, "docked");
  assert.equal(wide.flowWidth, wide.width);
  assert.equal(wide.reason, "owner-docked");
});

test("non-bot routes always keep the side panel hidden", () => {
  const s = deriveSidePanelState({
    ...BASE,
    botInView: false,
    ownerOpen: true,
    unanswerableNeed: true
  });
  assert.equal(s.mode, "hidden");
  assert.equal(s.visible, false);
  assert.equal(s.flowWidth, 0);
  assert.equal(s.reason, "no-bot");
});

test("only the panel's own X, its toggle and Escape dismiss it; a click on the chat never does", () => {
  assert.equal(panelDismissedBy("close-button"), true);
  assert.equal(panelDismissedBy("toggle"), true);
  assert.equal(panelDismissedBy("escape"), true);
  assert.equal(panelDismissedBy("outside-pointer"), false);
  assert.equal(panelDismissedBy("focus-moved"), false);
  assert.equal(panelDismissedBy("chat-activity"), false);
});

test("the open panel does not change shape when a computer preview starts streaming", () => {
  // The computer card starts a preview stream by itself; App feeds `liveViewState` only from the
  // full-screen viewer, so the mode a wide window opened with is the mode it keeps.
  const opened = deriveSidePanelState({ ...BASE, ownerOpen: true, liveViewState: "idle" });
  assert.equal(opened.mode, "docked");
  const viewer = deriveSidePanelState({ ...BASE, ownerOpen: true, liveViewState: "live" });
  assert.equal(viewer.mode, "overlay");
  assert.equal(viewer.flowWidth, 0);
});

test("a 760 px window docks the panel: it pushes the chat instead of covering the composer", () => {
  const s = deriveSidePanelState({ ...BASE, ownerOpen: true, viewportWidth: 760, railWidth: 68 });
  assert.equal(s.mode, "docked");
  assert.equal(s.flowWidth, s.width);
});

test("opening the full-screen view hides the open panel and closing it brings the panel back", () => {
  const opened = screenPanelStep({ wasScreenOpen: false, screenOpen: true, panelOpen: true, stash: null });
  assert.deepEqual(opened, { panelOpen: false, stash: true });
  const steady = screenPanelStep({ wasScreenOpen: true, screenOpen: true, panelOpen: false, stash: true });
  assert.deepEqual(steady, { panelOpen: false, stash: true });
  const closed = screenPanelStep({ wasScreenOpen: true, screenOpen: false, panelOpen: false, stash: true });
  assert.deepEqual(closed, { panelOpen: true, stash: null });
});

test("a panel that was closed stays closed after the full-screen view", () => {
  const opened = screenPanelStep({ wasScreenOpen: false, screenOpen: true, panelOpen: false, stash: null });
  assert.deepEqual(opened, { panelOpen: false, stash: false });
  const closed = screenPanelStep({ wasScreenOpen: true, screenOpen: false, panelOpen: false, stash: false });
  assert.deepEqual(closed, { panelOpen: false, stash: null });
});

test("a panel the owner opened inside the view is put back to its earlier state on exit", () => {
  const inView = screenPanelStep({ wasScreenOpen: true, screenOpen: true, panelOpen: true, stash: false });
  assert.equal(inView.panelOpen, true);
  const closed = screenPanelStep({ wasScreenOpen: true, screenOpen: false, panelOpen: true, stash: false });
  assert.equal(closed.panelOpen, false);
});

test("without a view change nothing moves", () => {
  const same = screenPanelStep({ wasScreenOpen: false, screenOpen: false, panelOpen: true, stash: null });
  assert.deepEqual(same, { panelOpen: true, stash: null });
});

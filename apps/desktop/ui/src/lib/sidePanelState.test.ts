import assert from "node:assert/strict";
import { test } from "node:test";
import {
  deriveSidePanelState,
  hasUnanswerableNeed,
  PANEL_DOCK_MIN_WIDTH,
  type SidePanelInput
} from "./sidePanelState.ts";

const BASE: SidePanelInput = {
  botInView: true,
  ownerOpen: false,
  unanswerableNeed: false,
  attentionDismissed: false,
  liveViewState: "idle",
  bigScreenDocked: true,
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

test("starting or stopping live view never changes chat column width or moves screen frame", () => {
  for (const ownerOpen of [false, true]) {
    const idle = deriveSidePanelState({ ...BASE, ownerOpen, liveViewState: "idle" });
    const starting = deriveSidePanelState({ ...BASE, ownerOpen, liveViewState: "starting" });
    const live = deriveSidePanelState({ ...BASE, ownerOpen, liveViewState: "live" });
    assert.equal(idle.flowWidth, 0);
    assert.equal(starting.flowWidth, 0);
    assert.equal(live.flowWidth, 0);
    if (ownerOpen) {
      assert.equal(idle.mode, "overlay");
      assert.equal(starting.mode, "overlay");
      assert.equal(live.mode, "overlay");
    }
  }
});

test("below PANEL_DOCK_MIN_WIDTH the panel stays overlay even without big screen", () => {
  const narrow = deriveSidePanelState({
    ...BASE,
    ownerOpen: true,
    bigScreenDocked: false,
    liveViewState: "idle",
    viewportWidth: PANEL_DOCK_MIN_WIDTH - 1
  });
  assert.equal(narrow.mode, "overlay");
  assert.equal(narrow.flowWidth, 0);
  assert.equal(narrow.reason, "owner-overlay-narrow");

  const wide = deriveSidePanelState({
    ...BASE,
    ownerOpen: true,
    bigScreenDocked: false,
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

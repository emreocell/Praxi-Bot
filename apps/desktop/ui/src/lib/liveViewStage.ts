import type { InputEvent } from "../api/types.ts";
import {
  clampButtonsMask,
  domButtonToInputButton,
  extractModifiers,
  KeyForwarder,
  normalizeWheelDelta
} from "./liveViewInput.ts";

/**
 * Event handlers of the live-view stage, kept framework-free so the exact
 * handler objects the component spreads onto its elements can be exercised
 * with a bubbling simulation (no DOM in the test runner).
 *
 * Wiring rules (regression-tested):
 * - key events are forwarded from ONE element, the IME textarea; the stage
 *   `div` carries no key handlers, and forwarded keys stop propagating;
 * - local controls inside the stage (Capture Tab / Exit keyboard) get the
 *   `toolbar` handlers, so a click on them never reaches the pointer
 *   forwarding on the stage and never becomes remote input.
 */

export interface StageEventBase {
  preventDefault(): void;
  stopPropagation(): void;
}

export interface StageModifiers {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}

export interface StagePointerEvent extends StageEventBase, StageModifiers {
  button: number;
  buttons: number;
  clientX: number;
  clientY: number;
  pointerId: number;
  currentTarget: {
    setPointerCapture(id: number): void;
    hasPointerCapture(id: number): boolean;
    releasePointerCapture(id: number): void;
  };
}

export interface StageWheelEvent extends StageEventBase, StageModifiers {
  clientX: number;
  clientY: number;
  deltaX: number;
  deltaY: number;
  deltaMode: number;
}

export interface StageKeyEvent extends StageEventBase, StageModifiers {
  code: string;
  key: string;
  location: number;
  repeat: boolean;
  nativeEvent: { isComposing: boolean };
  getModifierState?: (key: never) => boolean;
}

export interface StageDeps {
  isInteractive(): boolean;
  captureTab(): boolean;
  mapPointer(
    clientX: number,
    clientY: number,
    clamp: boolean
  ): { x: number; y: number } | null;
  stageSize(): { w: number; h: number };
  enqueue(event: InputEvent): void;
  /** Drop held keys/buttons (`reset` is sent only when something was held). */
  resetInput(): void;
  focusIme(): void;
  markFocused(focused: boolean): void;
  exitKeyboardFocus(): void;
  setComposing(composing: boolean): void;
  clearImeBuffer(): void;
}

type Handler<E> = (e: E) => void;

export interface StageHandlers {
  stage: {
    onPointerDown: Handler<StagePointerEvent>;
    onPointerMove: Handler<StagePointerEvent>;
    onPointerUp: Handler<StagePointerEvent>;
    onPointerCancel: Handler<StagePointerEvent>;
    onWheel: Handler<StageWheelEvent>;
    onMouseDown: Handler<StageEventBase>;
  };
  ime: {
    onKeyDown: Handler<StageKeyEvent>;
    onKeyUp: Handler<StageKeyEvent>;
    onCompositionStart: () => void;
    onCompositionEnd: Handler<{ data: string }>;
  };
  /** For the local toolbar rendered inside the stage. */
  toolbar: {
    onPointerDown: Handler<StageEventBase>;
    onPointerMove: Handler<StageEventBase>;
    onPointerUp: Handler<StageEventBase>;
    onPointerCancel: Handler<StageEventBase>;
    onMouseDown: Handler<StageEventBase>;
    onWheel: Handler<StageEventBase>;
  };
  /** Forget captured pointer and held-key bookkeeping (gate closed / focus left). */
  reset(): void;
}

/** True when focus moved to something outside `container` (or to nothing). */
export function focusLeftScope(
  related: unknown,
  container: { contains(node: never): boolean } | null
): boolean {
  if (!related || !container) {
    return true;
  }
  return !container.contains(related as never);
}

export function createStageController(getDeps: () => StageDeps): StageHandlers {
  const keys = new KeyForwarder();
  let activePointerId: number | null = null;

  const releaseCapture = (e: StagePointerEvent) => {
    try {
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
    } catch {
      // synthetic pointer events may not carry native capture
    }
  };

  const onPointerDown: Handler<StagePointerEvent> = (e) => {
    const d = getDeps();
    if (!d.isInteractive()) {
      return;
    }
    const btn = domButtonToInputButton(e.button);
    if (!btn) {
      return;
    }
    const pt = d.mapPointer(e.clientX, e.clientY, false);
    if (!pt) {
      return;
    }
    e.preventDefault();
    d.focusIme();
    d.markFocused(true);
    activePointerId = e.pointerId;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // synthetic pointer events may not carry native capture
    }
    d.enqueue({
      type: "pointer",
      phase: "down",
      x: pt.x,
      y: pt.y,
      button: btn,
      buttons: clampButtonsMask(e.buttons),
      modifiers: extractModifiers(e)
    });
  };

  const onPointerMove: Handler<StagePointerEvent> = (e) => {
    const d = getDeps();
    if (!d.isInteractive()) {
      return;
    }
    const dragging = activePointerId === e.pointerId;
    const pt = d.mapPointer(e.clientX, e.clientY, dragging);
    if (!pt) {
      return;
    }
    d.enqueue({
      type: "pointer",
      phase: "move",
      x: pt.x,
      y: pt.y,
      button: null,
      buttons: clampButtonsMask(e.buttons),
      modifiers: extractModifiers(e)
    });
  };

  const onPointerUp: Handler<StagePointerEvent> = (e) => {
    const d = getDeps();
    if (!d.isInteractive()) {
      return;
    }
    const btn = domButtonToInputButton(e.button);
    if (!btn) {
      return;
    }
    const dragging = activePointerId === e.pointerId;
    const pt = d.mapPointer(e.clientX, e.clientY, dragging);
    if (dragging) {
      activePointerId = null;
      releaseCapture(e);
    }
    if (!pt) {
      return;
    }
    d.enqueue({
      type: "pointer",
      phase: "up",
      x: pt.x,
      y: pt.y,
      button: btn,
      buttons: clampButtonsMask(e.buttons),
      modifiers: extractModifiers(e)
    });
  };

  // A cancelled gesture never produced a button release: reset instead of
  // inventing a left `up`.
  const onPointerCancel: Handler<StagePointerEvent> = (e) => {
    const d = getDeps();
    if (activePointerId === e.pointerId) {
      activePointerId = null;
      releaseCapture(e);
    }
    if (d.isInteractive()) {
      d.resetInput();
    }
  };

  const onWheel: Handler<StageWheelEvent> = (e) => {
    const d = getDeps();
    if (!d.isInteractive()) {
      return;
    }
    const pt = d.mapPointer(e.clientX, e.clientY, false);
    if (!pt) {
      return;
    }
    const size = d.stageSize();
    d.enqueue({
      type: "wheel",
      x: pt.x,
      y: pt.y,
      delta_x: normalizeWheelDelta(e.deltaX, e.deltaMode, size.w || 800),
      delta_y: normalizeWheelDelta(e.deltaY, e.deltaMode, size.h || 600),
      modifiers: extractModifiers(e)
    });
  };

  const toKeyLike = (e: StageKeyEvent, repeat: boolean) => ({
    code: e.code,
    key: e.key,
    location: e.location,
    repeat,
    altKey: e.altKey,
    ctrlKey: e.ctrlKey,
    metaKey: e.metaKey,
    shiftKey: e.shiftKey,
    isComposing: e.nativeEvent.isComposing,
    altGraph: e.getModifierState?.("AltGraph" as never) ?? false
  });

  const onKeyDown: Handler<StageKeyEvent> = (e) => {
    const d = getDeps();
    if (!d.isInteractive()) {
      return;
    }
    // Never trap keyboard-only users: Shift+Escape always exits to Bitirdim.
    if (e.shiftKey && e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      keys.reset();
      d.exitKeyboardFocus();
      return;
    }
    if (e.key === "Tab" && !d.captureTab()) {
      return;
    }
    const events = keys.down(toKeyLike(e, e.repeat), e.nativeEvent.isComposing);
    if (events.length > 0) {
      e.preventDefault();
      e.stopPropagation();
      for (const ev of events) {
        d.enqueue(ev);
      }
    }
  };

  const onKeyUp: Handler<StageKeyEvent> = (e) => {
    const d = getDeps();
    if (!d.isInteractive()) {
      return;
    }
    if (e.key === "Tab" && !d.captureTab()) {
      return;
    }
    const events = keys.up(toKeyLike(e, false), e.nativeEvent.isComposing);
    if (events.length > 0) {
      e.preventDefault();
      e.stopPropagation();
      for (const ev of events) {
        d.enqueue(ev);
      }
    }
  };

  const stop: Handler<StageEventBase> = (e) => {
    e.stopPropagation();
  };

  return {
    stage: {
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel,
      onWheel,
      onMouseDown: (e) => {
        const d = getDeps();
        if (d.isInteractive()) {
          e.preventDefault();
          d.focusIme();
        }
      }
    },
    ime: {
      onKeyDown,
      onKeyUp,
      onCompositionStart: () => getDeps().setComposing(true),
      onCompositionEnd: (e) => {
        const d = getDeps();
        d.setComposing(false);
        d.clearImeBuffer();
        if (d.isInteractive() && e.data) {
          d.enqueue({ type: "text", text: e.data });
        }
      }
    },
    toolbar: {
      onPointerDown: stop,
      onPointerMove: stop,
      onPointerUp: stop,
      onPointerCancel: stop,
      onMouseDown: stop,
      onWheel: stop
    },
    reset: () => {
      activePointerId = null;
      keys.reset();
    }
  };
}

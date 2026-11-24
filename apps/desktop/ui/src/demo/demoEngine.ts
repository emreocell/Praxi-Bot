import type en from "./demo.en.json";
import type { ProvidersDto } from "../api/types.ts";
import { modelGroups } from "../lib/modelPicker.ts";

/**
 * The offline demo bot. Everything here is pure and deterministic: a scripted timeline, a reducer
 * and two gates. It never imports the daemon client, never calls a provider and never touches
 * files, the browser or the computer: a demo run lives only in component state and is gone
 * when the window closes (nothing is stored as a chat or as memory).
 */
export type DemoTaskId = "summarise" | "plan" | "explain";
export const DEMO_TASK_IDS: readonly DemoTaskId[] = ["summarise", "plan", "explain"];

export interface DemoScript {
  title: string;
  hint: string;
  userText: string;
  steps: string[];
  approval: { title: string; detail: string; note: string };
  answer: string;
  denied: string;
}
export type DemoUi = typeof en.ui;
export type DemoData = Record<DemoTaskId, DemoScript> & { ui: DemoUi };

/** Characters of the answer revealed per tick: a calm stream, deterministic. */
export const DEMO_CHUNK = 6;
export const DEMO_TICK_MS = 60;
/** Pause between the demo bot's steps. */
export const DEMO_STEP_MS = 700;

export type DemoStatus = "running" | "awaitingApproval" | "done" | "stopped";
export type DemoDecision = "none" | "allowed" | "denied";

export interface DemoState {
  task: DemoTaskId;
  status: DemoStatus;
  steps: string[];
  decision: DemoDecision;
  /** Answer text revealed so far. */
  answer: string;
  /** The full answer being streamed (set when the approval is decided). */
  target: string;
}

export type DemoAction =
  | { type: "tick" }
  | { type: "allow" }
  | { type: "deny" }
  | { type: "stop" };

export function startDemo(task: DemoTaskId): DemoState {
  return { task, status: "running", steps: [], decision: "none", answer: "", target: "" };
}

/** Advances the demo by one tick. Pure: the same actions always give the same screen. */
export function demoReducer(state: DemoState, action: DemoAction, script: DemoScript): DemoState {
  if (state.status === "stopped" || state.status === "done") {
    return state;
  }
  switch (action.type) {
    case "stop":
      return { ...state, status: "stopped" };
    case "allow":
    case "deny": {
      if (state.status !== "awaitingApproval") {
        return state;
      }
      const allowed = action.type === "allow";
      return {
        ...state,
        status: "running",
        decision: allowed ? "allowed" : "denied",
        target: allowed ? script.answer : script.denied
      };
    }
    case "tick": {
      if (state.status === "awaitingApproval") {
        return state;
      }
      if (state.steps.length < script.steps.length) {
        return { ...state, steps: [...state.steps, script.steps[state.steps.length]] };
      }
      if (state.decision === "none") {
        return { ...state, status: "awaitingApproval" };
      }
      const next = state.target.slice(0, state.answer.length + DEMO_CHUNK);
      return next.length >= state.target.length
        ? { ...state, answer: state.target, status: "done" }
        : { ...state, answer: next };
    }
  }
}

/**
 * True when a real model is available (an enabled provider, including a signed-in coding CLI).
 * `null` (not loaded yet) counts as connected, so the demo never flashes at people who have a model.
 */
export function hasRealModel(dto: ProvidersDto | null): boolean {
  return dto === null || modelGroups(dto, null).some((g) => g.models.length > 0);
}

/** The demo is offered on its own only while no real model is connected; Help can always open it. */
export function offerDemo(dto: ProvidersDto | null): boolean {
  return !hasRealModel(dto);
}

/** After the demo ends (or is stopped), a gentle card points at the real connect flow. */
export function showConnectCard(state: DemoState): boolean {
  return state.status === "done" || state.status === "stopped";
}

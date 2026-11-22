import type { CliEngineDto, ProvidersDto } from "../api/types.ts";
import { type AutonomyLevel } from "./botProfile.ts";
import { cliStatus } from "./cliCards.ts";
import { type ProviderTestProblem, providerTestProblem } from "./humanError.ts";
import { providerBrand } from "./modelBrand.ts";
import { modelDisplayName } from "./modelName.ts";
import { isCliDefault, modelGroups, sourceRecommendations } from "./modelPicker.ts";

/**
 * First-run flow: three screens, one decision each.
 *   brain   - which model runs the bot (a signed-in coding CLI, or an API key)
 *   freedom - how much the bot may do without asking
 *   bot     - name and look, then "Start"
 * Everything here is pure so the flow can be tested without a daemon.
 */
export type FirstRunStep = "brain" | "freedom" | "bot";
/** The first bot of a fresh home is Praxi, the white ghost: it becomes the main bot (first in the list). */
export const FIRST_RUN_BOT_SHAPE = "ghost" as const;
export const FIRST_RUN_BOT_COLOR = "#f4f4f5";

export const FIRST_RUN_STEPS: readonly FirstRunStep[] = ["brain", "freedom", "bot"];

/** `choose`: nothing picked yet; `key`: an API provider waits for its key; `connecting`: a check is running; `connected`: a model is chosen. */
export type BrainPhase = "choose" | "key" | "connecting" | "connected";

/** API providers the first screen offers. `openai` stands for any OpenAI-compatible service. */
export type ApiGroup = "gemini" | "anthropic" | "openai" | "ollama";
export const API_GROUPS: readonly ApiGroup[] = ["gemini", "anthropic", "openai", "ollama"];

export type BrainChoice =
  | { kind: "cli"; id: string }
  | { kind: "api"; group: ApiGroup };

/** Human reasons a connection failed; each maps to one sentence in the user's language. */
export type ConnectError = "badKey" | "offline" | "ollamaDown" | "ollamaNoModels" | "noModels" | "unavailable" | "unknown" | ProviderTestProblem;

export interface FirstRunState {
  step: FirstRunStep;
  phase: BrainPhase;
  choice: BrainChoice | null;
  /** `provider/model` chosen automatically (or via "Change"); empty until connected. */
  model: string;
  error: ConnectError | null;
  level: AutonomyLevel;
  /** Set when the flow ends; `skipped` leaves the app usable with a quiet hint. */
  outcome: "started" | "skipped" | null;
}

export type FirstRunAction =
  | { type: "pickCli"; id: string; model: string }
  | { type: "pickApi"; group: ApiGroup }
  | { type: "connecting" }
  | { type: "connected"; model: string }
  | { type: "failed"; error: ConnectError }
  | { type: "changeBrain" }
  | { type: "setModel"; model: string }
  | { type: "setLevel"; level: AutonomyLevel }
  | { type: "next" }
  | { type: "back" }
  | { type: "skip" }
  | { type: "started" };

export const INITIAL_FIRST_RUN: FirstRunState = {
  step: "brain",
  phase: "choose",
  choice: null,
  model: "",
  error: null,
  level: "balanced",
  outcome: null
};

/** A detected CLI is already signed in, so it never needs a key; Ollama needs none either. */
export function needsKey(choice: BrainChoice | null): boolean {
  return choice?.kind === "api" && choice.group !== "ollama";
}

/** "Continue" on the brain screen is only possible once a model is chosen. */
export function canContinue(state: FirstRunState): boolean {
  return state.step !== "brain" || (state.phase === "connected" && state.model !== "");
}

export function firstRunReducer(state: FirstRunState, action: FirstRunAction): FirstRunState {
  switch (action.type) {
    case "pickCli":
      return {
        ...state,
        phase: "connected",
        choice: { kind: "cli", id: action.id },
        model: action.model,
        error: null
      };
    case "pickApi": {
      const choice: BrainChoice = { kind: "api", group: action.group };
      // Ollama has no key: go straight to the check.
      return {
        ...state,
        phase: needsKey(choice) ? "key" : "connecting",
        choice,
        model: "",
        error: null
      };
    }
    case "connecting":
      return { ...state, phase: "connecting", error: null };
    case "connected":
      return { ...state, phase: "connected", model: action.model, error: null };
    case "failed":
      // Stay where the user can fix it: the key field, or the provider list for Ollama.
      return {
        ...state,
        phase: needsKey(state.choice) ? "key" : "choose",
        model: "",
        error: action.error
      };
    case "changeBrain":
      return { ...state, phase: "choose", choice: null, model: "", error: null };
    case "setModel":
      return state.phase === "connected" ? { ...state, model: action.model } : state;
    case "setLevel":
      return { ...state, level: action.level };
    case "next": {
      if (!canContinue(state)) {
        return state;
      }
      const at = FIRST_RUN_STEPS.indexOf(state.step);
      return { ...state, step: FIRST_RUN_STEPS[Math.min(at + 1, FIRST_RUN_STEPS.length - 1)] };
    }
    case "back": {
      const at = FIRST_RUN_STEPS.indexOf(state.step);
      return { ...state, step: FIRST_RUN_STEPS[Math.max(at - 1, 0)] };
    }
    case "skip":
      return { ...state, outcome: "skipped" };
    case "started":
      return { ...state, outcome: "started" };
  }
}

/**
 * One human sentence for a failed connection. Raw daemon / CLI text is only used to tell the
 * causes apart; it is never shown.
 */
export function classifyConnectError(raw: string, group: ApiGroup | null): ConnectError {
  const text = raw.toLowerCase();
  if (/\b(?:401|403)\b|authentication failed|unauthori[sz]ed|forbidden|invalid[^a-z]*(api)?[^a-z]*key|api key not valid|permission denied|incorrect api key/.test(text)) {
    return "badKey";
  }
  const problem = group ? providerTestProblem(raw) : undefined;
  if (problem) {
    return problem;
  }
  if (group === "ollama" && /refused|econnrefused|unreachable|not running|connect|timed out|timeout|error sending request|tcp/.test(text)) {
    return "ollamaDown";
  }
  if (/network|dns|resolve|timed out|timeout|offline|unreachable|connection|econn|enotfound|failed to fetch/.test(text)) {
    return "offline";
  }
  if (/providers_require_app_daemon|outside_tauri|local daemon|not.*local/.test(text)) {
    return "unavailable";
  }
  return "unknown";
}

/** Catalog provider id behind an API card; `openai` is refined by the key's prefix. */
export function providerIdFor(group: ApiGroup, detected: string | null): string {
  if (group === "openai") {
    return detected === "openrouter" || detected === "openai" || detected === "deepseek" ? detected : "openai";
  }
  return group;
}

/** Engines the first screen can offer with one click: installed, runnable, already signed in. */
export function readyEngines(engines: readonly CliEngineDto[]): CliEngineDto[] {
  return engines.filter((e) => cliStatus(e) === "ready");
}

export interface AutoModel {
  /** `provider/model`. */
  ref: string;
  /** True when the picker's recommendation rules chose it (shown as "recommended"). */
  recommended: boolean;
}

/**
 * The model a freshly connected provider starts with: the first recommendation of the model
 * picker's own rules (`sourceRecommendations`), else the CLI's own default, else any listed model.
 */
export function autoModelFor(dto: ProvidersDto | null, providerId: string): AutoModel | null {
  const group = modelGroups(dto).find((g) => g.provider === providerId);
  if (!group) {
    return null;
  }
  const rec = sourceRecommendations(group)[0];
  if (rec) {
    return { ref: rec.option.ref, recommended: true };
  }
  const first = group.models.find((m) => !isCliDefault(m)) ?? group.models[0];
  return first ? { ref: first.ref, recommended: false } : null;
}

/** Number of models a provider lists (CLI "default" rows do not count). */
export function modelCount(dto: ProvidersDto | null, providerId: string): number {
  const group = modelGroups(dto).find((g) => g.provider === providerId);
  return group ? group.models.filter((m) => !isCliDefault(m)).length : 0;
}

/** Readable name of a `provider/model` ref: a CLI's own default reads as the CLI's name. */
export function refLabel(ref: string): string {
  const at = ref.indexOf("/");
  const provider = at < 0 ? "" : ref.slice(0, at);
  const model = ref.slice(at + 1);
  return model === "default" && provider ? providerBrand(provider).label : modelDisplayName(model);
}

/** Prompts that exercise real abilities; the chat offers them as one-click starters. */
export const FIRST_PROMPT_KEYS = ["draft", "screen", "folder", "news"] as const;

/** What is visibly wrong with a key while it is being typed, so the person hears it before pressing Connect. */
export type KeyShapeProblem = "address" | "short" | "prefix";

/**
 * Cheap, offline checks in plain words: a web address pasted instead of the key, a key cut off,
 * or a key of another service. They only advise; the provider has the final say.
 */
export function keyShapeProblem(group: ApiGroup | null, key: string): KeyShapeProblem | null {
  const k = key.trim();
  if (k === "" || group === "ollama") {
    return null;
  }
  if (/^https?:\/\//i.test(k) || /\s/.test(k)) {
    return "address";
  }
  if (k.length < 20) {
    return "short";
  }
  if (group === "gemini" && !k.startsWith("AIza")) {
    return "prefix";
  }
  if (group === "anthropic" && !k.startsWith("sk-ant-")) {
    return "prefix";
  }
  return null;
}

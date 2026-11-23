/**
 * Help > "Copy diagnostic info": the redacted support report is fetched once,
 * shown in a preview, and exactly that text is what the Copy button puts on the
 * clipboard (it is never fetched a second time, so what the user read is what
 * they paste).
 */
export type DiagnosticInfoState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "ready"; text: string }
  | { phase: "error"; error: unknown };

export type DiagnosticInfoAction =
  | { type: "load" }
  | { type: "loaded"; text: string }
  | { type: "failed"; error: unknown }
  | { type: "reset" };

export const INITIAL_DIAGNOSTIC_INFO: DiagnosticInfoState = { phase: "idle" };

export function diagnosticInfoReducer(
  state: DiagnosticInfoState,
  action: DiagnosticInfoAction
): DiagnosticInfoState {
  switch (action.type) {
    case "load":
      return state.phase === "loading" ? state : { phase: "loading" };
    case "loaded":
      return state.phase === "loading" ? { phase: "ready", text: action.text } : state;
    case "failed":
      return state.phase === "loading" ? { phase: "error", error: action.error } : state;
    case "reset":
      return INITIAL_DIAGNOSTIC_INFO;
  }
}

/** The text a Copy press writes: only the previewed text, nothing else. */
export function textToCopy(state: DiagnosticInfoState): string | null {
  return state.phase === "ready" && state.text.trim() !== "" ? state.text : null;
}

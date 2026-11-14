import { useCallback, useState } from "react";

/**
 * Thinking level chosen in the model picker.  The daemon has no field for it yet, so for now
 * it is only remembered on this device (localStorage) and not sent with a run.
 */
export const THINKING_LEVELS = ["low", "medium", "high", "max"] as const;
export type ThinkingLevel = (typeof THINKING_LEVELS)[number];
export const DEFAULT_THINKING_LEVEL: ThinkingLevel = "medium";

const KEY = "praxi.thinking.v1";

export function parseThinkingLevel(raw: unknown): ThinkingLevel {
  return THINKING_LEVELS.find((l) => l === raw) ?? DEFAULT_THINKING_LEVEL;
}

export function loadThinkingLevel(): ThinkingLevel {
  try {
    return parseThinkingLevel(window.localStorage?.getItem(KEY));
  } catch {
    return DEFAULT_THINKING_LEVEL;
  }
}

export function useThinkingLevel(): [ThinkingLevel, (level: ThinkingLevel) => void] {
  const [level, setLevel] = useState<ThinkingLevel>(loadThinkingLevel);
  const set = useCallback((next: ThinkingLevel) => {
    setLevel(next);
    try {
      window.localStorage?.setItem(KEY, next);
    } catch {
      // Not remembered when storage is unavailable.
    }
  }, []);
  return [level, set];
}

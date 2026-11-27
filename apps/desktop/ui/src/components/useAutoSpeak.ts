import { useEffect, useRef } from "react";
import { repliesToSpeak } from "../lib/autoSpeak";
import { DEFAULT_SETTINGS, loadSettingsSync } from "../store/settingsStore";
import type { RunState } from "../state/timeline";
import { replySpeechSupported, useReplySpeaker } from "./SpeakButton";

/** Reads a finished reply aloud when the owner turned "Speak replies" on (settings mirror, read live). */
export function useAutoSpeak(chatId: string, runs: readonly RunState[], external = false): void {
  const speakReply = useReplySpeaker();
  const seen = useRef(new Set<string>());
  const primedFor = useRef<string | null>(null);
  useEffect(() => {
    if (external) {
      primedFor.current = null;
      return;
    }
    const prime = primedFor.current !== chatId;
    primedFor.current = chatId;
    if (prime) {
      seen.current = new Set();
    }
    const due = repliesToSpeak(runs, seen.current, prime);
    const prefs = loadSettingsSync() ?? DEFAULT_SETTINGS;
    const last = due[due.length - 1];
    if (!last || !prefs.speakReplies || !replySpeechSupported()) {
      return;
    }
    speakReply(`auto:${last.id}`, last.text, undefined, true);
  }, [chatId, runs, speakReply, external]);
}

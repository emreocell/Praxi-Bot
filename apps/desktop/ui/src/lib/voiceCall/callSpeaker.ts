import { speechSupported } from "../speaker";
import { pickVoice } from "../voicePrefs";

/**
 * Queued read-aloud for the call: each sentence is one utterance and the engine plays them in
 * order, so speech starts at the first sentence. Same OS speech synthesis and voice choice as the
 * read-aloud button; nothing is sent anywhere.
 */

let pending = 0;
/** Bumped by every cancel: late end/error events of cancelled utterances are ignored. */
let generation = 0;
let onDrained: (() => void) | null = null;

export function queueSpeech(text: string, opts: { voiceName: string; rate: number; lang: string }, onStart?: () => void): void {
  if (!speechSupported() || !text.trim()) {
    return;
  }
  const u = new SpeechSynthesisUtterance(text);
  const voice = pickVoice(window.speechSynthesis.getVoices(), opts.voiceName, opts.lang);
  if (voice) {
    u.voice = voice;
    u.lang = voice.lang;
  } else {
    u.lang = opts.lang;
  }
  u.rate = opts.rate;
  const gen = generation;
  const finish = () => {
    if (gen !== generation) {
      return; // cancelled: the queue was dropped on purpose
    }
    pending -= 1;
    if (pending === 0) {
      onDrained?.();
    }
  };
  u.onstart = () => onStart?.();
  u.onend = finish;
  u.onerror = finish;
  pending += 1;
  window.speechSynthesis.speak(u);
}

/** Called when everything queued has been spoken (or failed). */
export function setDrainedHandler(fn: (() => void) | null): void {
  onDrained = fn;
}

export function hasQueuedSpeech(): boolean {
  return pending > 0;
}

/** Barge-in / end: drop everything queued and stop talking. The drained handler is not called. */
export function cancelQueuedSpeech(): void {
  pending = 0;
  generation += 1;
  if (speechSupported()) {
    window.speechSynthesis.cancel();
  }
}

import { pickVoice } from "./voicePrefs";

/**
 * Reads text aloud with the operating system's speech synthesis (`window.speechSynthesis`):
 * nothing leaves the machine unless the user picks an online voice, no key or account is needed.
 * One utterance at a time; starting another replaces the current one.
 */

type Listener = () => void;

let speakingId: string | null = null;
let generation = 0;
let offline: AbortController | null = null;
const listeners = new Set<Listener>();

function emit(): void {
  listeners.forEach((l) => l());
}

export function speechSupported(): boolean {
  return typeof window !== "undefined" && "speechSynthesis" in window && typeof SpeechSynthesisUtterance !== "undefined";
}

export function subscribeSpeaking(l: Listener): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function currentSpeakingId(): string | null {
  return speakingId;
}

export function stopSpeaking(): void {
  cancelPlayback();
  speakingId = null;
  emit();
}

function cancelPlayback(): void {
  generation += 1;
  offline?.abort();
  offline = null;
  if (speechSupported()) {
    window.speechSynthesis.cancel();
  }
}

/** Voices the OS offers (may be empty until the engine has loaded them). */
export function listVoices(): SpeechSynthesisVoice[] {
  return speechSupported() ? window.speechSynthesis.getVoices() : [];
}

export function speak(id: string, text: string, opts: { voiceName: string; rate: number; lang: string }): void {
  if (!speechSupported() || !text.trim()) {
    return;
  }
  cancelPlayback();
  const active = generation;
  const u = new SpeechSynthesisUtterance(text);
  const voice = pickVoice(listVoices(), opts.voiceName, opts.lang);
  if (voice) {
    u.voice = voice;
    u.lang = voice.lang;
  } else {
    u.lang = opts.lang;
  }
  u.rate = opts.rate;
  const done = () => {
    if (generation === active) {
      speakingId = null;
      emit();
    }
  };
  u.onend = done;
  u.onerror = done;
  speakingId = id;
  emit();
  window.speechSynthesis.speak(u);
}

type Synthesize = (text: string, lang: string, signal: AbortSignal) => Promise<Blob>;
type PlayWav = (wav: Blob, rate: number, signal: AbortSignal) => Promise<void>;

/** Abort the pending request or audio on Stop, barge-in or the next read-aloud. */
export async function speakOffline(
  id: string,
  text: string,
  opts: { lang: string; rate: number; synthesize: Synthesize },
  play: PlayWav = playWav
): Promise<void> {
  if (!text.trim()) return;
  cancelPlayback();
  const controller = new AbortController();
  const { signal } = controller;
  offline = controller;
  speakingId = id;
  emit();
  try {
    for (const piece of speechPieces(text, opts.lang)) {
      signal.throwIfAborted();
      const audio = await opts.synthesize(piece, opts.lang, signal);
      signal.throwIfAborted();
      await play(audio, opts.rate, signal);
    }
  } catch (e) {
    if (!signal.aborted) throw e;
  } finally {
    if (offline === controller) {
      offline = null;
      speakingId = null;
      emit();
    }
  }
}

/** Keep requests bounded while preserving sentence boundaries and full Unicode characters. */
function* speechPieces(text: string, lang: string): Generator<string> {
  const sentences = new Intl.Segmenter(lang, { granularity: "sentence" }).segment(text.replaceAll("\0", ""));
  const limit = 400;
  let pending = "";
  for (const { segment } of sentences) {
    const sentence = segment.trim();
    if (!sentence) continue;
    const combined = pending ? `${pending} ${sentence}` : sentence;
    if (combined.length <= limit) {
      pending = combined;
      continue;
    }
    if (pending) yield pending;
    const chars = Array.from(sentence);
    let start = 0;
    while (chars.length - start > limit) {
      const space = chars.lastIndexOf(" ", start + limit);
      const end = space > start ? space : start + limit;
      yield chars.slice(start, end).join("").trim();
      start = end;
    }
    pending = chars.slice(start).join("").trim();
  }
  if (pending) yield pending;
}

function playWav(blob: Blob, rate: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const audio = new Audio();
    const url = URL.createObjectURL(blob);
    let finished = false;
    const finish = (error?: unknown) => {
      if (finished) return;
      finished = true;
      audio.onended = null;
      audio.onerror = null;
      signal.removeEventListener("abort", cancel);
      audio.pause();
      audio.removeAttribute("src");
      URL.revokeObjectURL(url);
      if (error) reject(error);
      else resolve();
    };
    const cancel = () => finish(signal.reason);
    signal.addEventListener("abort", cancel, { once: true });
    audio.onended = () => finish();
    audio.onerror = () => finish(new Error("invalid WAV"));
    audio.src = url;
    audio.playbackRate = rate;
    void audio.play().catch(finish);
  });
}

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

/** A reply read aloud on its own is cut to this many characters (the daemon refuses more per request). */
export const MAX_SPEECH_CHARS = 1000;

/** The one read-aloud that waits for the current one to end instead of cutting it off. */
let waiting: { start: () => void; drop: () => void } | null = null;

function emit(): void {
  listeners.forEach((l) => l());
}

function startWaiting(): void {
  const next = waiting;
  waiting = null;
  next?.start();
}

/** At most `max` characters, ending at a sentence boundary when one fits (whole Unicode characters). */
export function truncateForSpeech(text: string, lang: string, max = MAX_SPEECH_CHARS): string {
  const clean = text.replaceAll("\0", "").trim();
  if (Array.from(clean).length <= max) return clean;
  let out = "";
  for (const { segment } of new Intl.Segmenter(lang, { granularity: "sentence" }).segment(clean)) {
    if (Array.from(out + segment).length > max) break;
    out += segment;
  }
  return out.trim() || Array.from(clean).slice(0, max).join("").trim();
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
  waiting?.drop();
  waiting = null;
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
      startWaiting();
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

/**
 * Abort the pending request or audio on Stop, barge-in or the next read-aloud. One utterance plays
 * at a time. With `whenIdle` (a reply read aloud on its own) the text waits for the current one to
 * end instead of cutting it off, a newer waiting one replaces an older one, and Stop drops it.
 * `maxChars` cuts a long text at a sentence boundary.
 */
export async function speakOffline(
  id: string,
  text: string,
  opts: { lang: string; rate: number; synthesize: Synthesize; whenIdle?: boolean; maxChars?: number },
  play: PlayWav = playWav
): Promise<void> {
  const spoken = opts.maxChars === undefined ? text : truncateForSpeech(text, opts.lang, opts.maxChars);
  if (!spoken.trim()) return;
  if (opts.whenIdle && speakingId !== null) {
    return new Promise<void>((resolve, reject) => {
      waiting?.drop();
      waiting = {
        start: () => void speakOffline(id, spoken, { ...opts, whenIdle: false }, play).then(resolve, reject),
        drop: resolve
      };
    });
  }
  cancelPlayback();
  const controller = new AbortController();
  const { signal } = controller;
  offline = controller;
  speakingId = id;
  emit();
  try {
    for (const piece of speechPieces(spoken, opts.lang)) {
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
      startWaiting();
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

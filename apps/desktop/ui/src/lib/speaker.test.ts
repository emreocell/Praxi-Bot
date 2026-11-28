import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, afterEach, before, beforeEach, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";

type SpeakOpts = { voiceName: string; rate: number; lang: string };

let speechSupported: () => boolean;
let speak: (id: string, text: string, opts: SpeakOpts) => void;
let speakOffline: typeof import("./speaker.ts").speakOffline;
let stopSpeaking: () => void;
let truncateForSpeech: typeof import("./speaker.ts").truncateForSpeech;
let currentSpeakingId: () => string | null;
let server: ViteDevServer;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui"] }
  });
  ({ speechSupported, speak, speakOffline, stopSpeaking, currentSpeakingId, truncateForSpeech } = await server.ssrLoadModule("/src/lib/speaker.ts"));
});

after(async () => {
  await server?.close();
});

const OPTS: SpeakOpts = { voiceName: "", rate: 1, lang: "en" };

class FakeUtterance {
  text: string;
  lang = "";
  rate = 0;
  voice: unknown = null;
  onend: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(text: string) {
    this.text = text;
  }
}

let calls: string[];
let spoken: FakeUtterance[];
const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const previousUtterance = Object.getOwnPropertyDescriptor(globalThis, "SpeechSynthesisUtterance");

function restore(name: string, descriptor: PropertyDescriptor | undefined) {
  if (descriptor) Object.defineProperty(globalThis, name, descriptor);
  else Reflect.deleteProperty(globalThis, name);
}

beforeEach(() => {
  calls = [];
  spoken = [];
});

afterEach(() => {
  restore("window", previousWindow);
  restore("SpeechSynthesisUtterance", previousUtterance);
  // Without a speech engine this only resets the module's speaking id for the next test.
  stopSpeaking();
});

function installSpeech() {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      speechSynthesis: {
        cancel: () => void calls.push("cancel"),
        speak: (u: FakeUtterance) => {
          calls.push("speak");
          spoken.push(u);
        },
        getVoices: () => []
      }
    }
  });
  Object.defineProperty(globalThis, "SpeechSynthesisUtterance", { configurable: true, value: FakeUtterance });
}

test("without a window there is no speech, and speak and stopSpeaking do not throw", () => {
  restore("window", undefined);
  restore("SpeechSynthesisUtterance", undefined);
  assert.equal(speechSupported(), false);
  assert.doesNotThrow(() => speak("id", "hello", OPTS));
  assert.equal(currentSpeakingId(), null);
  assert.doesNotThrow(() => stopSpeaking());
  assert.equal(currentSpeakingId(), null);
});

test("speak cancels the previous utterance, then speaks and records the id", () => {
  installSpeech();
  assert.equal(speechSupported(), true);
  speak("id", "hello", OPTS);
  assert.deepEqual(calls, ["cancel", "speak"]);
  assert.equal(currentSpeakingId(), "id");
  const [u] = spoken;
  assert.equal(u?.text, "hello");
  assert.equal(u?.lang, "en");
  assert.equal(u?.rate, 1);
  assert.equal(typeof u?.onend, "function");
  assert.equal(typeof u?.onerror, "function");
});

test("onend clears the speaking id", () => {
  installSpeech();
  speak("id", "hello", OPTS);
  spoken[0]?.onend?.();
  assert.equal(currentSpeakingId(), null);
});

test("onerror clears the speaking id", () => {
  installSpeech();
  speak("id", "hello", OPTS);
  spoken[0]?.onerror?.();
  assert.equal(currentSpeakingId(), null);
});

test("a second speak replaces the first, and the first one's onend leaves it alone", () => {
  installSpeech();
  speak("first", "hello", OPTS);
  speak("second", "again", OPTS);
  assert.deepEqual(calls, ["cancel", "speak", "cancel", "speak"]);
  assert.equal(currentSpeakingId(), "second");
  spoken[0]?.onend?.();
  assert.equal(currentSpeakingId(), "second");
});

test("speak with blank text does not reach the speech engine", () => {
  installSpeech();
  speak("id", "", OPTS);
  speak("id", "   \n\t", OPTS);
  assert.deepEqual(calls, []);
  assert.equal(currentSpeakingId(), null);
});

test("stopSpeaking cancels and clears the speaking id", () => {
  installSpeech();
  speak("id", "hello", OPTS);
  calls = [];
  stopSpeaking();
  assert.deepEqual(calls, ["cancel"]);
  assert.equal(currentSpeakingId(), null);
});

test("offline read-aloud passes the app language and plays bounded chunks in order", async () => {
  installSpeech();
  const text = `${"Günaydın. ".repeat(45)}Son cümle.`;
  const chunks: string[] = [];
  const play = async (wav: Blob, rate: number) => {
    assert.equal(currentSpeakingId(), "offline");
    assert.equal(rate, 1.25);
    chunks.push(await wav.text());
  };
  await speakOffline("offline", text, {
    lang: "tr-TR", rate: 1.25,
    synthesize: async (piece, lang) => {
      assert.equal(lang, "tr-TR");
      assert.ok(piece.length <= 400);
      return new Blob([piece], { type: "audio/wav" });
    }
  }, play);
  assert.ok(chunks.length > 1);
  assert.equal(chunks.join(" "), text);
  assert.equal(spoken.length, 0);
  assert.equal(currentSpeakingId(), null);
});

test("stopping pending synthesis prevents late playback", async () => {
  let finish!: (blob: Blob) => void;
  let requestSignal: AbortSignal | undefined;
  const audio = new Promise<Blob>((resolve) => { finish = resolve; });
  const pending = speakOffline("offline", "Hello.", {
    lang: "en", rate: 1,
    synthesize: (_piece, _lang, signal) => {
      requestSignal = signal;
      return audio;
    }
  }, () => assert.fail("cancelled audio played"));
  stopSpeaking();
  assert.equal(requestSignal?.aborted, true);
  finish(new Blob([]));
  await pending;
  assert.equal(currentSpeakingId(), null);
});

test("truncateForSpeech keeps whole sentences within the limit and whole characters when it must cut", () => {
  const sentences = "Birinci cümle. İkinci cümle. Üçüncü cümle.";
  assert.equal(truncateForSpeech(sentences, "tr", 100), sentences);
  assert.equal(truncateForSpeech(sentences, "tr", 30), "Birinci cümle. İkinci cümle.");
  assert.equal(truncateForSpeech("a\0b", "en", 10), "ab");
  const long = "😀".repeat(50);
  assert.equal(Array.from(truncateForSpeech(long, "en", 10)).length, 10);
  assert.equal(truncateForSpeech(`${"x".repeat(5000)}`, "en").length, 1000);
});

test("a long reply is cut to the limit before anything is synthesized", async () => {
  const pieces: string[] = [];
  await speakOffline("auto:1", `${"Bu bir cümle. ".repeat(400)}`, {
    lang: "tr", rate: 1, maxChars: 1000, whenIdle: true,
    synthesize: async (piece) => { pieces.push(piece); return new Blob([piece]); }
  }, async () => {});
  assert.ok(pieces.length > 0);
  assert.ok(pieces.join(" ").length <= 1000, String(pieces.join(" ").length));
});

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

test("a reply read aloud on its own waits for the current utterance, the newest waiter wins, Stop drops it", async () => {
  const log: string[] = [];
  let endFirst!: () => void;
  const synth = (label: string) => async (piece: string) => { log.push(`${label}:${piece}`); return new Blob([piece]); };
  const first = speakOffline("a", "First.", { lang: "en", rate: 1, synthesize: synth("a") },
    () => new Promise<void>((resolve) => { endFirst = resolve; }));
  await tick();
  assert.equal(currentSpeakingId(), "a");
  const base = { lang: "en", rate: 1 };
  const second = speakOffline("auto:b", "Second.", { ...base, whenIdle: true, synthesize: synth("b") }, async () => {});
  const third = speakOffline("auto:c", "Third.", { ...base, whenIdle: true, synthesize: synth("c") }, async () => {});
  await tick();
  assert.equal(currentSpeakingId(), "a", "the current utterance is not cut off");
  assert.deepEqual(log, ["a:First."]);
  endFirst();
  await Promise.all([first, second, third]);
  assert.deepEqual(log, ["a:First.", "c:Third."], "one at a time, only the newest waiter");
  assert.equal(currentSpeakingId(), null);

  // Stop clears what is waiting.
  let endCurrent!: () => void;
  const current = speakOffline("d", "Now.", { ...base, synthesize: synth("d") },
    () => new Promise<void>((resolve) => { endCurrent = resolve; }));
  await tick();
  const waiter = speakOffline("auto:e", "Later.", { ...base, whenIdle: true, synthesize: synth("e") }, async () => {});
  stopSpeaking();
  endCurrent();
  await Promise.all([current, waiter]);
  assert.ok(!log.some((l) => l.startsWith("e:")), "a stopped queue never plays");

  // A click (not whenIdle) still interrupts straight away.
  let endHeld!: () => void;
  const held = speakOffline("f", "Held.", { ...base, synthesize: synth("f") },
    () => new Promise<void>((resolve) => { endHeld = resolve; }));
  await tick();
  await speakOffline("g", "Click.", { ...base, synthesize: synth("g") }, async () => {});
  assert.ok(log.includes("g:Click."));
  endHeld();
  await held;
});

import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, afterEach, before, beforeEach, test } from "node:test";
import { createServer, type ViteDevServer } from "vite";

type SpeakOpts = { voiceName: string; rate: number; lang: string };

let speechSupported: () => boolean;
let speak: (id: string, text: string, opts: SpeakOpts) => void;
let speakOffline: typeof import("./speaker.ts").speakOffline;
let stopSpeaking: () => void;
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
  ({ speechSupported, speak, speakOffline, stopSpeaking, currentSpeakingId } = await server.ssrLoadModule("/src/lib/speaker.ts"));
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

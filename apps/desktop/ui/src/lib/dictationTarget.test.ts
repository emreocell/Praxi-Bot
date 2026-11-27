import assert from "node:assert/strict";
import { test } from "node:test";
import { dictationEndpoints, isNotConfigured, transcribeRecording, type SttEndpoint, type SttProvider } from "./dictationTarget.ts";

const encode = async (_b: Blob, provider: SttProvider) => ({ audio_base64: "QQ==", mime: provider === "whisper" ? "audio/webm" : "audio/pcm" });
const blob = new Blob(["x"]);

function endpoint(baseUrl: string, provider: SttProvider, result: string | Error): SttEndpoint & { calls: string[] } {
  const calls: string[] = [];
  return {
    baseUrl,
    calls,
    getVoice: async () => ({ stt_provider: provider }),
    transcribe: async (input) => {
      calls.push(`${input.mime}|${input.language}|${input.local_only}`);
      if (result instanceof Error) {
        throw result;
      }
      return { text: result };
    }
  };
}
const apiError = (status: number, code: string) => Object.assign(new Error(code), { status, code });

test("this computer's daemon transcribes first, whichever computer the bot is on", async () => {
  const local = endpoint("http://127.0.0.1:1", "gemini_live", "from this computer");
  const server = endpoint("http://127.0.0.1:2", "whisper", "from the server");
  const text = await transcribeRecording(blob, { local, active: server, language: "tr", encode });
  assert.equal(text, "from this computer");
  assert.deepEqual(local.calls, ["audio/pcm|tr|false"]);
  assert.deepEqual(server.calls, []);
});

test("the server is used only when this computer has no transcription service", async () => {
  const local = endpoint("http://127.0.0.1:1", "whisper", apiError(409, "voice_not_configured"));
  const server = endpoint("http://127.0.0.1:2", "whisper", "from the server");
  assert.equal(await transcribeRecording(blob, { local, active: server, language: "en", encode }), "from the server");
});

test("a failed transcription service is reported, not retried on another machine", async () => {
  const local = endpoint("http://127.0.0.1:1", "whisper", apiError(502, "voice_upstream_failed"));
  const server = endpoint("http://127.0.0.1:2", "whisper", "from the server");
  await assert.rejects(transcribeRecording(blob, { local, active: server, language: "en", encode }), { code: "voice_upstream_failed" });
  assert.deepEqual(server.calls, []);
});

test("nothing configured anywhere ends with the not-configured error", async () => {
  const local = endpoint("http://127.0.0.1:1", "whisper", apiError(409, "voice_not_configured"));
  const server = endpoint("http://127.0.0.1:2", "whisper", apiError(409, "voice_not_configured"));
  await assert.rejects(transcribeRecording(blob, { local, active: server, language: "en", encode }), (e) => isNotConfigured(e));
});

test("on-device transcription never leaves the machine", async () => {
  const local = endpoint("http://127.0.0.1:1", "local_voice", "offline text");
  await transcribeRecording(blob, { local, active: null, language: "en", encode });
  assert.deepEqual(local.calls, ["audio/pcm|en|true"]);
});

test("the same daemon is listed once, and a missing local daemon falls back to the active one", () => {
  const a = { baseUrl: "u1" };
  assert.deepEqual(dictationEndpoints(a, { baseUrl: "u1" }), [a]);
  assert.deepEqual(dictationEndpoints(null, a), [a]);
  assert.deepEqual(dictationEndpoints(null, null), []);
});

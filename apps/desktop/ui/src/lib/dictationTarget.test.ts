import assert from "node:assert/strict";
import { test } from "node:test";
import { DictationNotConfigured, isNotConfigured, transcribeRecording, type SttEndpoint, type SttProvider } from "./dictationTarget.ts";

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

test("no daemon on this computer: not set up, and nothing is sent to the active computer", async () => {
  const server = endpoint("http://127.0.0.1:2", "whisper", "from the server");
  await assert.rejects(transcribeRecording(blob, { local: null, active: server, language: "en", encode }), (e) => {
    assert.ok(e instanceof DictationNotConfigured);
    return isNotConfigured(e);
  });
  assert.deepEqual(server.calls, []);
});

test("this computer's daemon not answering is reported, and nothing goes to the active computer", async () => {
  const down = Object.assign(new Error("network"), { status: 0, code: "network" });
  const local = endpoint("http://127.0.0.1:1", "whisper", down);
  const server = endpoint("http://127.0.0.1:2", "whisper", "from the server");
  await assert.rejects(transcribeRecording(blob, { local, active: server, language: "en", encode }), { code: "network" });
  assert.deepEqual(server.calls, []);
  // Not even when it is the lookup of the provider that fails.
  const noVoice: SttEndpoint & { calls: string[] } = { ...server, getVoice: async () => Promise.reject(down) };
  await assert.rejects(transcribeRecording(blob, { local: noVoice, active: server, language: "en", encode }), { code: "network" });
  assert.deepEqual(server.calls, []);
});

test("only the exact voice_not_configured answer moves on to the active computer", async () => {
  for (const [status, code] of [
    [409, "conflict"],
    [403, "voice_not_configured"],
    [500, "voice_not_configured"]
  ] as const) {
    const local = endpoint("http://127.0.0.1:1", "whisper", apiError(status, code));
    const server = endpoint("http://127.0.0.1:2", "whisper", "from the server");
    await assert.rejects(transcribeRecording(blob, { local, active: server, language: "en", encode }), { code });
    assert.deepEqual(server.calls, [], `${status} ${code}`);
  }
});

test("when this computer is the active one and not set up, it is not asked twice", async () => {
  const local = endpoint("http://127.0.0.1:1", "whisper", apiError(409, "voice_not_configured"));
  const same = { ...local, baseUrl: "http://127.0.0.1:1" };
  await assert.rejects(transcribeRecording(blob, { local, active: same, language: "en", encode }), (e) => isNotConfigured(e));
  assert.equal(local.calls.length, 1);
});

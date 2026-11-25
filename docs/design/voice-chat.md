# Voice chat mode and the Gemini Live provider (design note)

Status: chain mode is implemented as the voice call mode (`voice-call-mode.md`, which supersedes the first
proposal here). This note records the realtime provider decision, the Live API facts verified against the
official documentation, and the design of the optional "natural voice" follow-up.

## Goal
A hands-free toggle in the chat: listen -> transcribe -> normal bot turn -> speak the reply -> listen again.
Speech-to-text is pluggable (Settings > General > Voice dictation):

| Provider | Where it runs | Audio format from the UI |
|---|---|---|
| `whisper` (default) | OpenAI-compatible `/audio/transcriptions`, cloud or loopback server | the `MediaRecorder` blob (webm/opus ...) |
| `gemini_live` | Gemini Live API over a WebSocket held by the daemon | raw PCM, 16-bit LE, 16 kHz, mono (`audio/pcm`) |

## Loop (chain mode, unchanged by the provider)
1. **Listen**: the UI opens the microphone (one WebView permission prompt, closed when the mode ends) and detects
   the end of speech with an energy gate (see `voice-call-mode.md`).
2. **Transcribe**: `POST /v1/voice/transcribe`. The UI asks `GET /v1/voice` for the provider and, for
   `gemini_live`, decodes its recording with Web Audio and resamples to 16 kHz mono PCM before sending.
3. **Answer**: the text is an ordinary user message (same permission engine, same run). No voice code path in the
   daemon for actions.
4. **Speak**: OS speech synthesis; barge-in cancels it.
5. Approvals are never answered by voice; the loop pauses on the approval card (ADR 0021).

## Gemini Live provider (implemented)
- The daemon holds the WebSocket and the key (secret store entry `voice/gemini`, write-only through
  `PUT /v1/voice`; `GET /v1/voice` only reports `gemini_key_set`). The key is not in the WebView and never logged:
  the connection URL carries it (as the docs show), so connect errors are not formatted and server close reasons
  have the key scrubbed.
- One short session per utterance (utterances are capped at 120 s). This sidesteps the ~10 minute connection limit,
  so session resumption and context-window compression are not needed. The cost is one handshake per utterance.
- Flow: connect -> `setup` -> wait `setupComplete` -> `realtimeInput.audio` chunks of 100 ms (3200 bytes,
  `audio/pcm;rate=16000`) -> `realtimeInput.audioStreamEnd` -> collect `serverContent.inputTranscription.text` ->
  close. The collection ends at `turnComplete`, or 2.5 s after the last transcript piece (the transcribe model
  does not send `turnComplete`), or 15 s after the stream end.
- Models: default `gemini-3.5-transcribe-live` (`responseModalities: ["TEXT"]`). For a conversational Live model
  (e.g. `gemini-3.8-live`, native audio) the setup uses `responseModalities: ["AUDIO"]` plus the system instruction
  "You are a speech transcriber. Transcribe only, do not answer."; the model turn is ignored. Both were seen to
  return the correct transcript in a manual smoke test (below).
- Only `wss://` endpoints; `ws://` only for loopback (tests use a fake WebSocket server, no network in CI).
- Errors reuse `bad_voice_config` (422), `voice_not_configured` (409) and `voice_upstream_failed` (502).

## Verified Live API facts
Source: official docs under `ai.google.dev/gemini-api/docs/live*` (live, live-guide, live-session, live-transcribe,
live-tools, ephemeral tokens), read 2026-11-22.
- Protocol: stateful WebSocket (`BidiGenerateContent`),
  `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=<API_KEY>`.
  Ephemeral tokens use `BidiGenerateContentConstrained?access_token=<token>` and are meant for client-to-server
  connections; we are server-to-server, so a standard key stays in the daemon.
- Input: raw 16-bit PCM, 16 kHz, little-endian, mime `audio/pcm;rate=16000` (the API resamples other rates).
  Output audio is always 24 kHz 16-bit PCM. Docs recommend ~100 ms chunks. Send `audioStreamEnd` when the stream
  pauses (> 1 s) or the client VAD detects the end of speech ("hybrid VAD", fastest finalization).
- Transcription: `inputAudioTranscription` (user speech) and `outputAudioTranscription` (model speech) in `setup`;
  results arrive in `serverContent.inputTranscription.text` / `outputTranscription.text`. The dedicated
  transcription model also emits `interimInputTranscription` (partials). `VERBATIM` is the default; `SMART`
  returns the cumulative cleaned transcript and is not used here.
- Models seen in the docs: `gemini-3.8-live`, `gemini-3.8-live-extended-thinking`,
  `gemini-3.1-flash-live-preview`, `gemini-2.5-flash-native-audio-preview-12-2025` (conversational, audio-out) and
  `gemini-3.5-transcribe-live` (Live Transcription, text out). Model ids change quickly: the model name is a
  setting, not a constant.
- Response modalities: one of `TEXT` or `AUDIO` per session; the transcription model uses `TEXT`.
- Session limits: audio-only session 15 min without compression; connection life ~10 min; a `GoAway` message
  precedes the close. `contextWindowCompression` (sliding window) extends the session; `sessionResumption`
  yields handles (valid 2 h) to resume across connections. Live transcription sessions: up to 10 min.
- Tool calling exists in the Live API (function calling; `gemini-3.8-live` supports async `NON_BLOCKING` calls).
  We do not enable it (see "natural voice").
- Barge-in: server VAD cancels the generation and reports `serverContent.interrupted`.
- Rate limits for the owner's free tier (Live models unlimited RPM/RPD; TTS ~10/day; Flash ~20/day) come from the
  owner's AI Studio page and are not stated in the public docs: **not verified by us**.

## Manual smoke test (2026-11-22, one run)
A 3 s synthetic English sentence (Windows speech synthesis, 16 kHz PCM) was sent through the real
`transcribe_pcm` code to the production endpoint with the owner's key (read per command, not printed or stored):
`gemini-3.5-transcribe-live` and `gemini-3.8-live` both returned "Hello, this is a short test of voice
transcription." in about 4 s. Turkish/German speech, noisy rooms and the desktop UI path were not exercised.

## Rules (unchanged)
- Approvals are click-only. Echo: headphones recommended; barge-in uses a stricter detector.
- Privacy: audio leaves the machine only while dictating or in a call, only to the configured endpoint
  (`https`/`wss`; plain `http`/`ws` only for loopback); a visible recording indicator and an always-available stop.
- Failure (no service configured, microphone denied) ends the mode with the existing messages.

## Slice 3, design only: "natural voice" (native Live audio output)
Not implemented. An optional toggle next to the provider choice that lets a conversational Live model both hear and
speak, for lower latency and prosody:
- The UI streams 16 kHz PCM chunks to the daemon over the existing authenticated local channel (a new owner-only
  WebSocket route, classified in `owner.rs`); the daemon keeps one Live session per call with
  `responseModalities: ["AUDIO"]`, `outputAudioTranscription` and `inputAudioTranscription`, session resumption and
  context-window compression for calls beyond 10 minutes, and relays 24 kHz PCM back for playback with an
  `AudioContext` queue; `interrupted` flushes the queue (barge-in).
- Text of both directions (the two transcriptions) is appended to the chat as ordinary messages, so history, search
  and export keep working.
- Tools: the Live model is given no tools of its own. If a user turn needs the bot (files, web, apps), the daemon
  submits the transcribed text as a normal chat message to the bot's run and feeds only the final assistant text back
  to the Live session to be spoken; every tool call therefore still goes through the permission engine. Approval
  requests stay on-screen cards; the Live session is told "an action is waiting for confirmation on screen" and never
  receives or answers an approval.
- Untrusted content (web pages, mail, tool output) is data: it is spoken, never forwarded as instructions to the Live
  session's system prompt.
- Cost/availability: depends on the owner's Live quota; the chain mode stays the default and the fallback.
- Open: per-device echo cancellation quality, language-specific voice choice, an indicator that the voice is the
  provider's synthetic voice.

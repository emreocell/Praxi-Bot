# Voice call mode (hands-free chat)

Status: first slice implemented in the desktop UI. Supersedes the proposal in `voice-chat.md`.
Builds on dictation (`POST /v1/voice/transcribe`), read-aloud (OS speech synthesis) and the per-bot voice.
No daemon change: a call is a UI loop around the existing APIs.

## Goal
Tap the phone button in the chat composer, talk, hear the answer, talk again. No keyboard needed.

## The loop
```
idle -> starting -> listening -> thinking -> speaking -> listening ...
                       |            |           |
                       |            +-----------+--> approval (paused, click-only)
                       +-- no speech: stays listening with a hint
```
1. **Listening**: the microphone stream is open and one `MediaRecorder` records the current turn.
   An energy-based voice activity detector (below) decides when the owner started and finished speaking.
2. **Thinking**: the recording is sent to the existing transcription call and dropped. Empty text goes back
   to listening ("No speech detected"). Otherwise the text is sent as an ordinary user message
   (`sendMessage`, same permission engine and run as typing; there is no voice code path in the daemon).
3. **Speaking**: while the run streams, complete sentences (`takeSpeakable`) go through `markdownToSpeech`
   into a queue of OS speech utterances, so speech starts at the first sentence. The per-bot voice and speed
   (or the global ones) are used.
4. **Barge-in**: while the bot speaks, a stricter detector (higher threshold, 400 ms onset) watches the
   microphone; sustained speech, the Interrupt button, drops the rest of the reply and returns to listening.
   Echo cancellation is requested from the WebView; headphones remain the reliable setup.
5. **End**: the End button, Escape in the panel, closing the chat or a fatal error releases the microphone.

The state machine is `lib/voiceCall/callMachine.ts` (pure reducer, tested). Errors it models: microphone
denied / missing / busy / unsupported (call ends, localized message and a retry), no speech (stays
listening), transcription failure (stays listening, hint), transcription not configured (call ends with the
Settings hint), reply failure (back to listening).

## Voice activity detection
`lib/voiceCall/vad.ts`, no dependencies: one RMS value per 50 ms frame from a Web Audio `AnalyserNode`.
- Speech threshold = `max(0.02, 3 x tracked room noise)`; the noise estimate follows only quiet frames.
- Onset 150 ms above the threshold, end after 1.2 s below it, utterances under 350 ms are dropped as noise,
  10 s without speech reports "no speech", hard cap 120 s per utterance (same as dictation).
- Not calibrated on real devices yet (see "Not verified"). Push-to-talk is the fallback when it misbehaves.

## UI
A non-modal docked panel with the bot avatar (animated with the call state: idle while listening, thinking,
working while speaking, waiting-approval, error), a level ring, status line (`aria-live`), optional captions,
mute, push-to-talk (hold the button or Space/Enter), captions toggle, interrupt, end. It uses design tokens
and lucide icons only and respects `prefers-reduced-motion` (the level ring stops moving). In mock mode
(`?mock=1`) a "Simulate speech" button drives the states without a microphone.

## Privacy and safety rules
- The microphone is opened only by tapping the call button, only while the panel is visible, and released
  when the call ends, fails, or the chat view unmounts. The panel shows a "Microphone on / muted" indicator
  at all times; mute disables the audio track itself.
- Audio exists only in memory: it is sent to the configured transcription service (the same one as
  dictation) and dropped. It is never written to disk, never logged. Captions (text) live in component
  memory only and vanish with the call.
- **Approvals are click-only (ADR 0021).** Transcribed speech is only ever sent as a normal chat message.
  The call code does not call any approval API, and a unit test scans `lib/voiceCall` and
  `components/voiceCall` for approval/decision calls. When a run waits for approval the call goes to the
  `approval` phase: the microphone events are ignored by the reducer (tested), and the panel tells the owner
  to use the card. The panel is non-modal so the card stays clickable. Saying "yes, approve" does nothing.
- Untrusted text: the bot's reply is read aloud as data; nothing in it can drive the call.

## Not in this slice
- A realtime provider (Gemini Live / OpenAI Realtime) behind the same panel; tool calls would still go
  through the permission engine.
- Per-device noise calibration, a language-aware end-of-speech model, streaming transcription.
- Recording starts when a listening turn starts, so the very first syllables after a barge-in may be cut.

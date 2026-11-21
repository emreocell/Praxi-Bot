# Voice chat mode (design note)

Status: superseded by `voice-call-mode.md`. Builds on read-aloud (OS speech synthesis), dictation (`POST /v1/voice/transcribe`)
and per-bot voice (voice and speed per bot, stored on the device).

## Goal
A hands-free toggle in the chat header: listen -> transcribe -> normal bot turn -> speak the reply -> listen again.

## Loop (chain mode, no realtime provider)
1. **Listen**: open the microphone (one WebView permission prompt, closed again when the mode ends).
   End of speech is detected on the device with a simple energy gate (WebAudio `AnalyserNode`): stop after
   about 1.2 s below the threshold, hard cap 120 s (same as dictation).
2. **Transcribe**: the existing dictation call. Empty text -> go back to listening, no turn is started.
3. **Answer**: the text is sent as an ordinary user message (same permission engine, same run). There is no
   separate voice code path in the daemon.
4. **Speak**: the final assistant text goes through `markdownToSpeech` and `speak()`. Barge-in: speech
   detected while speaking cancels the utterance and starts a new listening turn.
5. Loop until the user toggles it off, closes the chat, or the run needs an approval.

## Rules
- Approvals are never answered by voice: when a run waits for approval the loop pauses and shows the normal
  approval card (ADR 0021: critical actions need an on-screen confirmation).
- Echo: the microphone is closed while the bot speaks unless barge-in is enabled (headphones recommended).
- Privacy: audio is sent only to the configured dictation service, only while the mode is on; a visible
  recording indicator and an always-available stop button are required.
- Failure (no service configured, microphone denied) ends the mode with the existing messages.

## Open points
- A realtime provider (Gemini Live / OpenAI Realtime) can replace steps 1-2 and 4 later behind the same toggle;
  tool calls must still go through the permission engine.
- Silence threshold needs calibration per device (first second of noise floor).
- Implementation touches the composer and run-finished handling in the chat view, so it waits until the
  pending chat/composer PRs have landed.

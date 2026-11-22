# Voice call mode (hands-free chat)

Voice call mode in the desktop app lets you talk to a bot without typing. You
tap the phone button in the chat composer, speak, hear the bot's reply read
aloud, and speak again.

Behind the scenes, a voice call uses the same local dictation transcription
endpoint (`POST /v1/voice/transcribe`), sends your transcribed text as a normal
chat message, and reads the reply aloud with your operating system's speech
engine using that bot's configured voice and speed (or the global Settings
default). There is no separate voice code path in the daemon.

## Before you start

- Configure a dictation transcription service (Whisper-compatible or Gemini Live) in **Settings → General**. If none
  is configured, starting a call stops immediately with a prompt to add one.
- Allow microphone access for the Praxi desktop app.
- Use headphones if possible. The app asks the WebView for echo cancellation,
  but speakers feeding back into the microphone can still interrupt the bot
  early.
- Voice call mode is available only in the **desktop app**, not in the `bot` CLI
  or [Telegram](telegram.md).

## Choosing the speech-to-text provider

**Settings → General → Dictation service** has a **Provider** menu that applies to both
dictation and voice calls:

- **Whisper-compatible (OpenAI / local)**: an OpenAI-style `/audio/transcriptions`
  service, cloud or a local server (the default).
- **Gemini Live**: Google's Live API, used only to turn your speech into text. Paste a
  Gemini API key (from Google AI Studio) and, optionally, another Live model name; the
  default is `gemini-3.5-transcribe-live`. The key goes straight into the secret store and
  is never shown again. The daemon, not the app window, talks to Google, one short
  connection per utterance. Only the transcript is used: the answer still comes from the
  bot model you chose, and the Live model's own reply is ignored.

Audio leaves your machine only while you dictate or are in a call, and only to the service
you configured. Free-tier keys are subject to Google's own limits and terms.

## Starting and using a call

1. Open a chat with the bot you want to talk to.
2. Click the phone button (**Start voice chat**) in the chat composer toolbar.
3. A non-modal **Voice call** panel opens with the bot's avatar, a microphone
   indicator (**Microphone on** or **Microphone muted**), and a live status
   line.

The phone button stays available. If setup is incomplete, the panel explains
which key/provider to configure, how to restore owner authorization or daemon
connectivity, or how to allow microphone access in Windows Settings → Privacy &
security → Microphone (including access for desktop apps). Retry in the panel or
click the phone again after fixing the problem. A pending microphone prompt
times out instead of leaving the call connecting indefinitely.

If this chat already has a running bot, finish or stop that run and retry the
call. The microphone stays off while this explanation is shown. During a call,
clicking the phone focuses the existing panel; **End call** releases the microphone.

The call cycles through these steps automatically:

1. **Listening**: The app watches microphone loudness using energy-based voice
   activity detection (VAD). It starts a turn after 150 ms of speech above the
   tracked room-noise threshold and finishes after 1.2 s of silence. Clips
   shorter than 350 ms are dropped as noise. After 10 s without speech, it shows
   *"No speech detected. Still listening."* Each utterance is capped at 120 s.
2. **Thinking**: The recorded clip is sent in memory to your configured
   transcription service and then discarded. If speech was recognized, it is
   sent as an ordinary user message in the chat.
3. **Speaking**: As the bot's reply streams in, complete sentences are queued to
   your OS speech engine so playback starts on the first sentence (code blocks
   and tables are skipped in speech).

### Interrupting (barge-in) and call controls

While the bot is speaking, speaking loudly for at least 400 ms—or clicking
**Interrupt**—stops the rest of the spoken reply and switches back to
listening.

Panel controls:

- **Mute / Unmute microphone**: Disables or re-enables the microphone audio
  track.
- **Push to talk**: Switches off automatic voice detection and shows a **Hold to
  talk** button (also works by holding `Space` or `Enter` while focused on the
  button). Useful in noisy rooms where VAD triggers early or late.
- **Captions**: Toggles live captions of what you said and what the bot is
  saying.
- **Interrupt**: Stops the current spoken reply (shown while the bot is
  speaking).
- **End call**: Releases the microphone and closes the panel. Pressing `Esc`
  inside the panel or closing the chat also ends the call.

## What it can and cannot do

- **What it can do**: Hold multi-turn hands-free conversations, switch to
  push-to-talk on the fly, read replies sentence by sentence in the bot's
  chosen voice, and show live captions.
- **What it cannot do**:
  - Work without a configured transcription service or a working microphone.
  - Approve or deny tool calls by voice (see below).
  - Calibrate room noise per device automatically yet; use **Push to talk** if
    VAD misses speech or triggers on background noise.
  - Capture the very first syllables right after an automatic barge-in, because
    the turn's recorder starts when the call switches back to listening.

## Privacy and safety

- **Explicit microphone control**: The microphone opens only when you click the
  phone button, stays open only while the call panel is visible, and is released
  when you click **End call**, press `Esc`, leave the chat, or hit a fatal error.
  Muting disables the audio track itself.
- **No audio saved to disk**: Recorded audio exists only in memory until it is
  sent to your configured transcription service and dropped. It is never written
  to disk or logged. Captions stay in component memory only and vanish when the
  call ends.
- **Approvals are click-only**: Transcribed speech is only ever sent as a plain
  chat message. Voice call mode cannot call any approval API. When a bot needs
  permission for a tool call, the call enters **Waiting for your approval**,
  ignores microphone input, and reminds you to use the card in the chat. Because
  the panel is non-modal, the approval card stays clickable. Saying *"yes,
  approve"* has no effect.
- **Replies are data**: Bot responses are only read aloud; text in a reply
  cannot trigger call actions.

# Live voice conversation (Gemini Live)

Status: implemented (daemon routes in `crates/daemon-api/src/voice_live.rs`, provider session in
`apps/cli/src/voice_live.rs`, UI in `apps/desktop/ui/src/components/liveVoice/`).
Sits next to, and does not replace, [voice call mode](voice-call-mode.md) (dictation → bot → read aloud).

## Why a second voice mode

Voice call mode is a pipeline: speech-to-text, the bot's own model, text-to-speech. It works with
any bot model but each turn waits for the whole chain. Live voice talks to Google's **Gemini Live API**
(native audio in, native audio out, one open `WebSocket`), so the answer starts within a moment and the
owner can interrupt by speaking. The price is that the voice you hear is the Gemini Live model, not the
bot's own model. So the Live model is only the *voice*: anything beyond conversation is handed to the
bot, which does it through the normal run, permission engine and approval cards.

## Facts checked against Google's documentation (2026-11-27)

The Live docs moved from `ai.google.dev` to `aistudio.google.com/docs/live-api`; nothing below is from
memory. Items we could not confirm are listed under "Unconfirmed".

- **Model:** `gemini-3.8-live` (released September 2026, "the default option for low-latency,
  audio-to-audio voice agent experiences", no shutdown date announced). Also listed:
  `gemini-3.8-live-extended-thinking`, and legacy `gemini-3.1-flash-live-preview` and
  `gemini-2.5-flash-native-audio-preview-12-2025` (shutdown not before 2027-03-31). The model is a setting
  (`live_model`), so a rename does not need a release.
- **Endpoint:** `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=<API key>`
  (the same endpoint the dictation provider already uses; the key is only in that URL, which is never logged
  and never put in an error). The connection is checked before it is made: `wss://`, or `ws://` only to a
  loopback host (tests, a local proxy), and no credentials, query or fragment in the base (`validate_ws_base`,
  also called inside `connect`). No HTTP proxy is used for the socket. Ephemeral tokens exist for browser clients; we do not need them because the
  daemon, not the WebView, holds the socket and the key.
- **Session:** first message `setup` (model, `generationConfig.responseModalities = ["AUDIO"]`, voice,
  `systemInstruction`, `tools`, `inputAudioTranscription`, `outputAudioTranscription`, `sessionResumption`,
  `contextWindowCompression`), then wait for `setupComplete`. Audio in is raw 16-bit little-endian PCM at 16 kHz
  (`realtimeInput.audio`, `audio/pcm;rate=16000`); audio out is 24 kHz PCM16 in `serverContent.modelTurn.parts[].inlineData`.
  The server sends JSON in binary frames (text frames are accepted too). `interrupted` is sent when the owner talks
  over the model. `3.8` allows only the `AUDIO` response modality and no `thinking_level`.
- **Limits:** a connection lasts about 10 minutes (a `goAway` message comes first); audio-only sessions are limited
  to 15 minutes without compression. With `contextWindowCompression` and `sessionResumption` a conversation can go
  on across connections. Context window 131,072 tokens in, 65,536 out.
- **Price (paid tier, per 1M tokens):** audio in $3.00 (about $0.005/min), audio out $12.00 (about $0.018/min),
  text in $0.75, text out $4.50. A free tier exists and is "used to improve our products". Google Search grounding
  is not used here. Prices and limits are Google's and can change: the guide links to the pricing page instead of
  copying numbers beyond "a few cents per minute".
- **Languages:** the model picks the language itself (it cannot be forced); Turkish (`tr`) is in the supported list.

### Unconfirmed (and what we did about it)

- Where `scheduling` for a function response sits on the wire (top level or inside `response`) and its enum
  spelling (`INTERRUPT` vs `INTERRUPTED`) differ between pages. We send **no** `scheduling` and rely on the
  documented default for non-blocking calls (the model says the result when it is next quiet).
- No concurrent-session limit or per-model RPM/TPM figures for Live are published in the pages we could read.
  An over-limit refusal arrives as a close frame and is shown as a plain error.
- Whether output `inlineData` carries a `mimeType`: we do not need it (the rate is fixed at 24 kHz).

## Architecture

```
 microphone ─► app (16 kHz PCM16, 100 ms chunks) ─POST /v1/voice/live/{id}/audio─► daemon ─ws─► Gemini Live
 speaker   ◄─ app (24 kHz PCM16 queue)  ◄─ SSE events of POST /v1/voice/live ─── daemon ◄─ws─
```

- `POST /v1/voice/live` opens a session and answers with a **server-sent event stream**: `ready`, then `audio`
  (base64 PCM16), `you` / `bot` (transcript fragments), `turn_complete`, `interrupted`, `tool_call`,
  `tool_cancelled`, `reconnecting`, and finally `ended` (`limit`, `idle`, `error`, `closed`). Closing the response
  (or `DELETE /v1/voice/live/{id}`) ends the session and the provider connection.
- Uploads: `/audio` (raw PCM16, at most one second per request), `/tool` (the bot's answer to a `tool_call`),
  `/note` (a short update the voice model should pass on, e.g. "waiting for the owner's approval").
- `POST /v1/chats/{id}/voice-transcript` writes the finished conversation as ordinary `user` / `assistant`
  messages; it starts no run. The `user` lines are the owner's own speech (speech-to-text of their microphone,
  in their own session) and stay the owner's words. The `assistant` lines are the voice model's output, which
  can repeat what the bot read on the web or in a mail, so they are stored as **untrusted** content
  (`exposure_untrusted`, source `gemini-live`): a later run that reads the chat is treated as having seen
  untrusted text. Session ids are random (v4).
- **One conversation at a time** per daemon (a second open answers 409). Everything is **owner-only**: the routes
  are in `OWNER_ROUTES`, so a bearer token alone (a CLI engine, a script, a paired phone) is refused. Opening
  a microphone stream to a paid cloud service and spending the stored key is the owner's own action.
- The embedder (`VoiceHost`) owns the provider: key from the secret store (the Gemini key set under Bots & models,
  or the separate voice key, same rule as Gemini dictation), model and voice from `voice.json`
  (`live_model`, `live_voice`).
- **Lifetime:** 30 minutes at most; 5 minutes with nothing said, heard or handed over closes it as `idle` (an open
  microphone must not bill for hours). When the provider announces a `goAway` or the socket drops, the daemon
  reconnects with the latest resumption handle (`reconnecting` event), at most 8 times.

## The voice model has one function

`ask_bot(request)`. It cannot touch files, the web, the computer or anything else itself. When it calls it, the
daemon passes a `tool_call` event to the app; the app sends `request` as an **ordinary chat message** (same
path as typing: secret detection, the bot's permission preset, approval cards), waits for the run and
returns the bot's final text through `/tool`. Any other function name, or an empty request, is refused by the
daemon at once and never reaches the app. Consequences:

- Permission rules are unchanged: **Full** access still asks nothing except about money; **Ask** asks on screen.
- Approvals stay **click-only**. The voice model is told not to ask for permission itself; while a run waits for a
  click the panel says so, and a note tells the voice model to point at the screen. Saying "yes" does nothing.
- Prompt injection: text in tool results is information, not instructions (the system instruction says so), and the
  only thing a hijacked voice model can do is send another chat message, which is checked like any other.
- A request that contains a secret (key, card number) is not sent, as in voice call mode.

## Audio and privacy

- Audio is **never stored by default**. The daemon relays it and keeps nothing; the app keeps nothing.
- Setting `live_keep_audio` (Settings → General → Dictation → Live voice, default off) writes two WAV files per conversation
  (`<ms>-<random>-you.wav` 16 kHz and `<ms>-<random>-assistant.wav` 24 kHz) to `<data dir>/voice-live/`. They never
  leave the computer. The folder is `0700` and the files `0600` (Unix; Windows keeps the profile's permissions).
  The folder is capped at **200 MB** in total: before a conversation starts and after it ends the oldest `.wav`
  files are deleted until it fits (one full 30-minute conversation is about 145 MB). Nothing else deletes them;
  delete the folder to remove them all.
- Audio does go to Google while a conversation is open. On the free tier Google may use it to improve its products.
- The transcript becomes chat messages after the conversation (that is the point of the feature); it is not
  stored while the conversation is still going on, only held in the app.

## Not in this version

Camera or screen input, Google Search grounding, proactive audio tuning, choosing a language, push-to-talk
(automatic activity detection decides), and talking to a bot on a paired server (the conversation is opened on
this computer; the bot may still work on the server, because the handover is an ordinary chat message).

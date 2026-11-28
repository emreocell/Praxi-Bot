# Live voice conversation (Talk)

**Talk** opens a calm voice screen over the chat. You speak, the assistant answers out loud almost at
once, and you can interrupt it just by speaking. When you finish, the conversation is added to the chat as
ordinary messages.

It uses Google's **Gemini Live** (native audio, low latency). The voice you hear is Gemini's; whenever you
ask for something to be *done* (read a file, search the web, send an e-mail), the voice hands the request to
your bot, which does it the normal way with the permissions you set for it.

## Before you start

- You need a **Gemini API key** (from Google AI Studio). If you already added Gemini under **Bots & models**,
  Talk uses that key. Otherwise add a key under **Settings → General → Dictation → Gemini Live**. The key stays in the
  secret store and is never shown again.
- Allow microphone access for the desktop app. Use headphones if you can: speakers feeding back into the
  microphone make the assistant interrupt itself.
- Talk is available in the **desktop app**.

## Talking

1. Open a chat and click **Talk** next to the microphone in the message box.
2. The voice screen shows a glowing orb that moves with your voice and the assistant's, the words as they
   are said, and a clear **Microphone is on** indicator.
3. Just speak. To interrupt the assistant, speak over it, or press **Stop talking**.
4. **Mute** stops sending your microphone to Google. **End** closes the conversation.

A conversation lasts up to 30 minutes and ends by itself after 5 quiet minutes. The connection is renewed in
the background when Google asks for it.

## Asking the bot to do things

Say what you want. The voice answers "on it", the request appears in the chat as a normal message and the bot
runs exactly as if you had typed it.

- **Permissions do not change.** With **Full** access the bot asks nothing except about money; with **Ask** an
  approval card appears. The voice screen stays out of the way so the card is clickable.
- **Approvals are click-only.** Saying "yes, go ahead" does nothing; use the card. While a card waits, the voice
  tells you to look at the screen.
- A request that contains a password, key or card number is not sent.

## What is stored

- **Audio is not stored** unless you turn on **Settings → General → Dictation → Live voice → Keep audio of live
  conversations on this computer**. Then two WAV files per conversation (yours and the assistant's) are saved in
  the `voice-live` folder of the data directory, readable only by you. They are never uploaded. The folder
  keeps at most 200 MB: when it is over, the oldest recordings are deleted first. Delete the folder to remove
  them all.
- While a conversation is open your audio goes to Google. On the free tier Google may use it to improve its
  products. Check Google's terms for the paid tier.
- When you end the conversation, the words (yours and the assistant's) are saved in the chat as text. The
  assistant's words are marked as untrusted, like web pages, so a later task that reads them asks before it
  acts on them.

## Cost

Gemini Live is billed by Google on audio length: a few cents per minute on the paid tier, free within the
free tier's limits (and then Google's terms apply). See the
[Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing) page for the current numbers. Each bot's
spending limits apply to the bot's own work, not to this conversation.

## Settings

**Settings → General → Dictation → Live voice**: the model (default `gemini-3.8-live`; change it if Google renames
it), the voice (30 prebuilt voices, default Kore) and *Keep audio*.

## If it does not work

| You see | What to do |
| --- | --- |
| "Add a Gemini key" | Add the key as described above. |
| "Google refused the connection" | The key is wrong or has no access to the Live API, or the model name is outdated. Check both in Settings. |
| "Another conversation is open" | End it first; only one Talk conversation runs at a time. |
| Microphone errors | Same as [dictation](voice-call.md): allow the microphone in your system's privacy settings. |

Related: [Voice call mode](voice-call.md) (works with any bot model, slower) and
[Chatting and approvals](chatting-and-approvals.md).

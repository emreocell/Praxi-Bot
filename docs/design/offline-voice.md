# Offline voice follow-up (#571)

Main already has Whisper and Silero dictation, push-to-talk and Telegram voice notes (#644). This follow-up extends that component with local reply playback and resumable downloads. Cloud dictation and the OS speech engine remain selectable.

- Reuse the managed local runtime downloader and the pinned voice catalog. Preserve the voice download host allowlist, verify SHA-256 and size before use, and check disk space before downloading. The owner sees the model licence, source and size before choosing to download.
- Keep inference behind the existing `local-voice` feature. Run native inference in a bounded child process with private standard I/O and no listening socket. Use the loopback daemon API for desktop read-aloud. Never fall back to a cloud or OS voice when offline synthesis is requested.
- Use Sherpa ONNX's int8 Supertonic 3 CPU model for English, Turkish and German. Its weights are an owner-initiated download under OpenRAIL-M. The model licence and release build defaults are open for maintainer review.
- Exercise download, HTTP and audio boundaries with hermetic fixtures. An opt-in smoke test can use the pinned models to check actual synthesis without downloading anything in CI.

## Limits that hold below the route

- Synthesis input is checked by one function (`SynthesizeInput::validate`: 1 to 1000 characters, no NUL, language tag of ASCII letters, digits and `-`) in the route, in `VoiceHost` and again inside the worker process, so a direct caller of `VoiceControl::synthesize` or a forged worker header cannot make the worker synthesize an unbounded text.
- Dictation with `local_only` is enforced by the daemon: unless the configured provider is on-device the request is refused (409) and nothing is sent to a network provider. A paired peer cannot use the host's on-device recogniser (403); the owner on the host can.
- Every download redirect passes the rule the first URL passes (https, no credentials, default port, pinned host roots): `provider_catalog::local_runtime::download::{is_pinned_https_url, pinned_redirects}`. The voice catalog and the managed runtime/browser downloads use their own host lists with the same code.
- Auto read-aloud ("Speak replies" + "Offline read-aloud") is an explicit owner setting. A reply read aloud on its own is cut to 1000 characters at a sentence boundary and waits for the current utterance instead of cutting it off; one utterance plays at a time and a click on Read aloud or Stop always wins.

# Offline voice follow-up (#571)

Main already has Whisper and Silero dictation, push-to-talk and Telegram voice notes (#644). This follow-up extends that component with local reply playback and resumable downloads. Cloud dictation and the OS speech engine remain selectable.

- Reuse the managed local runtime downloader and the pinned voice catalog. Preserve the voice download host allowlist, verify SHA-256 and size before use, and check disk space before downloading. The owner sees the model licence, source and size before choosing to download.
- Keep inference behind the existing `local-voice` feature. Run native inference in a bounded child process with private standard I/O and no listening socket. Use the loopback daemon API for desktop read-aloud. Never fall back to a cloud or OS voice when offline synthesis is requested.
- Evaluate Sherpa ONNX's Supertonic 3 CPU model for English, Turkish and German. The downloadable weights use OpenRAIL-M. The project will not bundle them or introduce a GPL engine or phonemizer. The model licence choice is open for maintainer review.
- Exercise download, HTTP and audio boundaries with hermetic fixtures. An opt-in smoke test can use the pinned models to check actual synthesis without downloading anything in CI.

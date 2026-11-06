# Praxi-Bot

A self-hosted, open-source AI agent that controls desktops, manages tasks, and
connects to external services. It runs on your own machines and talks to the
LLM providers you choose (cloud APIs or local models).
The core daemon is written in Rust (tokio + axum); node-agents dial out to it
from every machine you want to control, requiring no inbound ports.

**Status: early development — not usable yet.**
No stable API, no releases. Expect breaking changes at any time.

## Build

Requirements: Rust 1.88+, MSVC build tools (Windows) or a standard C toolchain
(Linux/macOS).

```sh
# Check everything compiles
cargo build --workspace

# Build only the CLI
cargo build -p cli

# Run the CLI
./target/debug/bot --version
```

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

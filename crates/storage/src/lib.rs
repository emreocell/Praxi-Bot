//! SQLite and NDJSON event-log storage layer for messages, audit, and stream replay.
//!
//! Provides typed repository interfaces over SQLite for messages and audit records,
//! and per-thread NDJSON append logs for high-throughput event streaming.

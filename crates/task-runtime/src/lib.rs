//! Scheduled task execution and background job management for the core daemon.
//!
//! Drives cron-style and event-triggered background jobs, enforcing timeouts
//! and retry policies without blocking the main agent event loop.

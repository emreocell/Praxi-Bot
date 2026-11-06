//! Windows OS backend: UI Automation, `SendInput`, and screenshot capture.
//!
//! Implements the Desktop trait using UI Automation COM APIs on a dedicated
//! COM thread, with `SendInput` for keyboard and mouse actions.

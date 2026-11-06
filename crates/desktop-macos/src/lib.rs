//! macOS OS backend: Accessibility API, `CGEvent` input, and screenshot capture.
//!
//! Implements the Desktop trait using the macOS AX API for element inspection
//! and `CGEventPostToPid` for synthesised keyboard and mouse events.

//! Linux OS backend: AT-SPI accessibility, xdotool/ydotool input, and screenshot capture.
//!
//! Implements the Desktop trait using AT-SPI over D-Bus and X11/Wayland input
//! injection, with optional Xvfb support for headless environments.

//! Browser automation via Chrome `DevTools` Protocol: snapshot, element refs, and DOM diff.
//!
//! Launch (or attach to) Chrome/Edge, get a compact ref-indexed snapshot of a page
//! (`[e12] button "Sign in"`), act on refs, and receive the refreshed view as a diff.
//!
//! Parts of this crate are adapted from vercel-labs/agent-browser (Apache-2.0); see
//! `THIRD_PARTY_NOTICES.md`.

#![allow(
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::module_name_repetitions
)]

pub mod actions;
pub mod browser;
pub mod client;
pub mod diff;
pub mod dom;
pub mod error;
pub mod launch;
pub mod page;
pub mod snapshot;

pub use actions::{
    ActionResult, KeySpec, ScreenshotOptions, ScreenshotTarget, WaitCondition, parse_key,
};
pub use browser::{Browser, TargetInfo};
pub use client::{CdpClient, CdpEvent};
pub use diff::{SnapshotDiff, diff, diff_text};
pub use error::{Error, Result};
pub use launch::{LaunchOptions, find_browser};
pub use page::{DialogAction, DialogInfo, DialogOptions, Page};
pub use snapshot::{RefEntry, RefMap, SnapNode, Snapshot, SnapshotOptions, build_snapshot};

//! Browser automation via Chrome `DevTools` Protocol: snapshot, element refs, DOM diff,
//! and bounded live-view screencast + viewport-mapped human input.
//!
//! Launch (or attach to) Chrome/Edge, get a compact ref-indexed snapshot of a page
//! (`[e12] button "Sign in"`), act on refs, and receive the refreshed view as a diff,
//! or attach a [`BrowserViewTarget`] (via [`Page::live_view`] or [`BrowserViewTarget::new_idle`])
//! implementing [`live_view`] traits for bounded JPEG screencasting and takeover input.
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
pub mod attach;
pub mod browser;
pub mod client;
pub mod cursor;
pub mod destination;
pub mod diff;
pub mod dom;
pub mod downloads;
pub mod error;
pub mod hidden;
pub mod host;
pub mod human;
pub mod launch;
pub mod live_view;
pub mod page;
pub mod profile;
pub mod proxy;
pub mod saved_sites;
pub mod snapshot;
pub mod web_url;

pub use actions::{
    ActionResult, FieldTarget, FrameIdentity, KeySpec, ReadTarget, ReadValue, ScreenshotOptions,
    ScreenshotTarget, StepGuard, WaitCondition, parse_key,
};
pub use actions::{ConsentChoice, ConsentOutcome};
pub use actions::{
    Grounded, POINTER_MAX_EDGE, PointerButton, PointerOp, PointerRequest, PointerView, Region,
    pointer_scale,
};
pub use actions::{ScrollRequest, ScrollTo, SelectUnit, TextSelection};
pub use browser::{Browser, TargetInfo};
pub use client::{
    CdpClient, CdpClientConfig, CdpEvent, DEFAULT_MAX_SCREENCAST_EVENT_BYTES,
    DEFAULT_MAX_SCREENCAST_IMAGE_BYTES, ScreencastMetadata,
};
pub use destination::{
    AddrClass, DestinationCheck, ServedNetwork, Verdict, classify_ip, classify_url,
};
pub use diff::{SnapshotDiff, diff, diff_text};
pub use downloads::{
    Download, DownloadPolicy, DownloadReport, list_folder, mime_of, plain_path, sanitize_file_name,
};
pub use error::{Error, Result};
pub use human::{Feel, HumanLike};
pub use launch::{LaunchOptions, find_browser, find_browser_cached};
pub use live_view::{
    BROWSER_INPUT_LIMITATION, BrowserFrameSource, BrowserInputSink, BrowserViewConfig,
    BrowserViewStatus, BrowserViewTarget, ManualClock, TakeoverPinHook, ViewClock,
    ViewportGeometry,
};
pub use page::{DialogAction, DialogInfo, DialogOptions, Page};
pub use proxy::FilterProxy;
pub use snapshot::{RefEntry, RefMap, SnapNode, Snapshot, SnapshotOptions, build_snapshot};
pub use web_url::{is_public_web_url, popup_url_allowed};

//! Example: stream a bounded Watch -> Live screencast and send viewport input via `live_view`.
//!
//! Run with:
//! `cargo run -p browser-cdp --example live_view`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use browser_cdp::{Browser, BrowserViewConfig, LaunchOptions};
use live_view::{Button, EffectiveProfile, InputEvent, PointerPhase, ViewTarget};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = Browser::launch(LaunchOptions::default()).await?;
    let page = browser.new_page("https://example.com").await?;

    let pinned = Arc::new(AtomicBool::new(false));
    let pin_flag = pinned.clone();
    let target = page.live_view(BrowserViewConfig {
        takeover_pin: Some(Arc::new(move |active| {
            pin_flag.store(active, Ordering::SeqCst);
        })),
        ..BrowserViewConfig::default()
    });

    let source = target.frame_source();
    let mut sub = source.subscribe(EffectiveProfile::watch()).await?;
    let _ = tokio::time::timeout(Duration::from_secs(2), sub.frames.changed()).await;

    source.set_profile(EffectiveProfile::live()).await?;
    if let Some(sink) = target.input_sink() {
        sink.set_takeover_active(true).await?;
        sink.inject(&InputEvent::Pointer {
            phase: PointerPhase::Move,
            x: 0.5,
            y: 0.5,
            button: None,
            buttons: 0,
            modifiers: Vec::new(),
        })
        .await?;
        sink.inject(&InputEvent::Pointer {
            phase: PointerPhase::Down,
            x: 0.5,
            y: 0.5,
            button: Some(Button::Left),
            buttons: 1,
            modifiers: Vec::new(),
        })
        .await?;
        sink.release_pressed_inputs().await?;
        sink.set_takeover_active(false).await?;
    }

    drop(sub);
    browser.close().await?;
    Ok(())
}

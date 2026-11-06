//! Integration tests that need a real interactive Windows desktop.
//! Run with `cargo test -p desktop-windows -- --ignored --test-threads=1`.
#![cfg(windows)]

use std::time::{Duration, Instant};

use desktop_api::{
    Action, ClickMethod, DesktopBackend, DesktopError, Quality, Region, Target, snapshot::AppSpec,
};
use desktop_windows::WindowsBackend;

fn block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

#[test]
#[ignore = "needs a desktop session"]
fn windows_and_foreground_snapshot() {
    let b = WindowsBackend::new().unwrap();
    assert!(b.capabilities().accessibility_tree);
    let wins = block(b.windows()).unwrap();
    assert!(!wins.is_empty());
    let snap = block(b.snapshot(Some(wins[0].id))).unwrap();
    assert_eq!(snap.generation, 1);
    assert!(!snap.nodes.is_empty());
    let snap2 = block(b.snapshot(Some(wins[0].id))).unwrap();
    assert_eq!(snap2.generation, 2);
}

#[test]
#[ignore = "needs a desktop session"]
fn screenshot_is_jpeg_or_png() {
    let b = WindowsBackend::new().unwrap();
    let jpg = block(b.screenshot(Region::FullScreen, Quality::Low)).unwrap();
    assert_eq!(&jpg.data[..2], &[0xFF, 0xD8]);
    assert_eq!(jpg.mime_type, "image/jpeg");
    let png = block(b.screenshot(Region::FullScreen, Quality::Lossless)).unwrap();
    assert_eq!(&png.data[1..4], b"PNG");
}

#[test]
#[ignore = "needs a desktop session"]
fn stale_generation_is_rejected() {
    let b = WindowsBackend::new().unwrap();
    let wins = block(b.windows()).unwrap();
    let snap = block(b.snapshot(Some(wins[0].id))).unwrap();
    let err = block(b.act(Action::Click {
        target: Target::ByIndex {
            index: 0,
            generation: snap.generation + 7,
        },
        method: ClickMethod::Accessibility,
    }))
    .unwrap_err();
    assert!(matches!(err, DesktopError::ElementNotFound { .. }));
}

/// Opens an Explorer window on a temp dir, toggles the view-mode radio buttons
/// through the `SelectionItem` pattern and checks the state flip.
#[test]
#[ignore = "needs a desktop session; opens and leaves an Explorer window"]
fn explorer_selection_pattern_click() {
    let b = WindowsBackend::new().unwrap();
    let dir = std::env::temp_dir().join("praxi_uia_it");
    std::fs::create_dir_all(&dir).unwrap();
    block(b.launch(AppSpec {
        executable: "explorer.exe".into(),
        args: vec![dir.display().to_string()],
    }))
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let id = loop {
        let wins = block(b.windows()).unwrap();
        if let Some(w) = wins.iter().find(|w| w.title.contains("praxi_uia_it")) {
            break w.id;
        }
        assert!(Instant::now() < deadline, "explorer window did not appear");
        std::thread::sleep(Duration::from_millis(200));
    };
    std::thread::sleep(Duration::from_secs(1));

    let snap = block(b.snapshot(Some(id))).unwrap();
    let radios: Vec<_> = snap
        .nodes
        .iter()
        .filter(|n| n.role == "radiobutton")
        .collect();
    assert!(radios.len() >= 2, "expected view-mode radio buttons");
    let target = radios
        .iter()
        .find(|n| !n.states.iter().any(|s| s == "selected"))
        .unwrap();
    let res = block(b.act(Action::Click {
        target: Target::ByIndex {
            index: target.index,
            generation: snap.generation,
        },
        method: ClickMethod::Accessibility,
    }))
    .unwrap();
    assert!(res.dispatched);
    std::thread::sleep(Duration::from_millis(500));

    let after = block(b.snapshot(Some(id))).unwrap();
    let again = after
        .nodes
        .iter()
        .find(|n| n.role == "radiobutton" && n.name == target.name)
        .unwrap();
    assert!(again.states.iter().any(|s| s == "selected"));

    // The old generation is now stale.
    let err = block(b.act(Action::Click {
        target: Target::ByIndex {
            index: target.index,
            generation: snap.generation,
        },
        method: ClickMethod::Accessibility,
    }))
    .unwrap_err();
    assert!(matches!(err, DesktopError::ElementNotFound { .. }));
}

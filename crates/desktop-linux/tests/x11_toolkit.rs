//! Live checks of the X11 parts of the desktop toolkit. They need an X server
//! (`xvfb-run -a cargo test -p desktop-linux --test x11_toolkit`) and skip
//! themselves, loudly, when there is none.
#![cfg(target_os = "linux")]

use desktop_api::DesktopBackend as _;
use desktop_linux::LinuxBackend;

fn backend() -> Option<LinuxBackend> {
    let b = LinuxBackend::default();
    if b.capabilities().has_display {
        Some(b)
    } else {
        eprintln!("skipped: no X display (run under xvfb-run to exercise it)");
        None
    }
}

#[tokio::test]
async fn the_clipboard_round_trips_text_and_the_next_write_replaces_it() {
    let Some(b) = backend() else { return };
    let first = "first \u{11f}\u{15f}\u{131} \u{20ac} \u{1f600}\nsecond line";
    b.set_clipboard_text(first).await.unwrap();
    assert_eq!(b.clipboard_text().await.unwrap().as_deref(), Some(first));
    // A second write takes the selection over from the first server thread.
    b.set_clipboard_text("replaced").await.unwrap();
    assert_eq!(
        b.clipboard_text().await.unwrap().as_deref(),
        Some("replaced")
    );
}

#[tokio::test]
async fn an_operation_on_a_window_that_is_gone_is_an_error_not_a_guess() {
    let Some(b) = backend() else { return };
    let err = b
        .window_op(0x7fff_fff0, desktop_api::WindowOp::Minimize, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no longer exists"), "{err}");
}

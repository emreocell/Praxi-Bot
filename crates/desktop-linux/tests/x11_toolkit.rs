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

/// A plain GTK window of another program (python3), the kind of window a window operation
/// may touch. Skipped, loudly, when python3-gi is missing.
const FIXTURE: &str = r#"
import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk
w = Gtk.Window(title="Window ops fixture")
w.set_default_size(420, 240)
w.connect("destroy", Gtk.main_quit)
w.show_all()
Gtk.main()
"#;

struct KillOnDrop(std::process::Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn fixture_window(b: &LinuxBackend) -> Option<(KillOnDrop, desktop_api::WindowInfo)> {
    let ok = std::process::Command::new("python3")
        .args([
            "-c",
            "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
        ])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skipped: python3 with GTK 3 (python3-gi) is not installed");
        return None;
    }
    let child = std::process::Command::new("python3")
        .args(["-c", FIXTURE])
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the fixture window");
    let pid = child.id();
    let guard = KillOnDrop(child);
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Some(w) = b
            .windows()
            .await
            .unwrap()
            .into_iter()
            .find(|w| w.pid == pid)
        {
            return Some((guard, w));
        }
    }
    panic!("the fixture window did not appear");
}

/// The window's width, or `None` while the listing does not show it (a window that is
/// being resized is briefly not viewable).
async fn width_of(b: &LinuxBackend, id: u64) -> Option<f64> {
    b.windows()
        .await
        .unwrap()
        .into_iter()
        .find(|w| w.id == id)
        .map(|w| w.bounds.width)
}

#[tokio::test]
async fn a_window_operation_is_refused_when_the_window_is_not_the_program_that_was_approved() {
    use desktop_api::{ActGuard, DesktopError, WindowOp};
    let Some(b) = backend() else { return };
    let Some((_child, win)) = fixture_window(&b).await else {
        return;
    };
    // The program behind the window is named like on Windows: image, pid, start time.
    let facts = b
        .focus_target(win.id)
        .await
        .unwrap()
        .expect("the window's program is identified");
    assert_eq!(facts.identity.pid, win.pid);
    assert!(facts.identity.is_complete(), "{facts:?}");
    assert_eq!(facts.elevated, Some(false));
    assert!(!facts.owned && !facts.class.is_empty(), "{facts:?}");

    let resize = WindowOp::Resize {
        width: 520,
        height: 330,
    };
    // GTK lays the window out to its default size just after mapping it.
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    let before = width_of(&b, win.id).await.expect("the window is listed");
    // The approval was shown for another program (same pid, another start time): refused,
    // and nothing is sent.
    let mut other = facts.identity.clone();
    other.start_time += 1;
    let err = b
        .window_op(win.id, resize, Some(ActGuard { expect: other }))
        .await
        .unwrap_err();
    assert!(matches!(err, DesktopError::GuardRefused(_)), "{err}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        width_of(&b, win.id)
            .await
            .is_some_and(|w| (w - before).abs() < 1.0),
        "the window was resized"
    );

    // The approved program: the operation goes through.
    b.window_op(
        win.id,
        resize,
        Some(ActGuard {
            expect: facts.identity.clone(),
        }),
    )
    .await
    .expect("the approved program's window is resized");
    let mut resized = false;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if width_of(&b, win.id)
            .await
            .is_some_and(|w| (w - 520.0).abs() <= 4.0)
        {
            resized = true;
            break;
        }
    }
    assert!(resized, "the window did not take the new size");
}

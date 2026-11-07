//! Integration tests for `desktop-linux`.
//!
//! # Running the `#[ignore]` Xvfb + GTK integration test on Linux
//!
//! Install the headless X11, D-Bus, AT-SPI2, and GTK3 test dependencies (on
//! Debian/Ubuntu):
//!
//! ```sh
//! sudo apt-get update
//! sudo apt-get install -y \
//!   xvfb openbox dbus-x11 at-spi2-core libatk-adaptor libgail-common \
//!   python3 python3-gi gir1.2-gtk-3.0
//! ```
//!
//! Then run the ignored test inside an isolated `xvfb-run` + `dbus-run-session`
//! environment:
//!
//! ```sh
//! XDG_SESSION_TYPE=x11 WAYLAND_DISPLAY= \
//!   GTK_MODULES=gail:atk-bridge QT_ACCESSIBILITY=1 \
//!   xvfb-run -a -s "-screen 0 1920x1080x24" \
//!   dbus-run-session -- \
//!   cargo test -p desktop-linux --test gtk_integration -- --ignored --nocapture
//! ```

use desktop_api::DesktopError;
use desktop_linux::pure::detect_display_session;

#[test]
fn wayland_session_returns_clear_unsupported_error() {
    let err = detect_display_session(Some("wayland"), Some("wayland-0"), Some(":0"))
        .expect_err("wayland must be rejected in v0");
    assert!(matches!(err, DesktopError::Unsupported(_)));
    let msg = err.to_string();
    assert!(msg.contains("Wayland"));
    assert!(msg.contains("out of scope for v0"));
}

#[test]
fn xvfb_override_on_wayland_host_is_accepted() {
    let display = detect_display_session(Some("x11"), Some("wayland-0"), Some(":99"))
        .expect("explicit XDG_SESSION_TYPE=x11 with DISPLAY=:99 should succeed");
    assert_eq!(display, ":99");
}

#[cfg(target_os = "linux")]
const GTK_TEST_APP: &str = r#"
import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, GLib

class TestWindow(Gtk.Window):
    def __init__(self):
        super().__init__(title="Praxi GTK Test App")
        self.set_default_size(420, 240)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        self.add(box)
        self.entry = Gtk.Entry()
        self.entry.get_accessible().set_name("Query Input")
        box.pack_start(self.entry, False, False, 0)
        self.check = Gtk.CheckButton(label="Enable Option")
        box.pack_start(self.check, False, False, 0)
        self.button = Gtk.Button(label="Run Action")
        self.button.connect("clicked", lambda b: b.set_label("Action Completed"))
        box.pack_start(self.button, False, False, 0)

win = TestWindow()
win.connect("destroy", Gtk.main_quit)
win.show_all()
GLib.timeout_add_seconds(30, Gtk.main_quit)
Gtk.main()
"#;

#[cfg(target_os = "linux")]
fn prepare_headless_env() -> Option<std::process::Child> {
    use std::{
        process::{Command, Stdio},
        time::Duration,
    };

    let _ = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.a11y.Bus",
            "--object-path",
            "/org/a11y/bus",
            "--method",
            "org.freedesktop.DBus.Properties.Set",
            "org.a11y.Status",
            "IsEnabled",
            "<true>",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let wm = Command::new("openbox")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    std::thread::sleep(Duration::from_millis(400));
    wm
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires xvfb-run, dbus-run-session, at-spi2-core, openbox, and python3-gi (GTK 3)"]
fn xvfb_gtk_test_app_roundtrip() {
    use std::time::{Duration, Instant};

    use desktop_api::{
        Action, DesktopBackend, Quality, Region, Target, action::ClickMethod, render_snapshot,
        snapshot::AppSpec,
    };
    use desktop_linux::LinuxBackend;

    let mut wm_child = prepare_headless_env();

    futures::executor::block_on(async {
        let backend = LinuxBackend::new().expect("init LinuxBackend");
        let caps = backend.capabilities();
        assert_eq!(caps.platform, "linux");
        assert!(caps.accessibility_tree && caps.screenshot && caps.app_launch && caps.has_display);

        let pid = backend
            .launch(AppSpec {
                executable: "python3".into(),
                args: vec!["-c".into(), GTK_TEST_APP.into()],
            })
            .await
            .expect("launch GTK test app");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut window_id = None;
        while Instant::now() < deadline {
            let wins = backend.windows().await.expect("list windows");
            if let Some(w) = wins
                .iter()
                .find(|w| w.pid == pid || w.title.contains("Praxi GTK Test App"))
            {
                window_id = Some(w.id);
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let win_id = window_id.expect("Praxi GTK Test App window did not appear on X11");
        backend
            .activate_window(win_id)
            .await
            .expect("activate GTK window");
        std::thread::sleep(Duration::from_millis(600));

        let snap = backend
            .snapshot(Some(win_id))
            .await
            .expect("AT-SPI snapshot of GTK window");
        eprintln!("Snapshot:\n{}", render_snapshot(&snap));
        assert!(!snap.nodes.is_empty(), "expected interactive AT-SPI nodes");

        let edit_node = snap
            .nodes
            .iter()
            .find(|n| n.role == "edit")
            .expect("expected edit node in snapshot");
        backend
            .act(Action::SetValue {
                target: Target::ByIndex {
                    index: edit_node.index,
                    generation: snap.generation,
                },
                value: "headless-atspi".into(),
            })
            .await
            .expect("SetValue on GTK entry");

        let button_node = snap
            .nodes
            .iter()
            .find(|n| n.role == "button" && n.name.contains("Run Action"))
            .expect("expected Run Action button in snapshot");
        backend
            .act(Action::Click {
                target: Target::ByIndex {
                    index: button_node.index,
                    generation: snap.generation,
                },
                method: ClickMethod::Accessibility,
            })
            .await
            .expect("Click on GTK button");

        std::thread::sleep(Duration::from_millis(300));
        let img = backend
            .screenshot(Region::Window { window_id: win_id }, Quality::Medium)
            .await
            .expect("XGetImage screenshot of GTK window");
        assert_eq!(img.mime_type, "image/jpeg");
        assert!(img.width > 0 && img.height > 0 && img.data.len() > 2);
    });

    if let Some(mut wm) = wm_child.take() {
        let _ = wm.kill();
        let _ = wm.wait();
    }
}

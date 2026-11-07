//! X11 helpers over `x11rb`: window enumeration/activation, `XTest` input
//! injection, `GetImage` (`XGetImage`) screen capture, and process launch.

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use desktop_api::{
    DesktopError,
    snapshot::{AppSpec, Image, Pid, Quality, Rect, Region, WindowInfo},
};
use x11rb::{
    CURRENT_TIME, NONE,
    connection::Connection as _,
    protocol::{
        xproto::{
            Atom, AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ClientMessageEvent,
            ConfigureWindowAux, ConnectionExt as _, EventMask, GetKeyboardMappingReply,
            ImageFormat, InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
            MapState, StackMode, Window,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

use crate::pure::{
    Key, char_to_keysym, clamp_capture_rect, detect_display_session, parse_key, zpixmap_to_bgra,
};

fn x11_err<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> DesktopError + '_ {
    move |e| DesktopError::Accessibility(format!("{ctx}: {e}"))
}

fn shot_err<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> DesktopError + '_ {
    move |e| DesktopError::Screenshot(format!("{ctx}: {e}"))
}

/// Ensure we are on an X11 session (rejecting Wayland with [`DesktopError::Unsupported`]).
pub fn ensure_x11_session() -> Result<String, DesktopError> {
    let xdg = std::env::var("XDG_SESSION_TYPE").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let x11 = std::env::var("DISPLAY").ok();
    detect_display_session(xdg.as_deref(), wayland.as_deref(), x11.as_deref())
}

/// Check whether a valid X11 display is currently reachable.
#[must_use]
pub fn has_x11_display() -> bool {
    ensure_x11_session().is_ok() && x11rb::connect(None).is_ok()
}

fn connect() -> Result<(RustConnection, usize), DesktopError> {
    let dpy = ensure_x11_session()?;
    x11rb::connect(Some(&dpy)).map_err(x11_err("x11rb::connect"))
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, DesktopError> {
    Ok(conn
        .intern_atom(false, name)
        .map_err(x11_err("intern_atom"))?
        .reply()
        .map_err(x11_err("intern_atom reply"))?
        .atom)
}

fn read_u32_prop(conn: &RustConnection, win: Window, prop: Atom) -> Option<u32> {
    let reply = conn
        .get_property(false, win, prop, AtomEnum::ANY, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    reply.value32()?.next()
}

fn read_u32_list_prop(conn: &RustConnection, win: Window, prop: Atom) -> Option<Vec<u32>> {
    let reply = conn
        .get_property(false, win, prop, AtomEnum::ANY, 0, 4096)
        .ok()?
        .reply()
        .ok()?;
    let vals: Vec<u32> = reply.value32()?.collect();
    (!vals.is_empty()).then_some(vals)
}

fn read_string_prop(conn: &RustConnection, win: Window, prop: Atom) -> Option<String> {
    let reply = conn
        .get_property(false, win, prop, AtomEnum::ANY, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    if reply.value.is_empty() {
        return None;
    }
    let s = String::from_utf8_lossy(&reply.value)
        .trim_matches('\0')
        .trim()
        .to_string();
    (!s.is_empty()).then_some(s)
}

fn window_title(conn: &RustConnection, win: Window, net_wm_name: Atom) -> String {
    if let Some(title) = read_string_prop(conn, win, net_wm_name) {
        return title;
    }
    read_string_prop(conn, win, AtomEnum::WM_NAME.into()).unwrap_or_default()
}

fn window_pid(conn: &RustConnection, win: Window, net_wm_pid: Atom) -> u32 {
    read_u32_prop(conn, win, net_wm_pid).unwrap_or(0)
}

/// Screen-space geometry of `win` on `root`.
pub fn window_bounds_on_conn(conn: &RustConnection, root: Window, win: Window) -> Option<Rect> {
    let geom = conn.get_geometry(win).ok()?.reply().ok()?;
    let trans = conn
        .translate_coordinates(win, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    Some(Rect {
        x: f64::from(trans.dst_x),
        y: f64::from(trans.dst_y),
        width: f64::from(geom.width),
        height: f64::from(geom.height),
    })
}

fn resolve_active_window(
    conn: &RustConnection,
    root: Window,
    net_active_window: Atom,
    clients: &[u32],
) -> Option<Window> {
    if let Some(active) = read_u32_prop(conn, root, net_active_window)
        && active != 0
        && active != root
    {
        return Some(active);
    }
    let focus = conn.get_input_focus().ok()?.reply().ok()?.focus;
    if focus == 0 || focus == 1 || focus == root {
        return clients.last().copied();
    }
    if clients.contains(&focus) {
        return Some(focus);
    }
    // Walk up the parent chain to find the top-level client window.
    let mut cur = focus;
    for _ in 0..16 {
        let Ok(cookie) = conn.query_tree(cur) else {
            break;
        };
        let Ok(tree) = cookie.reply() else {
            break;
        };
        if clients.contains(&cur) || tree.parent == root {
            return Some(cur);
        }
        if tree.parent == 0 || tree.parent == cur {
            break;
        }
        cur = tree.parent;
    }
    Some(focus)
}

fn candidate_windows(conn: &RustConnection, root: Window, net_client_list: Atom) -> Vec<u32> {
    if let Some(list) = read_u32_list_prop(conn, root, net_client_list) {
        return list;
    }
    conn.query_tree(root)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|t| t.children)
        .unwrap_or_default()
}

fn build_window_info(
    conn: &RustConnection,
    root: Window,
    win: Window,
    focused_win: Option<Window>,
    net_wm_name: Atom,
    net_wm_pid: Atom,
) -> Option<WindowInfo> {
    let bounds = window_bounds_on_conn(conn, root, win)?;
    if bounds.width <= 1.0 || bounds.height <= 1.0 {
        return None;
    }
    Some(WindowInfo {
        id: u64::from(win),
        title: window_title(conn, win, net_wm_name),
        pid: window_pid(conn, win, net_wm_pid),
        bounds,
        focused: focused_win == Some(win),
    })
}

/// Information for a specific window ID.
pub fn window_info(window_id: u64) -> Option<WindowInfo> {
    let win = u32::try_from(window_id).ok()?;
    let (conn, screen_num) = connect().ok()?;
    let root = conn.setup().roots[screen_num].root;
    let net_client_list = intern_atom(&conn, b"_NET_CLIENT_LIST").ok()?;
    let net_active = intern_atom(&conn, b"_NET_ACTIVE_WINDOW").ok()?;
    let net_wm_name = intern_atom(&conn, b"_NET_WM_NAME").ok()?;
    let net_wm_pid = intern_atom(&conn, b"_NET_WM_PID").ok()?;
    let clients = candidate_windows(&conn, root, net_client_list);
    let focused = resolve_active_window(&conn, root, net_active, &clients);
    build_window_info(&conn, root, win, focused, net_wm_name, net_wm_pid)
}

/// Information for the currently active/focused X11 window.
pub fn active_window_info() -> Result<Option<WindowInfo>, DesktopError> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let net_client_list = intern_atom(&conn, b"_NET_CLIENT_LIST")?;
    let net_active = intern_atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let net_wm_name = intern_atom(&conn, b"_NET_WM_NAME")?;
    let net_wm_pid = intern_atom(&conn, b"_NET_WM_PID")?;
    let clients = candidate_windows(&conn, root, net_client_list);
    let Some(active) = resolve_active_window(&conn, root, net_active, &clients) else {
        return Ok(None);
    };
    Ok(build_window_info(
        &conn,
        root,
        active,
        Some(active),
        net_wm_name,
        net_wm_pid,
    ))
}

/// Enumerate all visible top-level X11 windows.
pub fn list_windows() -> Result<Vec<WindowInfo>, DesktopError> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let net_client_list = intern_atom(&conn, b"_NET_CLIENT_LIST")?;
    let net_active = intern_atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let net_wm_name = intern_atom(&conn, b"_NET_WM_NAME")?;
    let net_wm_pid = intern_atom(&conn, b"_NET_WM_PID")?;

    let clients = candidate_windows(&conn, root, net_client_list);
    let focused = resolve_active_window(&conn, root, net_active, &clients);

    let mut out = Vec::new();
    for win in clients {
        let viewable = conn
            .get_window_attributes(win)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.map_state == MapState::VIEWABLE);
        if !viewable && focused != Some(win) {
            continue;
        }
        if let Some(info) = build_window_info(&conn, root, win, focused, net_wm_name, net_wm_pid)
            && (!info.title.is_empty() || info.focused)
        {
            out.push(info);
        }
    }
    Ok(out)
}

/// Bring `window_id` to the foreground and focus it.
pub fn activate_window(window_id: u64) -> Result<(), DesktopError> {
    let win = u32::try_from(window_id)
        .map_err(|_| DesktopError::Accessibility("invalid X11 window id".into()))?;
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let net_active = intern_atom(&conn, b"_NET_ACTIVE_WINDOW")?;

    let ev = ClientMessageEvent::new(32, win, net_active, [2, CURRENT_TIME, 0, 0, 0]);
    let _ = conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    );
    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    let _ = conn.set_input_focus(InputFocus::PARENT, win, CURRENT_TIME);
    conn.sync().map_err(x11_err("activate_window sync"))?;
    Ok(())
}

/// Launch an application process on the current X11 session.
pub fn launch(app: &AppSpec) -> Result<Pid, DesktopError> {
    let dpy = ensure_x11_session()?;
    let child = Command::new(&app.executable)
        .args(&app.args)
        .env("DISPLAY", &dpy)
        .env("GTK_MODULES", "gail:atk-bridge")
        .env("QT_ACCESSIBILITY", "1")
        .env("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| DesktopError::Internal(format!("launch '{}': {e}", app.executable)))?;
    Ok(child.id())
}

// ── XTest input injection ─────────────────────────────────────────────────────

fn move_pointer(conn: &RustConnection, x: f64, y: f64) -> Result<(), DesktopError> {
    #[allow(clippy::cast_possible_truncation)]
    let rx = x.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    #[allow(clippy::cast_possible_truncation)]
    let ry = y.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, CURRENT_TIME, NONE, rx, ry, 0)
        .map_err(x11_err("xtest motion"))?;
    conn.sync().map_err(x11_err("xtest motion sync"))
}

fn button_event(
    conn: &RustConnection,
    root: Window,
    button: u8,
    press: bool,
) -> Result<(), DesktopError> {
    let ev = if press {
        BUTTON_PRESS_EVENT
    } else {
        BUTTON_RELEASE_EVENT
    };
    conn.xtest_fake_input(ev, button, CURRENT_TIME, root, 0, 0, 0)
        .map_err(x11_err("xtest button"))?;
    conn.sync().map_err(x11_err("xtest button sync"))
}

/// Move the pointer to `(x, y)` and left-click via `XTest`.
pub fn click_at(x: f64, y: f64) -> Result<(), DesktopError> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    move_pointer(&conn, x, y)?;
    button_event(&conn, root, 1, true)?;
    button_event(&conn, root, 1, false)
}

/// Drag from `from` to `to` via `XTest`.
pub fn drag(from: (f64, f64), to: (f64, f64)) -> Result<(), DesktopError> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    move_pointer(&conn, from.0, from.1)?;
    button_event(&conn, root, 1, true)?;
    thread::sleep(Duration::from_millis(40));
    move_pointer(&conn, to.0, to.1)?;
    thread::sleep(Duration::from_millis(40));
    button_event(&conn, root, 1, false)
}

/// Scroll at `(x, y)` using X11 scroll buttons (`4`/`5` vertical, `6`/`7` horizontal).
pub fn scroll_at(x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<(), DesktopError> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    move_pointer(&conn, x, y)?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v_steps = delta_y.abs().ceil().clamp(0.0, 50.0) as u32;
    let v_btn = if delta_y > 0.0 { 5 } else { 4 };
    for _ in 0..v_steps {
        button_event(&conn, root, v_btn, true)?;
        button_event(&conn, root, v_btn, false)?;
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let h_steps = delta_x.abs().ceil().clamp(0.0, 50.0) as u32;
    let h_btn = if delta_x > 0.0 { 7 } else { 6 };
    for _ in 0..h_steps {
        button_event(&conn, root, h_btn, true)?;
        button_event(&conn, root, h_btn, false)?;
    }
    Ok(())
}

/// Look up `(keycode, temporary_binding)` for `keysym` in the server's keymap.
fn resolve_keycode(conn: &RustConnection, keysym: u32) -> Result<(u8, bool), DesktopError> {
    let setup = conn.setup();
    let min_kc = setup.min_keycode;
    let max_kc = setup.max_keycode;
    let count = max_kc.saturating_sub(min_kc).saturating_add(1);
    let GetKeyboardMappingReply {
        keysyms_per_keycode,
        keysyms,
        ..
    } = conn
        .get_keyboard_mapping(min_kc, count)
        .map_err(x11_err("get_keyboard_mapping"))?
        .reply()
        .map_err(x11_err("get_keyboard_mapping reply"))?;

    let per = usize::from(keysyms_per_keycode.max(1));
    let mut unused_kc: Option<u8> = None;

    for (idx, chunk) in keysyms.chunks(per).enumerate() {
        let Ok(offset) = u8::try_from(idx) else {
            break;
        };
        let kc = min_kc.saturating_add(offset);
        if chunk.first().copied() == Some(keysym) {
            return Ok((kc, false));
        }
        if unused_kc.is_none() && kc != 8 && chunk.iter().all(|&s| s == 0) {
            unused_kc = Some(kc);
        }
    }

    let scratch = unused_kc.unwrap_or(max_kc);
    conn.change_keyboard_mapping(1, scratch, 2, &[keysym, keysym])
        .map_err(x11_err("change_keyboard_mapping"))?;
    conn.sync()
        .map_err(x11_err("change_keyboard_mapping sync"))?;
    Ok((scratch, true))
}

fn unbind_keycode(conn: &RustConnection, kc: u8) {
    let _ = conn.change_keyboard_mapping(1, kc, 2, &[0, 0]);
    let _ = conn.sync();
}

fn key_event(conn: &RustConnection, root: Window, kc: u8, press: bool) -> Result<(), DesktopError> {
    let ev = if press {
        KEY_PRESS_EVENT
    } else {
        KEY_RELEASE_EVENT
    };
    conn.xtest_fake_input(ev, kc, CURRENT_TIME, root, 0, 0, 0)
        .map_err(x11_err("xtest key"))?;
    conn.sync().map_err(x11_err("xtest key sync"))
}

/// Type arbitrary UTF-8 text via `XTest` keysym injection.
pub fn type_text(text: &str) -> Result<(), DesktopError> {
    if text.is_empty() {
        return Ok(());
    }
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    for ch in text.chars() {
        let sym = char_to_keysym(ch);
        let (kc, temp) = resolve_keycode(&conn, sym)?;
        let res = key_event(&conn, root, kc, true).and_then(|()| key_event(&conn, root, kc, false));
        if temp {
            unbind_keycode(&conn, kc);
        }
        res?;
    }
    Ok(())
}

/// Press a key chord: all keys down in order, then released in reverse order.
pub fn key_chord(keys: &[String]) -> Result<(), DesktopError> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut syms = Vec::with_capacity(keys.len());
    for k in keys {
        let parsed =
            parse_key(k).ok_or_else(|| DesktopError::Unsupported(format!("unknown key '{k}'")))?;
        let sym = match parsed {
            Key::Keysym(s) => s,
            Key::Char(c) => char_to_keysym(c),
        };
        syms.push(sym);
    }

    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let mut resolved: Vec<(u8, bool)> = Vec::with_capacity(syms.len());

    for sym in syms {
        let pair = resolve_keycode(&conn, sym)?;
        resolved.push(pair);
    }

    let mut result = Ok(());
    for &(kc, _) in &resolved {
        if let Err(e) = key_event(&conn, root, kc, true) {
            result = Err(e);
            break;
        }
    }
    for &(kc, _) in resolved.iter().rev() {
        let _ = key_event(&conn, root, kc, false);
    }
    for (kc, temp) in resolved {
        if temp {
            unbind_keycode(&conn, kc);
        }
    }
    result
}

// ── XGetImage screenshot capture ──────────────────────────────────────────────

fn jpeg_quality(q: Quality) -> u8 {
    match q {
        Quality::Low => 40,
        Quality::Medium => 70,
        Quality::High | Quality::Lossless => 90,
    }
}

/// Capture `region` via X11 `GetImage` (`XGetImage`) and encode as JPEG or PNG.
pub fn screenshot(region: &Region, quality: Quality) -> Result<Image, DesktopError> {
    let (conn, screen_num) = connect()?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let screen_w = u32::from(screen.width_in_pixels);
    let screen_h = u32::from(screen.height_in_pixels);

    let target_rect = match region {
        Region::FullScreen => Rect {
            x: 0.0,
            y: 0.0,
            width: f64::from(screen_w),
            height: f64::from(screen_h),
        },
        Region::Window { window_id } => {
            let win = u32::try_from(*window_id)
                .map_err(|_| DesktopError::Screenshot("window not found".into()))?;
            window_bounds_on_conn(&conn, root, win)
                .ok_or_else(|| DesktopError::Screenshot("window not found".into()))?
        }
        Region::Rect(r) => *r,
    };

    let (left, top, width, height) = clamp_capture_rect(&target_rect, screen_w, screen_h)?;
    let reply = conn
        .get_image(ImageFormat::Z_PIXMAP, root, left, top, width, height, !0)
        .map_err(shot_err("get_image"))?
        .reply()
        .map_err(shot_err("get_image reply"))?;

    let wu = u32::from(width);
    let hu = u32::from(height);
    let mut bgra = zpixmap_to_bgra(&reply.data, wu, hu, reply.depth)?;

    let mut out = Vec::new();
    let mime_type = if quality == Quality::Lossless {
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        let mut enc = png::Encoder::new(&mut out, wu, hu);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(shot_err("png header"))?;
        writer
            .write_image_data(&bgra)
            .map_err(shot_err("png data"))?;
        writer.finish().map_err(shot_err("png finish"))?;
        "image/png"
    } else {
        let enc = jpeg_encoder::Encoder::new(&mut out, jpeg_quality(quality));
        enc.encode(&bgra, width, height, jpeg_encoder::ColorType::Bgra)
            .map_err(shot_err("jpeg encode"))?;
        "image/jpeg"
    };

    Ok(Image {
        data: out,
        width: wu,
        height: hu,
        mime_type: mime_type.to_string(),
    })
}

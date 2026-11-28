//! X11 helpers over `x11rb`: window enumeration/activation, `XTest` input
//! injection, `GetImage` (`XGetImage`) screen capture, and process launch.

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use desktop_api::{
    ActGuard, DesktopError, InputTarget, WindowOp,
    snapshot::{AppSpec, Image, Pid, Quality, Rect, Region, WindowInfo},
};
use x11rb::{
    CURRENT_TIME, NONE,
    connection::Connection as _,
    protocol::{
        Event,
        xproto::{
            Atom, AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ClientMessageEvent,
            ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask,
            GetKeyboardMappingReply, ImageFormat, InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
            MOTION_NOTIFY_EVENT, MapState, PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent,
            StackMode, Window, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

use crate::helper::{LaunchFail, explain_launch};
use crate::pure::{
    Key, KeyPlace, TypeAction, char_to_keysym, clamp_capture_rect, detect_display_session,
    find_keysym, free_keycodes, parse_key, parse_wm_class, plan_text_actions, x_unreachable,
    zpixmap_to_bgra,
};

fn display_name() -> String {
    std::env::var("DISPLAY").unwrap_or_else(|_| "(unset)".into())
}

fn x11_err<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> DesktopError + '_ {
    move |e| DesktopError::Accessibility(format!("{ctx} on display {}: {e}", display_name().trim()))
}

fn shot_err<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> DesktopError + '_ {
    move |e| DesktopError::Screenshot(format!("{ctx} on display {}: {e}", display_name().trim()))
}

/// Ensure we are on an X11 session (rejecting Wayland with [`DesktopError::Unsupported`]).
pub fn ensure_x11_session() -> Result<String, DesktopError> {
    let xdg = std::env::var("XDG_SESSION_TYPE").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let x11 = std::env::var("DISPLAY").ok();
    detect_display_session(xdg.as_deref(), wayland.as_deref(), x11.as_deref())
}

/// How long one X11 handshake may block the probe thread.
const X11_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Cached, authenticated startup reachability. Capability queries are synchronous
/// and run inside async tool calls, so the handshake runs on its own thread.
/// A failure is retried after [`crate::pure::PROBE_RETRY_AFTER`] instead of being
/// pinned for the process lifetime (a slow start used to force every later
/// desktop call to report "no display").
#[must_use]
pub fn has_x11_display() -> bool {
    static CACHE: std::sync::Mutex<crate::pure::ProbeCache> =
        std::sync::Mutex::new(crate::pure::ProbeCache::new());
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.get(std::time::Instant::now(), || {
        ensure_x11_session().is_ok()
            && probe_thread(Duration::from_secs(1), || x11rb::connect(None).is_ok())
    })
}

fn probe_thread(timeout: Duration, probe: impl FnOnce() -> bool + Send + 'static) -> bool {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if thread::Builder::new()
        .name("desktop-display-probe".into())
        .spawn(move || {
            let _ = sender.send(probe());
        })
        .is_err()
    {
        return false;
    }
    receiver.recv_timeout(timeout).unwrap_or(false)
}

/// Handshake with `display`, bounded so a dead socket cannot hang the caller.
///
/// # Errors
///
/// [`DesktopError::Accessibility`] naming the display when the session is wrong
/// or the server does not answer in time.
pub fn probe_x_server(timeout: Duration) -> Result<(), String> {
    let dpy = ensure_x11_session().map_err(|e| e.to_string())?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let name = dpy.clone();
    if thread::Builder::new()
        .name("x11-probe".into())
        .spawn(move || {
            let res = x11rb::connect(Some(&name))
                .map(|_| ())
                .map_err(|e| x_unreachable(&name, &e.to_string()));
            let _ = sender.send(res);
        })
        .is_err()
    {
        return Err(x_unreachable(&dpy, "could not start the X probe thread"));
    }
    match receiver.recv_timeout(timeout) {
        Ok(res) => res,
        Err(_) => Err(x_unreachable(
            &dpy,
            &format!("timed out after {}s", timeout.as_secs().max(1)),
        )),
    }
}

fn connect() -> Result<(RustConnection, usize), DesktopError> {
    let dpy = ensure_x11_session()?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let name = dpy.clone();
    if thread::Builder::new()
        .name("x11-connect".into())
        .spawn(move || {
            let res = x11rb::connect(Some(&name)).map_err(|e| x_unreachable(&name, &e.to_string()));
            let _ = sender.send(res);
        })
        .is_err()
    {
        return Err(DesktopError::Accessibility(x_unreachable(
            &dpy,
            "could not start the X connect thread",
        )));
    }
    match receiver.recv_timeout(X11_CONNECT_TIMEOUT) {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(msg)) => Err(DesktopError::Accessibility(msg)),
        Err(_) => {
            // Spawn may still complete after the deadline. Dropping the receiver
            // makes that send fail; the thread then drops the connection.
            Err(DesktopError::Accessibility(x_unreachable(
                &dpy,
                "timed out waiting for the X server",
            )))
        }
    }
}

/// Root window size in pixels. Used when AT-SPI has no frame and X has no
/// focused window, so coordinates are not invented as 1920×1080.
pub fn screen_size() -> Result<(u32, u32), DesktopError> {
    let (conn, screen_num) = connect()?;
    let screen =
        conn.setup().roots.get(screen_num).ok_or_else(|| {
            DesktopError::Accessibility(x_unreachable(&display_name(), "no screen"))
        })?;
    Ok((
        u32::from(screen.width_in_pixels),
        u32::from(screen.height_in_pixels),
    ))
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

/// The top-level window under the screen point `(x, y)`, numbered like [`list_windows`].
///
/// The root's children are walked from the top of the stack down: the first viewable one that
/// contains the point and takes input there wins (a click-through window, such as the bot-cursor
/// overlay, is skipped: the click goes through it). When a window manager frames clients, the
/// client inside the frame is the answer, so the id matches the window list.
pub fn window_at(x: f64, y: f64) -> Result<Option<u64>, DesktopError> {
    use x11rb::connection::RequestConnection as _;
    use x11rb::protocol::shape::{ConnectionExt as _, SK};
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let tree = conn
        .query_tree(root)
        .map_err(x11_err("query_tree"))?
        .reply()
        .map_err(x11_err("query_tree reply"))?;
    let (px, py) = (x.round(), y.round());
    let has_shape = conn
        .extension_information(x11rb::protocol::shape::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();
    let mut top = None;
    for win in tree.children.iter().rev() {
        let Some(attrs) = conn
            .get_window_attributes(*win)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            continue;
        };
        if attrs.map_state != MapState::VIEWABLE {
            continue;
        }
        let Some(geo) = conn.get_geometry(*win).ok().and_then(|c| c.reply().ok()) else {
            continue;
        };
        let (gx, gy) = (f64::from(geo.x), f64::from(geo.y));
        if px < gx || py < gy || px >= gx + f64::from(geo.width) || py >= gy + f64::from(geo.height)
        {
            continue;
        }
        if has_shape
            && conn
                .shape_get_rectangles(*win, SK::INPUT)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|r| r.rectangles.is_empty())
        {
            continue;
        }
        top = Some(*win);
        break;
    }
    let Some(top) = top else { return Ok(None) };
    let net_client_list = intern_atom(&conn, b"_NET_CLIENT_LIST")?;
    let clients = candidate_windows(&conn, root, net_client_list);
    if clients.contains(&top) {
        return Ok(Some(u64::from(top)));
    }
    // A frame: look for the client inside it (a few levels deep).
    let mut level = vec![top];
    for _ in 0..3 {
        let mut next = Vec::new();
        for w in level {
            let Some(t) = conn.query_tree(w).ok().and_then(|c| c.reply().ok()) else {
                continue;
            };
            for child in t.children {
                if clients.contains(&child) {
                    return Ok(Some(u64::from(child)));
                }
                next.push(child);
            }
        }
        level = next;
    }
    Ok(Some(u64::from(top)))
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

// ── Window management ─────────────────────────────────────────────────────────

/// `_NET_WM_STATE` actions of the EWMH spec.
const NET_WM_STATE_REMOVE: u32 = 0;
const NET_WM_STATE_ADD: u32 = 1;
/// ICCCM `IconicState`, the argument of a `WM_CHANGE_STATE` request.
const ICONIC_STATE: u32 = 3;

fn send_to_root(
    conn: &RustConnection,
    root: Window,
    win: Window,
    kind: Atom,
    data: [u32; 5],
) -> Result<(), DesktopError> {
    let ev = ClientMessageEvent::new(32, win, kind, data);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    )
    .map_err(x11_err("send_event"))?;
    Ok(())
}

/// The program and kind of a top-level window, read now: who owns it (image, pid
/// and start time from `/proc`), whether that program runs outside the user's own
/// account, the window class, and whether it is a dialog (transient for another
/// window, or of the dialog window type). `None` when the window is gone or its
/// program cannot be identified.
pub fn top_level_facts(window_id: u64) -> Option<InputTarget> {
    let win = u32::try_from(window_id).ok()?;
    let info = window_info(window_id)?;
    let identity = crate::identity::process_identity(info.pid)?;
    let (conn, _) = connect().ok()?;
    let class = conn
        .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
        .ok()?
        .reply()
        .ok()
        .map(|r| parse_wm_class(&r.value))
        .unwrap_or_default();
    let transient = read_u32_prop(&conn, win, AtomEnum::WM_TRANSIENT_FOR.into())
        .is_some_and(|t| t != 0 && t != win);
    let dialog = intern_atom(&conn, b"_NET_WM_WINDOW_TYPE")
        .ok()
        .zip(intern_atom(&conn, b"_NET_WM_WINDOW_TYPE_DIALOG").ok())
        .and_then(|(kind, dialog)| {
            read_u32_list_prop(&conn, win, kind).map(|types| types.contains(&dialog))
        })
        .unwrap_or(false);
    Some(InputTarget {
        window_id,
        title: info.title,
        class,
        elevated: crate::identity::process_elevated(info.pid),
        identity,
        password_focus: false,
        owned: transient || dialog,
    })
}

/// Minimise, maximise, restore, move, resize or ask to close a window. The window
/// manager is asked the standard (EWMH / ICCCM) way; a program that does not
/// support a graceful close is never killed.
///
/// Right before the request the window is judged again: with a `guard` it must
/// still belong to the program the approval was shown for, and in any case it
/// must still pass [`desktop_api::guard::window_op_refusal`]. A mismatch is a
/// refusal and nothing is sent.
pub fn window_op(
    window_id: u64,
    op: WindowOp,
    guard: Option<&ActGuard>,
) -> Result<(), DesktopError> {
    let win = u32::try_from(window_id)
        .map_err(|_| DesktopError::Accessibility("invalid X11 window id".into()))?;
    if window_info(window_id).is_none() {
        return Err(DesktopError::Accessibility(
            "window no longer exists".into(),
        ));
    }
    desktop_api::guard::check_window_op(guard, top_level_facts(window_id).as_ref(), op)
        .map_err(|r| DesktopError::GuardRefused(format!("not sent: {}", r.why())))?;
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let maximise = |add: bool| -> Result<(), DesktopError> {
        let state = intern_atom(&conn, b"_NET_WM_STATE")?;
        let vert = intern_atom(&conn, b"_NET_WM_STATE_MAXIMIZED_VERT")?;
        let horz = intern_atom(&conn, b"_NET_WM_STATE_MAXIMIZED_HORZ")?;
        let action = if add {
            NET_WM_STATE_ADD
        } else {
            NET_WM_STATE_REMOVE
        };
        send_to_root(&conn, root, win, state, [action, vert, horz, 1, 0])
    };
    match op {
        WindowOp::Minimize => {
            let change = intern_atom(&conn, b"WM_CHANGE_STATE")?;
            send_to_root(&conn, root, win, change, [ICONIC_STATE, 0, 0, 0, 0])?;
        }
        WindowOp::Maximize => maximise(true)?,
        WindowOp::Restore => {
            maximise(false)?;
            // A minimised window is mapped again by an activation request.
            let net_active = intern_atom(&conn, b"_NET_ACTIVE_WINDOW")?;
            send_to_root(&conn, root, win, net_active, [1, CURRENT_TIME, 0, 0, 0])?;
        }
        WindowOp::Move { x, y } => {
            maximise(false)?;
            conn.configure_window(win, &ConfigureWindowAux::new().x(x).y(y))
                .map_err(x11_err("configure_window"))?;
        }
        WindowOp::Resize { width, height } => {
            maximise(false)?;
            conn.configure_window(win, &ConfigureWindowAux::new().width(width).height(height))
                .map_err(x11_err("configure_window"))?;
        }
        WindowOp::Close => {
            let protocols = intern_atom(&conn, b"WM_PROTOCOLS")?;
            let delete = intern_atom(&conn, b"WM_DELETE_WINDOW")?;
            let supports_delete = read_u32_list_prop(&conn, win, protocols)
                .is_some_and(|atoms| atoms.contains(&delete));
            if supports_delete {
                // The program's own close request, the one its close button sends.
                let ev =
                    ClientMessageEvent::new(32, win, protocols, [delete, CURRENT_TIME, 0, 0, 0]);
                conn.send_event(false, win, EventMask::NO_EVENT, ev)
                    .map_err(x11_err("send_event"))?;
            } else {
                let close = intern_atom(&conn, b"_NET_CLOSE_WINDOW")?;
                send_to_root(&conn, root, win, close, [CURRENT_TIME, 1, 0, 0, 0])?;
            }
        }
    }
    conn.sync().map_err(x11_err("window_op sync"))?;
    Ok(())
}

// ── Clipboard (the CLIPBOARD selection) ───────────────────────────────────────

/// Longest clipboard text read, in bytes. Larger content (an INCR transfer) is refused.
const MAX_CLIPBOARD_BYTES: u32 = 4 * 1024 * 1024;
/// How long a clipboard owner may take to answer a request.
const CLIPBOARD_TIMEOUT: Duration = Duration::from_millis(1500);
const CLIPBOARD_POLL: Duration = Duration::from_millis(10);

fn hidden_window(conn: &RustConnection, screen_num: usize) -> Result<Window, DesktopError> {
    let screen = &conn.setup().roots[screen_num];
    let win = conn.generate_id().map_err(x11_err("generate_id"))?;
    conn.create_window(
        0,
        win,
        screen.root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(x11_err("create_window"))?;
    Ok(win)
}

/// The text of the CLIPBOARD selection; `None` when it holds no text.
pub fn clipboard_text() -> Result<Option<String>, DesktopError> {
    let (conn, screen_num) = connect()?;
    let win = hidden_window(&conn, screen_num)?;
    let clipboard = intern_atom(&conn, b"CLIPBOARD")?;
    let utf8 = intern_atom(&conn, b"UTF8_STRING")?;
    let prop = intern_atom(&conn, b"PRAXI_CLIPBOARD")?;
    let incr = intern_atom(&conn, b"INCR")?;
    conn.convert_selection(win, clipboard, utf8, prop, CURRENT_TIME)
        .map_err(x11_err("convert_selection"))?;
    conn.flush().map_err(x11_err("flush"))?;
    let deadline = std::time::Instant::now() + CLIPBOARD_TIMEOUT;
    let result = loop {
        match conn.poll_for_event().map_err(x11_err("poll_for_event"))? {
            Some(Event::SelectionNotify(ev)) => {
                break if ev.property == NONE {
                    Ok(None)
                } else {
                    read_clipboard_property(&conn, win, ev.property, incr)
                };
            }
            Some(_) => {}
            None if std::time::Instant::now() >= deadline => {
                break Err(DesktopError::Accessibility(
                    "the program that owns the clipboard did not answer".into(),
                ));
            }
            None => thread::sleep(CLIPBOARD_POLL),
        }
    };
    let _ = conn.destroy_window(win);
    let _ = conn.flush();
    result
}

fn read_clipboard_property(
    conn: &RustConnection,
    win: Window,
    prop: Atom,
    incr: Atom,
) -> Result<Option<String>, DesktopError> {
    let reply = conn
        .get_property(true, win, prop, AtomEnum::ANY, 0, MAX_CLIPBOARD_BYTES / 4)
        .map_err(x11_err("get_property"))?
        .reply()
        .map_err(x11_err("get_property reply"))?;
    if reply.type_ == incr {
        return Err(DesktopError::Accessibility(
            "the clipboard text is too large to read".into(),
        ));
    }
    if reply.value.is_empty() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&reply.value).into_owned()))
}

/// Take the CLIPBOARD selection and serve `text` from a background thread until
/// another program takes the selection over (the way every X11 program offers it).
pub fn set_clipboard_text(text: &str) -> Result<(), DesktopError> {
    let (conn, screen_num) = connect()?;
    let win = hidden_window(&conn, screen_num)?;
    let clipboard = intern_atom(&conn, b"CLIPBOARD")?;
    let utf8 = intern_atom(&conn, b"UTF8_STRING")?;
    let targets = intern_atom(&conn, b"TARGETS")?;
    let text_atom = intern_atom(&conn, b"TEXT")?;
    conn.set_selection_owner(win, clipboard, CURRENT_TIME)
        .map_err(x11_err("set_selection_owner"))?;
    let owner = conn
        .get_selection_owner(clipboard)
        .map_err(x11_err("get_selection_owner"))?
        .reply()
        .map_err(x11_err("get_selection_owner reply"))?
        .owner;
    if owner != win {
        return Err(DesktopError::Accessibility(
            "could not take the clipboard".into(),
        ));
    }
    let text = text.to_owned();
    thread::Builder::new()
        .name("x11-clipboard".into())
        .spawn(move || {
            serve_clipboard(&conn, &text, clipboard, utf8, targets, text_atom);
        })
        .map_err(|e| DesktopError::Internal(format!("clipboard thread: {e}")))?;
    Ok(())
}

fn serve_clipboard(
    conn: &RustConnection,
    text: &str,
    clipboard: Atom,
    utf8: Atom,
    targets: Atom,
    text_atom: Atom,
) {
    let string: Atom = AtomEnum::STRING.into();
    loop {
        let Ok(event) = conn.wait_for_event() else {
            return;
        };
        match event {
            // Someone else owns the selection now: this copy is no longer wanted.
            Event::SelectionClear(_) => return,
            Event::SelectionRequest(req) => {
                let mut property = req.property;
                if req.selection != clipboard {
                    property = NONE;
                } else if req.target == targets {
                    let list = [targets, utf8, string, text_atom];
                    let _ = conn.change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        req.property,
                        AtomEnum::ATOM,
                        &list,
                    );
                } else if req.target == utf8 || req.target == string || req.target == text_atom {
                    let _ = conn.change_property8(
                        PropMode::REPLACE,
                        req.requestor,
                        req.property,
                        req.target,
                        text.as_bytes(),
                    );
                } else {
                    property = NONE;
                }
                let notify = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: req.time,
                    requestor: req.requestor,
                    selection: req.selection,
                    target: req.target,
                    property,
                };
                let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
                let _ = conn.flush();
            }
            _ => {}
        }
    }
}

/// How long `Command::spawn` may block. A stuck filesystem must not hang the tool.
const LAUNCH_SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Launch an application process on the current X11 session.
pub fn launch(app: &AppSpec) -> Result<Pid, DesktopError> {
    let dpy = ensure_x11_session()?;
    let exe = app.executable.clone();
    let args = app.args.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let started = thread::Builder::new()
        .name("desktop-launch".into())
        .spawn(move || {
            let mut command = Command::new(&exe);
            // Its own process group: a signal meant for the daemon or the tool call that started
            // it (stop, timeout) must not take the window the bot opened down with it.
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
            match command
                .args(&args)
                .env("DISPLAY", &dpy)
                .env("GTK_MODULES", "gail:atk-bridge")
                .env("QT_ACCESSIBILITY", "1")
                .env("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let pid = child.id();
                    if sender.send(Ok(pid)).is_ok() {
                        // Caller accepted the pid. Reap on exit so it is not a zombie.
                        let _ = child.wait();
                    } else {
                        // Caller already gave up. Kill and reap.
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                Err(err) => {
                    let _ = sender.send(Err(err));
                }
            }
        });
    if started.is_err() {
        return Err(DesktopError::Internal(explain_launch(
            &app.executable,
            LaunchFail::Other("could not start the launch thread"),
        )));
    }
    match receiver.recv_timeout(LAUNCH_SPAWN_TIMEOUT) {
        Ok(Ok(pid)) => Ok(pid),
        Ok(Err(err)) => Err(launch_io_error(&app.executable, &err)),
        Err(_) => match receiver.try_recv() {
            Ok(Ok(pid)) => Ok(pid),
            Ok(Err(err)) => Err(launch_io_error(&app.executable, &err)),
            Err(_) => Err(DesktopError::Internal(explain_launch(
                &app.executable,
                LaunchFail::TimedOut {
                    secs: LAUNCH_SPAWN_TIMEOUT.as_secs().max(1),
                },
            ))),
        },
    }
}

fn launch_io_error(program: &str, err: &std::io::Error) -> DesktopError {
    if err.kind() == std::io::ErrorKind::NotFound {
        DesktopError::Internal(explain_launch(program, LaunchFail::NotFound))
    } else {
        let text = err.to_string();
        DesktopError::Internal(explain_launch(program, LaunchFail::Other(&text)))
    }
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
    let moved = move_pointer(&conn, to.0, to.1);
    thread::sleep(Duration::from_millis(40));
    // Always try to release the button, even if the move failed.
    let up = button_event(&conn, root, 1, false);
    moved.and(up)
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

struct Keymap {
    min_kc: u8,
    max_kc: u8,
    per: u8,
    keysyms: Vec<u32>,
}

fn load_keymap(conn: &RustConnection) -> Result<Keymap, DesktopError> {
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
    Ok(Keymap {
        min_kc,
        max_kc,
        per: keysyms_per_keycode.max(1),
        keysyms,
    })
}

/// Look up `(keycode, temporary_binding)` for a chord keysym.
///
/// Level-0 hits are used as-is. Anything else (shifted symbols, characters
/// absent from the layout) is bound onto a scratch keycode so the chord does
/// not depend on Shift already being down.
fn resolve_keycode(conn: &RustConnection, keysym: u32) -> Result<(u8, bool), DesktopError> {
    let map = load_keymap(conn)?;
    match find_keysym(keysym, map.min_kc, map.max_kc, map.per, &map.keysyms) {
        KeyPlace::Level0 { keycode } => Ok((keycode, false)),
        KeyPlace::Level1 { .. } | KeyPlace::Bind { .. } => {
            let free = free_keycodes(map.min_kc, map.max_kc, map.per, &map.keysyms, 1);
            let scratch = free.first().copied().unwrap_or(map.max_kc);
            conn.change_keyboard_mapping(1, scratch, 2, &[keysym, keysym])
                .map_err(x11_err("change_keyboard_mapping"))?;
            conn.sync()
                .map_err(x11_err("change_keyboard_mapping sync"))?;
            Ok((scratch, true))
        }
    }
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
///
/// Characters on the current layout are pressed directly (with Shift when they
/// sit on level 1). Characters outside the layout, including Turkish letters
/// such as ğ ş ı İ, are bound onto free keycodes for the whole string and
/// cleared only after the last key-up so a client cannot observe `NoSymbol`.
pub fn type_text(text: &str) -> Result<(), DesktopError> {
    if text.is_empty() {
        return Ok(());
    }
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let map = load_keymap(&conn)?;
    let steps: Vec<KeyPlace> = text
        .chars()
        .map(|ch| {
            find_keysym(
                char_to_keysym(ch),
                map.min_kc,
                map.max_kc,
                map.per,
                &map.keysyms,
            )
        })
        .collect();
    let distinct_binds = {
        let mut syms = Vec::new();
        for step in &steps {
            if let KeyPlace::Bind { keysym } = *step
                && !syms.contains(&keysym)
            {
                syms.push(keysym);
            }
        }
        syms.len()
    };
    let free = free_keycodes(
        map.min_kc,
        map.max_kc,
        map.per,
        &map.keysyms,
        distinct_binds,
    );
    let shift = match find_keysym(0xFFE1, map.min_kc, map.max_kc, map.per, &map.keysyms) {
        KeyPlace::Level0 { keycode } => Some(keycode),
        _ => None,
    };
    let actions = plan_text_actions(&steps, shift, &free).map_err(DesktopError::Accessibility)?;

    let mut result = Ok(());
    let mut unbound = false;
    let mut synced_binds = false;
    for action in actions {
        match action {
            TypeAction::Bind { keycode, keysym } => {
                if result.is_err() {
                    continue;
                }
                if let Err(err) = conn
                    .change_keyboard_mapping(1, keycode, 2, &[keysym, keysym])
                    .map_err(x11_err("change_keyboard_mapping"))
                {
                    result = Err(err);
                }
            }
            TypeAction::Down { keycode } | TypeAction::Up { keycode } => {
                if result.is_err() {
                    continue;
                }
                if !synced_binds {
                    if let Err(err) = conn.sync().map_err(x11_err("change_keyboard_mapping sync")) {
                        result = Err(err);
                        continue;
                    }
                    synced_binds = true;
                }
                let press = matches!(action, TypeAction::Down { .. });
                if let Err(err) = key_event(&conn, root, keycode, press) {
                    result = Err(err);
                }
            }
            TypeAction::Unbind { keycode } => {
                if !unbound {
                    thread::sleep(Duration::from_millis(30));
                    unbound = true;
                }
                unbind_keycode(&conn, keycode);
            }
        }
    }
    result
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
    if resolved.iter().any(|(_, temp)| *temp) {
        // Leave the scratch keysym in place until the client can read the event.
        thread::sleep(Duration::from_millis(30));
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

#[cfg(test)]
mod display_tests {
    use super::*;

    #[test]
    fn capability_probe_times_out_without_blocking_the_caller() {
        let started = std::time::Instant::now();
        assert!(!probe_thread(Duration::from_millis(20), || {
            thread::sleep(Duration::from_secs(5));
            true
        }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

//! The bot cursor as an X11 window.
//!
//! One override-redirect window (the window manager never sees it, so it never takes focus and
//! is never decorated), kept above the others. It is click-through: its `Shape` *input* region is
//! empty, so every click goes to the window below. Its look comes from the `Shape` *bounding*
//! region (the arrow and the ripple ring cut out of a square), which works on a bare server
//! with no compositor; the colours are a pixmap behind it. The pixels are drawn by
//! [`desktop_api::cursor::sprite`]; the thread and the ripple timing by
//! [`desktop_api::cursor::ThreadedSurface`].

use std::sync::Arc;

use desktop_api::cursor::sprite::{Bitmap, mask_rects, opaque_bgrx};
use desktop_api::cursor::{CursorSurface, OverlayWindow, ThreadedSurface};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
    connection::{Connection as _, RequestConnection as _},
    protocol::{
        shape::{self, ConnectionExt as _, SK, SO},
        xproto::{
            ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
            ImageFormat, ImageOrder, Rectangle, StackMode, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

/// Pixels with at least this alpha are part of the window's shape.
const SHAPE_ALPHA: u8 = 110;

/// A cursor surface on the display `$DISPLAY`.
///
/// # Errors
/// No X server, or no `Shape` extension (without it the window could not be click-through, so
/// none is made).
pub fn open() -> Result<Arc<dyn CursorSurface>, String> {
    let surface = ThreadedSurface::spawn("x11-cursor", 1.0, || {
        Ok(Box::new(X11Overlay::new()?) as Box<dyn OverlayWindow>)
    })?;
    Ok(Arc::new(surface))
}

struct X11Overlay {
    conn: RustConnection,
    root: Window,
    win: Window,
    depth: u8,
    /// The server takes 32-bit pixels at the root depth, so the colours can be a pixmap.
    pixmap_ok: bool,
    msb_first: bool,
    visible: bool,
    last_origin: Option<(i32, i32)>,
}

fn e<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> String + '_ {
    move |err| format!("cursor overlay: {what}: {err}")
}

impl X11Overlay {
    fn new() -> Result<Self, String> {
        let (conn, screen_num) = x11rb::connect(None).map_err(e("connect"))?;
        let setup = conn.setup();
        let screen = &setup.roots[screen_num];
        let (root, depth) = (screen.root, screen.root_depth);
        let pixmap_ok = setup
            .pixmap_formats
            .iter()
            .any(|f| f.depth == depth && f.bits_per_pixel == 32 && f.scanline_pad == 32);
        let msb_first = setup.image_byte_order == ImageOrder::MSB_FIRST;
        let ext = conn
            .extension_information(shape::X11_EXTENSION_NAME)
            .map_err(e("query extensions"))?;
        if ext.is_none() {
            return Err("cursor overlay: the X server has no Shape extension, so the window could not be click-through".into());
        }
        let win = conn.generate_id().map_err(e("window id"))?;
        let side = u16::try_from(desktop_api::cursor::sprite::BASE_CANVAS).unwrap_or(72);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            side,
            side,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0x00FF_6A00)
                .event_mask(x11rb::protocol::xproto::EventMask::NO_EVENT),
        )
        .map_err(e("create window"))?;
        // Click-through before anything can be shown: an empty input region.
        conn.shape_rectangles(
            SO::SET,
            SK::INPUT,
            x11rb::protocol::xproto::ClipOrdering::UNSORTED,
            win,
            0,
            0,
            &[],
        )
        .map_err(e("empty input shape"))?;
        conn.sync().map_err(e("sync"))?;
        Ok(Self {
            conn,
            root,
            win,
            depth,
            pixmap_ok,
            msb_first,
            visible: false,
            last_origin: None,
        })
    }

    fn paint(&self, bmp: &Bitmap) -> Result<(), String> {
        let rects: Vec<Rectangle> = mask_rects(bmp, SHAPE_ALPHA)
            .into_iter()
            .map(|(x, y, width, height)| Rectangle {
                x,
                y,
                width,
                height,
            })
            .collect();
        self.conn
            .shape_rectangles(
                SO::SET,
                SK::BOUNDING,
                x11rb::protocol::xproto::ClipOrdering::YX_BANDED,
                self.win,
                0,
                0,
                &rects,
            )
            .map_err(e("bounding shape"))?;
        if self.pixmap_ok {
            let mut data = opaque_bgrx(bmp);
            if self.msb_first {
                for px in data.as_chunks_mut::<4>().0 {
                    px.reverse();
                }
            }
            let pm = self.conn.generate_id().map_err(e("pixmap id"))?;
            let w = u16::try_from(bmp.width).unwrap_or(0);
            let h = u16::try_from(bmp.height).unwrap_or(0);
            self.conn
                .create_pixmap(self.depth, pm, self.win, w, h)
                .map_err(e("create pixmap"))?;
            let gc = self.conn.generate_id().map_err(e("gc id"))?;
            self.conn
                .create_gc(gc, pm, &x11rb::protocol::xproto::CreateGCAux::new())
                .map_err(e("create gc"))?;
            self.conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    pm,
                    gc,
                    w,
                    h,
                    0,
                    0,
                    0,
                    self.depth,
                    &data,
                )
                .map_err(e("put image"))?;
            self.conn
                .change_window_attributes(
                    self.win,
                    &ChangeWindowAttributesAux::new().background_pixmap(pm),
                )
                .map_err(e("background"))?;
            let _ = self.conn.free_gc(gc);
            let _ = self.conn.free_pixmap(pm);
        }
        Ok(())
    }
}

impl OverlayWindow for X11Overlay {
    fn place(&mut self, origin: (i32, i32), bitmap: &Bitmap) -> Result<(), String> {
        self.paint(bitmap)?;
        if self.last_origin != Some(origin) || !self.visible {
            self.conn
                .configure_window(
                    self.win,
                    &ConfigureWindowAux::new()
                        .x(origin.0)
                        .y(origin.1)
                        .stack_mode(StackMode::ABOVE),
                )
                .map_err(e("move"))?;
            self.last_origin = Some(origin);
        }
        if self.visible {
            // Keep it on top of windows that were raised since.
            self.conn
                .configure_window(
                    self.win,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )
                .map_err(e("raise"))?;
        } else {
            self.conn.map_window(self.win).map_err(e("map"))?;
            self.visible = true;
        }
        self.conn
            .clear_area(false, self.win, 0, 0, 0, 0)
            .map_err(e("repaint"))?;
        self.conn.flush().map_err(e("flush"))
    }

    fn hide(&mut self) -> Result<(), String> {
        if self.visible {
            self.conn.unmap_window(self.win).map_err(e("unmap"))?;
            self.visible = false;
        }
        self.conn.flush().map_err(e("flush"))
    }
}

impl Drop for X11Overlay {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.win);
        let _ = self.conn.flush();
        let _ = self.root;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::MapState;

    /// Needs an X server (`xvfb-run cargo test -p desktop-linux cursor_x11`); skipped otherwise.
    fn have_x() -> bool {
        std::env::var_os("DISPLAY").is_some() && x11rb::connect(None).is_ok()
    }

    fn find_overlay(conn: &RustConnection, root: Window) -> Vec<Window> {
        conn.query_tree(root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .filter(|w| {
                conn.get_window_attributes(*w)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_some_and(|a| a.override_redirect)
                    && conn
                        .get_geometry(*w)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .is_some_and(|g| g.width == 72 && g.height == 72)
            })
            .collect()
    }

    #[test]
    fn the_window_is_override_redirect_click_through_shaped_mapped_and_unmapped() {
        if !have_x() {
            eprintln!("skipped: no X server (run under xvfb-run)");
            return;
        }
        let (conn, n) = x11rb::connect(None).unwrap();
        let root = conn.setup().roots[n].root;
        let surface = open().unwrap();
        surface.move_to(200.0, 150.0).unwrap();
        let mut found = Vec::new();
        for _ in 0..200 {
            found = find_overlay(&conn, root);
            if let Some(w) = found.first() {
                let a = conn.get_window_attributes(*w).unwrap().reply().unwrap();
                if a.map_state == MapState::VIEWABLE {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let win = *found.first().expect("overlay window exists");
        let attrs = conn.get_window_attributes(win).unwrap().reply().unwrap();
        assert!(
            attrs.override_redirect,
            "the window manager must not manage it"
        );
        assert_eq!(attrs.map_state, MapState::VIEWABLE);
        let geo = conn.get_geometry(win).unwrap().reply().unwrap();
        assert_eq!(
            (i32::from(geo.x), i32::from(geo.y)),
            (164, 114),
            "centred on the point"
        );
        // Click-through: the input region is empty. The bounding region is the arrow.
        let input = conn
            .shape_get_rectangles(win, SK::INPUT)
            .unwrap()
            .reply()
            .unwrap();
        assert!(input.rectangles.is_empty(), "input region must be empty");
        let bounding = conn
            .shape_get_rectangles(win, SK::BOUNDING)
            .unwrap()
            .reply()
            .unwrap();
        assert!(!bounding.rectangles.is_empty());
        let area: u32 = bounding
            .rectangles
            .iter()
            .map(|r| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert!(
            area < 72 * 72 / 4,
            "shaped like an arrow, not a square: {area}"
        );
        // A ripple widens the shape, then it settles; hide unmaps.
        surface.ripple(200.0, 150.0).unwrap();
        surface.hide().unwrap();
        for _ in 0..200 {
            let a = conn.get_window_attributes(win).unwrap().reply().unwrap();
            if a.map_state == MapState::UNMAPPED {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let a = conn.get_window_attributes(win).unwrap().reply().unwrap();
        assert_eq!(a.map_state, MapState::UNMAPPED);
        drop(surface);
        assert!(find_overlay(&conn, root).is_empty(), "destroyed on drop");
    }
}

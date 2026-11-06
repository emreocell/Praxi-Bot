//! GDI screen capture and JPEG / PNG encoding.
#![allow(unsafe_code)]

use desktop_api::{
    DesktopError,
    snapshot::{Image, Quality, Rect, Region},
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SRCCOPY, SelectObject,
    },
    UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

use crate::winlist::{hwnd_from_id, window_bounds};

fn region_rect(region: &Region) -> Result<(i32, i32, i32, i32), DesktopError> {
    let r: Rect = match region {
        Region::FullScreen => {
            // SAFETY: GetSystemMetrics takes a constant and returns an int.
            unsafe {
                Rect {
                    x: f64::from(GetSystemMetrics(SM_XVIRTUALSCREEN)),
                    y: f64::from(GetSystemMetrics(SM_YVIRTUALSCREEN)),
                    width: f64::from(GetSystemMetrics(SM_CXVIRTUALSCREEN)),
                    height: f64::from(GetSystemMetrics(SM_CYVIRTUALSCREEN)),
                }
            }
        }
        Region::Window { window_id } => window_bounds(hwnd_from_id(*window_id))
            .ok_or_else(|| DesktopError::Screenshot("window not found".into()))?,
        Region::Rect(r) => *r,
    };
    // Pixel rectangles are small; rounding to i32 is intended.
    #[allow(clippy::cast_possible_truncation)]
    let out = (
        r.x.round() as i32,
        r.y.round() as i32,
        r.width.round() as i32,
        r.height.round() as i32,
    );
    if out.2 <= 0 || out.3 <= 0 {
        return Err(DesktopError::Screenshot("empty capture region".into()));
    }
    Ok(out)
}

/// Capture BGRA pixels (top-down) for a screen rectangle.
fn grab(x: i32, y: i32, w: i32, h: i32) -> Result<Vec<u8>, DesktopError> {
    let err = |m: &str| DesktopError::Screenshot(m.to_string());
    // SAFETY: standard GDI capture sequence. Every acquired object (screen DC,
    // memory DC, bitmap) is released/deleted on all paths below; the pixel
    // buffer is exactly w*h*4 bytes as required by GetDIBits for 32 bpp.
    unsafe {
        let screen = GetDC(Some(HWND::default()));
        if screen.is_invalid() {
            return Err(err("GetDC failed"));
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let blt = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                #[allow(clippy::cast_possible_truncation)]
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        #[allow(clippy::cast_sign_loss)]
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        #[allow(clippy::cast_sign_loss)]
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr().cast()),
            &raw mut bi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(Some(HWND::default()), screen);

        if blt.is_err() {
            return Err(err("BitBlt failed"));
        }
        if lines == 0 {
            return Err(err("GetDIBits failed"));
        }
        Ok(buf)
    }
}

fn jpeg_quality(q: Quality) -> u8 {
    match q {
        Quality::Low => 40,
        Quality::Medium => 70,
        Quality::High | Quality::Lossless => 90,
    }
}

/// Capture `region` and encode it (JPEG, or PNG for [`Quality::Lossless`]).
pub fn screenshot(region: &Region, quality: Quality) -> Result<Image, DesktopError> {
    let (left, top, w, h) = region_rect(region)?;
    let mut bgra = grab(left, top, w, h)?;
    #[allow(clippy::cast_sign_loss)]
    let (wu, hu) = (w as u32, h as u32);
    let mut out = Vec::new();
    let mime_type = if quality == Quality::Lossless {
        // BGRA -> RGBA, opaque alpha (GDI leaves it zero).
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        let mut enc = png::Encoder::new(&mut out, wu, hu);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| DesktopError::Screenshot(format!("png: {e}")))?;
        writer
            .write_image_data(&bgra)
            .map_err(|e| DesktopError::Screenshot(format!("png: {e}")))?;
        writer
            .finish()
            .map_err(|e| DesktopError::Screenshot(format!("png: {e}")))?;
        "image/png"
    } else {
        let enc = jpeg_encoder::Encoder::new(&mut out, jpeg_quality(quality));
        let (Ok(jw), Ok(jh)) = (u16::try_from(wu), u16::try_from(hu)) else {
            return Err(DesktopError::Screenshot("image too large for JPEG".into()));
        };
        enc.encode(&bgra, jw, jh, jpeg_encoder::ColorType::Bgra)
            .map_err(|e| DesktopError::Screenshot(format!("jpeg: {e}")))?;
        "image/jpeg"
    };
    Ok(Image {
        data: out,
        width: wu,
        height: hu,
        mime_type: mime_type.to_string(),
    })
}

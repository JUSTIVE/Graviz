//! Debug self-screenshot: capturing an app's OWN windows does not require the
//! Screen Recording permission, so `GRAVIZ_SELFSHOT=/path.png` lets an
//! agent (or CI) see what the window actually rendered.

#![cfg(target_os = "macos")]

use anyhow::{anyhow, Context, Result};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::window as cgw;

fn own_window_id() -> Result<u32> {
    let pid = std::process::id() as i64;
    let info: CFArray<*const std::ffi::c_void> = cgw::copy_window_info(
        cgw::kCGWindowListOptionOnScreenOnly | cgw::kCGWindowListExcludeDesktopElements,
        cgw::kCGNullWindowID,
    )
    .ok_or_else(|| anyhow!("CGWindowListCopyWindowInfo returned null"))?;

    let mut best: Option<(f64, u32)> = None;
    for item in info.iter() {
        let dict = unsafe {
            CFDictionary::<CFString, CFType>::wrap_under_get_rule(*item as *const _)
        };
        let owner = dict
            .find(CFString::from_static_string("kCGWindowOwnerPID"))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64());
        if owner != Some(pid) {
            continue;
        }
        // skip menu/panel layers, then take the LARGEST window: GPUI can own
        // more than one (helper surfaces), and the first is not always the
        // real one.
        let layer = dict
            .find(CFString::from_static_string("kCGWindowLayer"))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
            .unwrap_or(0);
        if layer != 0 {
            continue;
        }
        let area = dict
            .find(CFString::from_static_string("kCGWindowBounds"))
            .and_then(|v| v.downcast::<CFDictionary>())
            .and_then(|b| {
                let get = |k: &'static str| {
                    b.find(CFString::from_static_string(k).as_CFTypeRef() as *const _)
                        .map(|v| unsafe { CFNumber::wrap_under_get_rule(*v as *const _) })
                        .and_then(|n| n.to_f64())
                };
                Some(get("Width")? * get("Height")?)
            })
            .unwrap_or(0.0);
        if let Some(id) = dict
            .find(CFString::from_static_string("kCGWindowNumber"))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
        {
            if best.map(|(a, _)| area > a).unwrap_or(true) {
                best = Some((area, id as u32));
            }
        }
    }
    if let Some((_, id)) = best {
        return Ok(id);
    }
    Err(anyhow!("no on-screen window owned by pid {pid}"))
}

pub fn capture_to(path: &str) -> Result<()> {
    let win = own_window_id()?;
    let null_rect = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0.0, 0.0),
    );
    let img = cgw::create_image(
        null_rect,
        cgw::kCGWindowListOptionIncludingWindow,
        win,
        cgw::kCGWindowImageBoundsIgnoreFraming | cgw::kCGWindowImageBestResolution,
    )
    .ok_or_else(|| anyhow!("CGWindowListCreateImage returned null (window {win})"))?;

    let w = img.width() as u32;
    let h = img.height() as u32;
    let stride = img.bytes_per_row();
    let data = img.data();
    let bytes = data.bytes();

    // BGRA (little-endian ARGB) → RGBA
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        let row = &bytes[y * stride..];
        for x in 0..w as usize {
            let src = &row[x * 4..x * 4 + 4];
            let dst = &mut rgba[(y * w as usize + x) * 4..][..4];
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = 255;
        }
    }
    image::RgbaImage::from_raw(w, h, rgba)
        .ok_or_else(|| anyhow!("image buffer size mismatch"))?
        .save(path)
        .with_context(|| format!("saving {path}"))?;
    eprintln!("selfshot: wrote {w}×{h} to {path}");
    Ok(())
}

/// When `GRAVIZ_SELFSHOT` is set, capture the window after a delay and exit.
pub fn arm_if_requested() {
    let Ok(path) = std::env::var("GRAVIZ_SELFSHOT") else {
        return;
    };
    let delay_ms: u64 = std::env::var("GRAVIZ_SELFSHOT_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        for attempt in 0..3 {
            match capture_to(&path) {
                Ok(()) => std::process::exit(0),
                Err(e) if attempt == 2 => {
                    eprintln!("selfshot failed: {e:#}");
                    std::process::exit(1);
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_secs(1)),
            }
        }
    });
}

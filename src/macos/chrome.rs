//! macOS window chrome: the native title bar (traffic lights) is kept, so
//! there is no hit-testing to install; the Windows-only hooks are no-ops.
//! `main` only calls them with a Win32 handle, which never exists here
//! (except [`monitor_refresh_hz`], which needs none).

use crate::geometry::{self, ChromeLayout};

pub fn set_layout(_layout: ChromeLayout) {}

pub fn install(_window: isize) {}

pub fn set_cloaked(_window: isize, _cloaked: bool) {}

pub fn resize_width_keep_rail(_window: isize, _new_width: i32, _rail: geometry::Side) {}

/// Refresh rate of the main display (CoreGraphics; no window handle is
/// needed). `None` when the mode reports 0 Hz, as some built-in panels do.
pub fn monitor_refresh_hz(_window: isize) -> Option<u32> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
        fn CGDisplayModeGetRefreshRate(mode: *mut std::ffi::c_void) -> f64;
        fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
    }
    // SAFETY: plain CoreGraphics C calls; the copied mode is released once.
    let hz = unsafe {
        let mode = CGDisplayCopyDisplayMode(CGMainDisplayID());
        if mode.is_null() {
            return None;
        }
        let hz = CGDisplayModeGetRefreshRate(mode);
        CGDisplayModeRelease(mode);
        hz
    };
    (hz >= 1.0).then(|| hz.round() as u32)
}

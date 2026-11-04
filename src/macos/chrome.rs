//! macOS window chrome: the native title bar (traffic lights) is kept, so
//! there is no hit-testing to install; the Windows-only hooks are no-ops.
//! `main` only calls them with a Win32 handle, which never exists here.

use crate::geometry::{self, ChromeLayout};

pub fn set_layout(_layout: ChromeLayout) {}

pub fn install(_window: isize) {}

pub fn set_cloaked(_window: isize, _cloaked: bool) {}

pub fn resize_width_keep_rail(_window: isize, _new_width: i32, _rail: geometry::Side) {}

pub fn monitor_refresh_hz(_window: isize) -> Option<u32> {
    None
}

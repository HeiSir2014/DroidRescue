//! Custom window chrome. egui paints the title bar and frame; this module
//! tells Windows which parts are non-client (caption / resize borders) via
//! WM_NCHITTEST, so dragging, double-click, Aero Snap and edge resizing stay
//! fully native.

use crate::geometry::{self, ChromeLayout, Hit};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT,
    HTTOP, HTTOPLEFT, HTTOPRIGHT, WM_NCHITTEST,
};

static LAYOUT: Mutex<ChromeLayout> = Mutex::new(ChromeLayout {
    caption_height: 40,
    controls_left: i32::MAX,
    border: 6,
    resizable: geometry::Edges::ALL,
});

const SUBCLASS_ID: usize = 0x7061_6463; // "padc"

/// Publish the current non-client layout (called by the UI every frame).
pub fn set_layout(layout: ChromeLayout) {
    *LAYOUT.lock().unwrap() = layout;
}

/// Install the hit-test subclass and DWM shadow + rounded corners.
pub fn install(hwnd: isize) {
    let hwnd = hwnd as HWND;
    // SAFETY: hwnd is our own live top-level window; called on its UI thread.
    unsafe {
        SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0);
        let pref = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&raw const pref).cast(),
            size_of_val(&pref) as u32,
        );
        // A 1px extended frame makes DWM draw the native drop shadow.
        let margins = MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: 1,
            cyBottomHeight: 0,
        };
        DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

/// Hide/show the window at the DWM level without changing its state (no
/// taskbar/focus change). Used to mask the frames between a resize and the
/// first correctly laid-out present, which would otherwise show a stretched
/// or mis-placed rail.
pub fn set_cloaked(hwnd: isize, cloaked: bool) {
    use windows_sys::Win32::Graphics::Dwm::DWMWA_CLOAK;
    let value: i32 = i32::from(cloaked);
    // SAFETY: setting a DWM attribute on our own window.
    unsafe {
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_CLOAK as u32,
            (&raw const value).cast(),
            size_of_val(&value) as u32,
        );
    }
}

/// Change our window's width in one atomic `SetWindowPos` (no two-step
/// flicker), keeping the rail edge fixed and the window inside the work area.
pub fn resize_width_keep_rail(hwnd: isize, new_width: i32, rail: geometry::Side) {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};
    let hwnd = hwnd as HWND;
    // SAFETY: queries and a resize of our own window, on its UI thread.
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) == 0 {
            return;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
        let left = geometry::resize_keep_rail(
            r.left,
            r.right - r.left,
            new_width,
            rail,
            info.rcWork.left,
            info.rcWork.right,
        );
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            left,
            r.top,
            new_width,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Refresh rate (Hz) of the monitor our window is on.
pub fn monitor_refresh_hz(hwnd: isize) -> Option<u32> {
    use windows_sys::Win32::Graphics::Gdi::{
        DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
        MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
    };
    // SAFETY: plain queries; structs are zeroed and size-tagged as the API requires.
    unsafe {
        let monitor = MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, (&raw mut info).cast()) == 0 {
            return None;
        }
        let mut mode: DEVMODEW = std::mem::zeroed();
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) == 0 {
            return None;
        }
        // 0 and 1 mean "hardware default".
        (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency)
    }
}

fn hit_code(hit: Hit) -> u32 {
    match hit {
        Hit::Client => HTCLIENT,
        Hit::Caption => HTCAPTION,
        Hit::Left => HTLEFT,
        Hit::Right => HTRIGHT,
        Hit::Top => HTTOP,
        Hit::Bottom => HTBOTTOM,
        Hit::TopLeft => HTTOPLEFT,
        Hit::TopRight => HTTOPRIGHT,
        Hit::BottomLeft => HTBOTTOMLEFT,
        Hit::BottomRight => HTBOTTOMRIGHT,
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            // Signed screen coordinates packed in lparam (multi-monitor safe).
            let x = i32::from((lparam & 0xFFFF) as u16 as i16);
            let y = i32::from(((lparam >> 16) & 0xFFFF) as u16 as i16);
            let mut r: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetWindowRect(hwnd, &mut r) };
            let layout = *LAYOUT.lock().unwrap();
            let hit = geometry::hit_test(
                x - r.left,
                y - r.top,
                r.right - r.left,
                r.bottom - r.top,
                &layout,
            );
            hit_code(hit) as LRESULT
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

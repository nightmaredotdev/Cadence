//! Where windows can be placed: the work area (the monitor minus the
//! taskbar and docked bars) of the monitor a window is on.

use super::error::Win32Error;
use cadence_core::geometry::Rect;
use std::mem::size_of;
use windows_sys::Win32::Foundation::{ERROR_INVALID_WINDOW_HANDLE, HWND};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

/// The work area of the monitor that `hwnd` is on (or nearest to), in
/// physical screen coordinates.
pub fn monitor_work_area(hwnd: isize) -> Result<Rect, Win32Error> {
    let window = hwnd as HWND;
    // SAFETY: no pointer arguments; any value is accepted and checked.
    if hwnd == 0 || unsafe { IsWindow(window) } == 0 {
        return Err(Win32Error::from_code(ERROR_INVALID_WINDOW_HANDLE));
    }
    // SAFETY: a valid window; MONITOR_DEFAULTTONEAREST never returns null.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `monitor` is a valid handle and `info.cbSize` is initialized.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return Err(Win32Error::last());
    }
    let r = info.rcWork;
    Ok(Rect::new(r.left, r.top, r.right, r.bottom))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WNDCLASSW, WS_POPUP,
    };

    #[test]
    fn the_work_area_of_a_window_is_on_its_monitor() {
        let class: Vec<u16> = "CadencePlacementTest\0".encode_utf16().collect();
        // SAFETY: null module name returns this executable's handle.
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        // SAFETY: `wc` is initialized; registering twice fails harmlessly.
        unsafe { RegisterClassW(&wc) };
        // SAFETY: registered class, valid strings, no parent or menu; never shown.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_POPUP,
                10,
                10,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "{}", Win32Error::last());
        let work = monitor_work_area(hwnd as isize).unwrap();
        assert!(
            work.right - work.left > 100 && work.bottom - work.top > 100,
            "{work:?}"
        );
        // SAFETY: a window created (and owned) by this thread.
        unsafe { DestroyWindow(hwnd) };
        assert_eq!(
            monitor_work_area(hwnd as isize).unwrap_err().code,
            ERROR_INVALID_WINDOW_HANDLE
        );
        assert!(monitor_work_area(0).is_err());
    }
}

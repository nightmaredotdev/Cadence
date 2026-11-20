//! Native support for overlay mode: whole-window opacity and a read-back of
//! the window styles that make a window translucent and click-through.
//!
//! Click-through itself (`WS_EX_TRANSPARENT | WS_EX_LAYERED`) is switched by
//! the windowing library that owns the window (winit, through
//! `set_cursor_hittest`), because it rewrites the whole extended style from
//! its own state whenever the window changes (shown, hidden, made topmost,
//! ...); bits set behind its back would be lost. Opacity is not a style, so it
//! is set here, on a window that is already layered.
//!
//! Every function only accepts windows of the calling process, so a stale or
//! foreign handle can never change another program's window.

use super::error::Win32Error;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_INVALID_WINDOW_HANDLE, HWND,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow,
    LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, SetLayeredWindowAttributes, WS_EX_LAYERED,
    WS_EX_TRANSPARENT,
};

/// The native state of a window, as far as overlay mode is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayStyles {
    /// `WS_EX_LAYERED`: the window can have a whole-window opacity.
    pub layered: bool,
    /// `WS_EX_TRANSPARENT` on a layered window: mouse input passes through
    /// to the windows underneath.
    pub click_through: bool,
    /// The whole-window opacity (255 = opaque), if one is set.
    pub alpha: Option<u8>,
}

impl OverlayStyles {
    /// Takes mouse input and is drawn normally (not layered at all).
    pub fn is_normal(&self) -> bool {
        !self.click_through && !self.layered
    }
}

/// Fails unless `hwnd` is an existing window of this process.
fn own_window(hwnd: isize) -> Result<HWND, Win32Error> {
    let window = hwnd as HWND;
    // SAFETY: no pointer arguments; any value is accepted and checked.
    if hwnd == 0 || unsafe { IsWindow(window) } == 0 {
        return Err(Win32Error::from_code(ERROR_INVALID_WINDOW_HANDLE));
    }
    let mut pid = 0u32;
    // SAFETY: valid out-pointer; the handle was just checked.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    // SAFETY: no preconditions.
    if pid != unsafe { GetCurrentProcessId() } {
        return Err(Win32Error::from_code(ERROR_ACCESS_DENIED));
    }
    Ok(window)
}

fn ex_style(window: HWND) -> u32 {
    // SAFETY: no pointer arguments; the handle was validated by the caller.
    unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 }
}

/// Sets the opacity of a layered window of this process (255 = opaque).
/// Fails, changing nothing, if the window is not layered.
pub fn set_window_opacity(hwnd: isize, alpha: u8) -> Result<(), Win32Error> {
    let window = own_window(hwnd)?;
    if ex_style(window) & WS_EX_LAYERED == 0 {
        return Err(Win32Error {
            code: ERROR_INVALID_PARAMETER,
            message: "the window is not a layered window".into(),
        });
    }
    // SAFETY: a valid layered window of this process; no pointer arguments.
    if unsafe { SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA) } == 0 {
        return Err(Win32Error::last());
    }
    Ok(())
}

/// Reads the overlay-related state of a window of this process.
pub fn window_overlay_styles(hwnd: isize) -> Result<OverlayStyles, Win32Error> {
    let window = own_window(hwnd)?;
    let ex = ex_style(window);
    let layered = ex & WS_EX_LAYERED != 0;
    let alpha = layered.then(|| layered_alpha(window)).flatten();
    Ok(OverlayStyles {
        layered,
        click_through: layered && ex & WS_EX_TRANSPARENT != 0,
        alpha,
    })
}

fn layered_alpha(window: HWND) -> Option<u8> {
    let (mut key, mut alpha, mut flags) = (0u32, 0u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS::default());
    // SAFETY: valid out-pointers; fails harmlessly for a window whose layered
    // attributes were never set.
    let ok = unsafe { GetLayeredWindowAttributes(window, &mut key, &mut alpha, &mut flags) };
    (ok != 0 && flags & LWA_ALPHA != 0).then_some(alpha)
}

#[cfg(test)]
mod tests {
    //! These tests use a window that is never shown, so they need no
    //! interactive desktop and run with the normal test suite.

    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetDesktopWindow, RegisterClassW,
        SetWindowLongPtrW, WNDCLASSW, WS_POPUP,
    };

    /// A hidden top-level window of this thread, destroyed on drop.
    struct HiddenWindow(HWND);

    impl HiddenWindow {
        fn new() -> Self {
            let class: Vec<u16> = "CadenceOverlayTest\0".encode_utf16().collect();
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
            // SAFETY: registered class, valid strings, no parent or menu; not visible.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    module,
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null(), "{}", Win32Error::last());
            Self(hwnd)
        }

        fn id(&self) -> isize {
            self.0 as isize
        }

        /// What the windowing library does for click-through (on or off).
        fn set_click_through(&self, on: bool) {
            let bits = WS_EX_LAYERED | WS_EX_TRANSPARENT;
            let ex = ex_style(self.0);
            let ex = if on { ex | bits } else { ex & !bits };
            // SAFETY: a window of this thread; no pointer arguments.
            unsafe { SetWindowLongPtrW(self.0, GWL_EXSTYLE, ex as isize) };
        }
    }

    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            // SAFETY: a window created (and owned) by this thread.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn a_normal_window_is_reported_as_normal() {
        let w = HiddenWindow::new();
        let s = window_overlay_styles(w.id()).unwrap();
        assert!(s.is_normal(), "{s:?}");
        assert_eq!(s.alpha, None);
    }

    #[test]
    fn opacity_needs_a_layered_window_and_changes_nothing_otherwise() {
        let w = HiddenWindow::new();
        let e = set_window_opacity(w.id(), 153).unwrap_err();
        assert_eq!(e.code, ERROR_INVALID_PARAMETER, "{e}");
        assert!(window_overlay_styles(w.id()).unwrap().is_normal());
    }

    #[test]
    fn click_through_and_opacity_are_read_back_and_reversible() {
        let w = HiddenWindow::new();
        w.set_click_through(true);
        // Layered but without attributes yet: no opacity is reported.
        let s = window_overlay_styles(w.id()).unwrap();
        assert!(s.click_through && s.layered && s.alpha.is_none(), "{s:?}");

        set_window_opacity(w.id(), 153).unwrap();
        let s = window_overlay_styles(w.id()).unwrap();
        assert!(s.click_through && s.alpha == Some(153), "{s:?}");

        set_window_opacity(w.id(), 255).unwrap();
        assert_eq!(window_overlay_styles(w.id()).unwrap().alpha, Some(255));

        w.set_click_through(false);
        let s = window_overlay_styles(w.id()).unwrap();
        assert!(s.is_normal(), "{s:?}");
        assert_eq!(s.alpha, None, "removing WS_EX_LAYERED drops the opacity");
    }

    #[test]
    fn invalid_and_foreign_windows_are_refused() {
        assert_eq!(
            window_overlay_styles(0).unwrap_err().code,
            ERROR_INVALID_WINDOW_HANDLE
        );
        let gone = HiddenWindow::new();
        let id = gone.id();
        drop(gone);
        assert_eq!(
            set_window_opacity(id, 153).unwrap_err().code,
            ERROR_INVALID_WINDOW_HANDLE
        );
        assert_eq!(
            window_overlay_styles(id).unwrap_err().code,
            ERROR_INVALID_WINDOW_HANDLE
        );
        // The desktop window belongs to another process.
        // SAFETY: no preconditions.
        let desktop = unsafe { GetDesktopWindow() } as isize;
        assert_eq!(
            set_window_opacity(desktop, 153).unwrap_err().code,
            ERROR_ACCESS_DENIED
        );
    }
}

//! Overlay mode: the window becomes translucent and mouse clicks pass
//! through it to the windows underneath, while clicking, hotkeys and the
//! tray keep working.
//!
//! Safety rules (the user must never lose control of the window):
//! * Overlay mode is only turned on while the overlay hotkey is registered,
//!   because that hotkey is the quick way back ([`blocker`]). The tray, the
//!   tray icon, closing the window and starting Cadence again also end it.
//! * The mode is only reported as active when the native window is verified
//!   to be click-through and translucent. Every transition reads the native
//!   state back; a transition that fails is undone, and what the window
//!   actually ends up as is what is reported ([`turn_on`], [`turn_off`]).
//!
//! The native operations are behind [`OverlayWindow`], so these rules are
//! tested without a desktop.

#![forbid(unsafe_code)]

/// Opacity of the window in overlay mode (0-255): translucent, with the
/// status still readable over most backgrounds.
pub const OVERLAY_ALPHA: u8 = 166;

/// What overlay mode needs to know about the native window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeState {
    /// Mouse input passes through the window.
    pub click_through: bool,
    /// Whole-window opacity, if one is set (255 = opaque).
    pub alpha: Option<u8>,
    /// The window is layered (needed for an opacity; a layered window
    /// without an opacity is not drawn at all).
    pub layered: bool,
}

/// The native operations of overlay mode.
pub trait OverlayWindow {
    /// Makes mouse input pass through the window (`true`) or not (`false`).
    /// Click-through windows are layered windows.
    fn set_click_through(&self, on: bool) -> Result<(), String>;
    /// Sets the opacity of a layered window.
    fn set_opacity(&self, alpha: u8) -> Result<(), String>;
    /// Reads the current native state. Fails if the window does not exist.
    fn state(&self) -> Result<NativeState, String>;
}

/// The outcome of a transition: the verified state, and why the requested
/// state was not reached (if it was not).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    /// The window is click-through now (overlay mode is active).
    pub active: bool,
    pub error: Option<String>,
}

/// Whether the overlay hotkey can bring the window back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyStatus {
    /// Registered with Windows: pressing it ends overlay mode.
    Registered,
    /// No overlay hotkey is set.
    Unbound,
    /// Set, but Windows refused it (the reason is given).
    Unavailable(String),
    /// Hotkeys are paused (while a new binding is being recorded, or if they
    /// could not be registered again afterwards).
    Suspended,
}

/// Why overlay mode cannot be turned on now (`None` if it can), as shown to
/// the user.
pub fn blocker(hotkey: &HotkeyStatus) -> Option<String> {
    match hotkey {
        HotkeyStatus::Registered => None,
        HotkeyStatus::Unbound => Some(
            "Overlay mode needs an overlay hotkey to switch it off again. Set one on the \
             Hotkeys tab."
                .into(),
        ),
        HotkeyStatus::Unavailable(reason) => Some(format!(
            "Overlay mode needs a working overlay hotkey to switch it off again ({reason}). \
             Choose another one on the Hotkeys tab."
        )),
        HotkeyStatus::Suspended => Some(
            "Overlay mode is not available while the hotkeys are paused (while a new one is \
             being recorded)."
                .into(),
        ),
    }
}

/// Makes the window translucent (`alpha`) and click-through. On any failure
/// the change is undone and the window's verified state is reported.
pub fn turn_on(window: &dyn OverlayWindow, alpha: u8) -> Transition {
    let attempt = (|| {
        window.set_click_through(true)?;
        window.set_opacity(alpha)?;
        let state = window.state()?;
        if state.click_through && state.alpha == Some(alpha) {
            Ok(())
        } else {
            Err(format!(
                "the window did not become translucent and click-through ({state:?})"
            ))
        }
    })();
    match attempt {
        Ok(()) => Transition {
            active: true,
            error: None,
        },
        Err(error) => {
            // Undo whatever part took effect. A layered window without an
            // opacity is invisible, so this matters even when click-through
            // itself failed.
            let undo = window.set_click_through(false);
            let active = is_click_through(window);
            let error = match undo {
                Err(e) if active => format!("{error}; restoring the window also failed: {e}"),
                _ => error,
            };
            Transition {
                active,
                error: Some(error),
            }
        }
    }
}

/// Restores normal opacity and mouse input. If that fails, the window's
/// verified state is reported (overlay mode stays active, so the hotkey and
/// the tray can try again).
pub fn turn_off(window: &dyn OverlayWindow) -> Transition {
    let result = window.set_click_through(false);
    // Not layered any more means fully opaque; if something else keeps the
    // window layered, make sure it is not left translucent.
    if let Ok(state) = window.state()
        && state.layered
        && !state.click_through
        && state.alpha.is_some_and(|a| a != u8::MAX)
    {
        let _ = window.set_opacity(u8::MAX);
    }
    let active = is_click_through(window);
    let error = match result {
        Err(e) => Some(e),
        Ok(()) if active => Some("the window is still click-through".into()),
        Ok(()) => None,
    };
    Transition { active, error }
}

/// Whether the window is click-through right now. A window whose state
/// cannot be read no longer exists, so it cannot be in the way.
fn is_click_through(window: &dyn OverlayWindow) -> bool {
    window.state().is_ok_and(|s| s.click_through)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A simulated window following the rules of a real one: click-through
    /// makes it layered, removing it drops the opacity. Operations can be
    /// made to fail.
    #[derive(Default)]
    struct FakeWindow {
        state: Cell<Option<NativeState>>,
        fail_click_through: Cell<Option<bool>>,
        fail_opacity: Cell<bool>,
        /// Opacity calls succeed but have no effect.
        ignore_opacity: Cell<bool>,
        /// Something else also makes the window layered, so it stays layered
        /// (with its opacity) when click-through is removed.
        layered_elsewhere: Cell<bool>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeWindow {
        fn normal() -> Self {
            let w = Self::default();
            w.state.set(Some(NativeState {
                click_through: false,
                alpha: None,
                layered: false,
            }));
            w
        }

        fn current(&self) -> NativeState {
            self.state.get().expect("window exists")
        }

        fn destroy(&self) {
            self.state.set(None);
        }
    }

    impl OverlayWindow for FakeWindow {
        fn set_click_through(&self, on: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("click_through({on})"));
            if self.fail_click_through.get() == Some(on) {
                return Err(format!("cannot set click-through to {on}"));
            }
            let Some(s) = self.state.get() else {
                return Err("no window".into());
            };
            let layered = on || self.layered_elsewhere.get();
            self.state.set(Some(NativeState {
                click_through: on,
                layered,
                alpha: if layered { s.alpha } else { None },
            }));
            Ok(())
        }

        fn set_opacity(&self, alpha: u8) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("opacity({alpha})"));
            let Some(s) = self.state.get() else {
                return Err("no window".into());
            };
            if self.fail_opacity.get() || !s.layered {
                return Err("cannot set the opacity".into());
            }
            if !self.ignore_opacity.get() {
                self.state.set(Some(NativeState {
                    alpha: Some(alpha),
                    ..s
                }));
            }
            Ok(())
        }

        fn state(&self) -> Result<NativeState, String> {
            self.state.get().ok_or_else(|| "no window".into())
        }
    }

    fn ok(active: bool) -> Transition {
        Transition {
            active,
            error: None,
        }
    }

    #[test]
    fn turning_on_and_off_is_verified_and_reversible() {
        let w = FakeWindow::normal();
        assert_eq!(turn_on(&w, OVERLAY_ALPHA), ok(true));
        assert_eq!(
            w.current(),
            NativeState {
                click_through: true,
                alpha: Some(OVERLAY_ALPHA),
                layered: true
            }
        );
        assert_eq!(turn_off(&w), ok(false));
        assert_eq!(
            w.current(),
            NativeState {
                click_through: false,
                alpha: None,
                layered: false
            },
            "back to a normal, opaque window"
        );
        // Again, to show that nothing is left over.
        assert_eq!(turn_on(&w, OVERLAY_ALPHA), ok(true));
        assert_eq!(turn_off(&w), ok(false));
    }

    #[test]
    fn a_failed_click_through_change_reports_overlay_as_off() {
        let w = FakeWindow::normal();
        w.fail_click_through.set(Some(true));
        let t = turn_on(&w, OVERLAY_ALPHA);
        assert!(!t.active);
        assert!(t.error.unwrap().contains("click-through"));
        assert!(!w.current().click_through && !w.current().layered);
    }

    #[test]
    fn a_failed_opacity_change_undoes_click_through() {
        // Without the undo the window would stay layered without an
        // opacity, which Windows does not draw at all.
        let w = FakeWindow::normal();
        w.fail_opacity.set(true);
        let t = turn_on(&w, OVERLAY_ALPHA);
        assert!(!t.active, "{t:?}");
        assert!(t.error.unwrap().contains("opacity"));
        assert_eq!(
            w.current(),
            NativeState {
                click_through: false,
                alpha: None,
                layered: false
            }
        );
        assert_eq!(
            *w.calls.borrow(),
            [
                "click_through(true)",
                "opacity(166)",
                "click_through(false)"
            ]
        );
    }

    #[test]
    fn an_opacity_that_does_not_take_effect_is_detected_and_undone() {
        let w = FakeWindow::normal();
        w.ignore_opacity.set(true);
        let t = turn_on(&w, OVERLAY_ALPHA);
        assert!(!t.active);
        assert!(t.error.unwrap().contains("did not become translucent"));
        assert!(w.current().is_normal());
    }

    #[test]
    fn if_undoing_fails_the_window_is_reported_as_it_really_is() {
        // Click-through worked, the opacity did not, and removing
        // click-through fails too: the window really is click-through, so
        // overlay mode is reported as active and can be switched off later.
        let w = FakeWindow::normal();
        w.fail_opacity.set(true);
        w.fail_click_through.set(Some(false));
        let t = turn_on(&w, OVERLAY_ALPHA);
        assert!(t.active, "{t:?}");
        let error = t.error.unwrap();
        assert!(
            error.contains("restoring the window also failed"),
            "{error}"
        );

        // Once the native call works again, turning it off recovers.
        w.fail_click_through.set(None);
        assert_eq!(turn_off(&w), ok(false));
        assert!(w.current().is_normal());
    }

    #[test]
    fn a_failed_turn_off_keeps_overlay_active_for_another_try() {
        let w = FakeWindow::normal();
        assert!(turn_on(&w, OVERLAY_ALPHA).active);
        w.fail_click_through.set(Some(false));
        let t = turn_off(&w);
        assert!(t.active, "still click-through, so still reported active");
        assert!(t.error.is_some());
        w.fail_click_through.set(None);
        assert_eq!(turn_off(&w), ok(false));
    }

    #[test]
    fn a_window_that_no_longer_exists_is_never_reported_as_an_overlay() {
        let w = FakeWindow::normal();
        w.destroy();
        let t = turn_on(&w, OVERLAY_ALPHA);
        assert!(!t.active && t.error.is_some());

        let w = FakeWindow::normal();
        assert!(turn_on(&w, OVERLAY_ALPHA).active);
        w.destroy();
        let t = turn_off(&w);
        assert!(!t.active, "nothing is left that could be click-through");
    }

    #[test]
    fn turning_off_restores_full_opacity_if_the_window_stays_layered() {
        // Something other than overlay mode keeps the window layered: it
        // must not be left translucent.
        let w = FakeWindow::normal();
        w.layered_elsewhere.set(true);
        assert_eq!(turn_on(&w, OVERLAY_ALPHA), ok(true));
        assert_eq!(turn_off(&w), ok(false));
        assert_eq!(
            w.current(),
            NativeState {
                click_through: false,
                alpha: Some(u8::MAX),
                layered: true
            },
            "takes the mouse again and is fully opaque"
        );
    }

    #[test]
    fn overlay_mode_needs_a_registered_hotkey() {
        assert_eq!(blocker(&HotkeyStatus::Registered), None);
        assert!(
            blocker(&HotkeyStatus::Unbound)
                .unwrap()
                .contains("Set one on the Hotkeys tab")
        );
        let taken = blocker(&HotkeyStatus::Unavailable("another program uses F9".into())).unwrap();
        assert!(taken.contains("another program uses F9"), "{taken}");
        assert!(
            blocker(&HotkeyStatus::Suspended)
                .unwrap()
                .contains("hotkeys are paused")
        );
    }

    impl NativeState {
        fn is_normal(&self) -> bool {
            !self.click_through && !self.layered && self.alpha.is_none()
        }
    }
}

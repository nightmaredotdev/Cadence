//! GUI integration test: drives the real Slint window on the main thread
//! (`harness = false`). Interactive - it needs an unlocked Windows desktop, so
//! it only runs when `--ignored` is passed:
//!
//! ```text
//! cargo test -p cadence --test gui -- --ignored
//! ```
//!
//! Actions are performed through the same callbacks the controls invoke, and
//! hotkeys are recorded by injecting real key presses into the focused window.
//! Every injected click is swallowed by a low-level mouse hook, and keys are
//! injected only while a window of this process has the foreground. The
//! settings file lives in a temporary directory.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(windows)]
mod imp {
    use cadence::config_store;
    use cadence::gui::{App, GuiOptions};
    use cadence::overlay::OVERLAY_ALPHA;
    use cadence::runtime::{JOIN_TIMEOUT, WaiterKind};
    use cadence::ui::RunStatus;
    use cadence_core::backend::InputBackend;
    use cadence_core::config::{Config, HotkeyAction, HotkeyConfig, TargetMode};
    use cadence_core::engine::ThreadExit;
    use cadence_core::geometry::{Point, Rect};
    use cadence_core::keys::{Hotkey, Key, Modifiers};
    use cadence_platform::diagnostics::{
        HookEvent, MouseHookRecorder, TestWindow, WindowInfo, WindowKind, click_unmarked,
        click_unmarked_button, client_rect, cursor_position, force_foreground, foreground_window,
        monitors, post_close, send_keys, window_at, windows_of_process, work_area_of,
    };
    use cadence_platform::{HotkeyService, WindowsInput};
    use slint::{ComponentHandle, LogicalSize, Model, Timer, TimerMode};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    const VK_CONTROL: u16 = 0x11;
    const VK_SHIFT: u16 = 0x10;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_A: u16 = 0x41;
    const VK_RETURN: u16 = 0x0D;
    const VK_TAB: u16 = 0x09;
    const VK_END: u16 = 0x23;
    const VK_HOME: u16 = 0x24;
    const VK_LEFT: u16 = 0x25;
    const VK_RIGHT: u16 = 0x27;
    const VK_0: u16 = 0x30;
    const VK_1: u16 = 0x31;
    const VK_5: u16 = 0x35;
    const VK_6: u16 = 0x36;
    const VK_7: u16 = 0x37;
    const VK_K: u16 = 0x4B;
    const VK_F13: u16 = 0x7C;
    const VK_F16: u16 = 0x7F;
    const VK_F17: u16 = 0x80;
    const VK_F18: u16 = 0x81;

    /// Runs the Slint event loop for `duration`.
    fn pump(duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            let left = end.saturating_duration_since(Instant::now());
            let t = Timer::default();
            t.start(TimerMode::SingleShot, left, || {
                let _ = slint::quit_event_loop();
            });
            slint::run_event_loop_until_quit().expect("event loop");
        }
    }

    /// Pumps the event loop until `cond` holds (or panics after `timeout`).
    fn pump_until(what: &str, timeout: Duration, cond: impl Fn() -> bool) {
        let end = Instant::now() + timeout;
        while !cond() {
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            pump(Duration::from_millis(20));
        }
    }

    fn step(name: &str) {
        println!("  - {name}");
    }

    fn own_windows() -> Vec<WindowInfo> {
        windows_of_process(std::process::id())
    }

    fn main_window(title_prefix: &str) -> WindowInfo {
        own_windows()
            .into_iter()
            .find(|w| w.visible && w.title.starts_with(title_prefix))
            .unwrap_or_else(|| {
                panic!(
                    "no visible window titled {title_prefix:?}: {:?}",
                    own_windows()
                )
            })
    }

    fn downs(events: &[HookEvent]) -> usize {
        events.iter().filter(|e| e.message.is_down()).count()
    }

    /// A point on the primary monitor that is not over one of this process's
    /// windows: "Pick target" deliberately ignores clicks on its own windows.
    fn point_off_own_windows(offset: Point) -> Point {
        let m = monitors()[0];
        let (w, h) = (m.right - m.left, m.bottom - m.top);
        let own: Vec<_> = own_windows().into_iter().map(|w| w.id).collect();
        [(1, 3), (2, 3), (3, 4), (1, 4)]
            .iter()
            .flat_map(|&(a, b)| [(a, b), (b - a, b)])
            .map(|(a, b)| Point::new(m.left + w * a / b + offset.x, m.top + h * a / b + offset.y))
            .find(|&p| !own.contains(&window_at(p)))
            .expect("a point outside this process's windows")
    }

    /// Button presses and releases (of any origin) seen by the recorder.
    fn button_events(events: &[HookEvent]) -> usize {
        events
            .iter()
            .filter(|e| e.message.is_down() || e.message.is_up())
            .count()
    }

    fn assert_paired(events: &[HookEvent]) {
        for pair in events.chunks(2) {
            assert!(pair[0].message.is_down(), "{events:?}");
            assert!(
                pair.get(1).is_some_and(|e| e.message.is_up()),
                "button left held: {events:?}"
            );
        }
    }

    fn saved(path: &Path) -> Config {
        let text = std::fs::read_to_string(path).expect("settings file exists");
        toml::from_str(&text).expect("settings file parses")
    }

    fn options(path: &Path) -> GuiOptions {
        GuiOptions {
            config_path: Some(path.to_path_buf()),
            waiter: WaiterKind::HighResolution,
            log_dir: None,
        }
    }

    /// Keys are only injected while one of this process's windows is in front.
    fn focus(window: cadence_platform::diagnostics::WindowId) {
        for _ in 0..40 {
            if force_foreground(window) {
                pump(Duration::from_millis(80));
                if foreground_window() == window {
                    return;
                }
            }
            pump(Duration::from_millis(50));
        }
        panic!("inconclusive: could not bring a test-owned window to the foreground");
    }

    fn press(window: cadence_platform::diagnostics::WindowId, keys: &[(u16, bool)]) {
        assert_eq!(
            foreground_window(),
            window,
            "foreground changed; not injecting keys"
        );
        assert_eq!(send_keys(keys), keys.len());
    }

    fn tap(vk: u16) -> Vec<(u16, bool)> {
        vec![(vk, false), (vk, true)]
    }

    struct CursorRestore(Point);
    impl Drop for CursorRestore {
        fn drop(&mut self) {
            let _ = WindowsInput::new().move_cursor(self.0);
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cadence-gui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Hotkeys on F13-F15 so the test never collides with a running
    /// Cadence that uses the defaults.
    fn initial_config() -> Config {
        Config {
            hotkeys: {
                let mut h = HotkeyConfig::none();
                h.toggle = Some(Hotkey::plain(Key::F13));
                h.set_target = Some(Hotkey::plain(Key::F15));
                h.exit = Some(Hotkey::plain(Key::F14));
                h
            },
            ..Config::default()
        }
    }

    /// Registers `key` from a separate hotkey thread, standing in for another
    /// program that already uses the combination.
    fn hold_hotkey(key: Key) -> HotkeyService {
        let taken = Config {
            hotkeys: {
                let mut h = HotkeyConfig::none();
                h.toggle = Some(Hotkey::plain(key));
                h
            },
            ..Config::default()
        };
        let (service, regs) =
            HotkeyService::start(&taken.validate().unwrap().hotkeys, |_| {}).unwrap();
        assert!(regs[0].result.is_ok(), "{key} must be free for the test");
        service
    }

    pub fn main() {
        let interactive = std::env::args().any(|a| a == "--ignored" || a == "--include-ignored");
        if !interactive {
            println!("gui: skipped (needs an interactive desktop; run with --ignored)");
            return;
        }
        println!("gui: running");
        let dir = temp_dir();
        let path = dir.join("config.toml");
        config_store::save(&path, &initial_config()).unwrap();
        let _cursor = CursorRestore(cursor_position());
        let previous_foreground = foreground_window();
        let recorder = MouseHookRecorder::start(true).expect("mouse hook");

        first_session(&path, &recorder);
        second_session(&path);
        compact_and_overlay_session(&dir, &recorder);
        overlay_needs_an_available_hotkey(&dir);
        about_is_an_owned_dialog_centred_over_the_window(&dir);
        startup_conflict_is_shown(&dir);
        obsolete_start_stop_bindings_are_migrated(&dir);
        corrupt_settings_are_reported(&dir);

        force_foreground(previous_foreground);
        let _ = std::fs::remove_dir_all(&dir);
        println!("gui: all checks passed");
    }

    fn first_session(path: &Path, recorder: &MouseHookRecorder) {
        step("launch");
        let app = App::new(options(path)).expect("app starts");
        let ui = app.ui();
        app.show_window();
        pump(Duration::from_millis(500));
        let win = main_window("Cadence");
        assert_eq!(win.title, "Cadence");
        assert!(!win.topmost);
        assert_eq!(ui.get_status(), RunStatus::Stopped);
        assert_eq!(ui.get_status_headline(), "Stopped");
        assert_eq!(ui.get_config_warning(), "");
        assert_eq!(ui.get_toggle_key(), "F13", "shown on the Start/Stop button");
        assert!(ui.get_fixed_mode());
        assert_eq!(ui.get_target_text(), "Not set");
        assert_eq!(ui.get_coordinates_text(), "Coordinates: Not set");

        step("fixed mode without a target explains what to do");
        ui.invoke_start_stop_clicked();
        pump_until("no-target notice", Duration::from_secs(2), || {
            ui.get_notice_text()
                .to_lowercase()
                .contains("no target set")
        });
        assert_eq!(ui.get_status(), RunStatus::Stopped);
        assert_eq!(downs(&recorder.events()), 0);

        step("current-cursor mode: start and stop with the button");
        ui.invoke_mode_selected(false);
        assert!(!ui.get_fixed_mode());
        assert_eq!(app.config().click.mode, TargetMode::Cursor);
        assert_eq!(saved(path).click.mode, TargetMode::Cursor);
        let at = cursor_position();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 3
        });
        pump_until("running state", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Running
        });
        assert_eq!(main_window("Cadence").title, "Cadence - Running");
        ui.invoke_start_stop_clicked();
        pump_until("stopped state", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        assert!(
            events.iter().all(|e| e.point == at),
            "cursor mode must not move the cursor"
        );
        assert_eq!(main_window("Cadence").title, "Cadence");
        recorder.clear();

        // While picking, the recorder (installed before the picker, so it
        // runs after it) records and swallows every mouse event: whatever the
        // picker lets through is visible here and never reaches the desktop.
        recorder.set_record_all(true);
        recorder.clear();

        step("pick target: waits for a click, with no timeout");
        ui.invoke_mode_selected(true);
        assert!(
            ui.get_pick_enabled(),
            "Pick target is enabled in Fixed position mode"
        );
        let target = point_off_own_windows(Point::new(0, 0));
        ui.invoke_pick_target_clicked();
        assert!(ui.get_picking() && app.picking());
        pump(Duration::from_millis(3500)); // longer than the old countdown
        assert!(ui.get_picking(), "picking has no timeout");
        assert_eq!(ui.get_target_text(), "Not set");

        step("pick target: the next left-click sets the position and is swallowed");
        recorder.clear();
        assert_eq!(click_unmarked_button(target, false), 2);
        let expected = format!("{}, {}", target.x, target.y);
        pump_until("target displayed", Duration::from_secs(2), || {
            ui.get_target_text() == expected
        });
        assert!(!ui.get_picking() && !app.picking());
        assert_eq!(
            ui.get_coordinates_text(),
            format!("Coordinates: {expected}")
        );
        assert!(
            !ui.get_notice_text().contains("set to"),
            "a picked position is shown by the coordinates, not a message"
        );
        assert_eq!(app.config().click.target, Some(target));
        assert_eq!(saved(path).click.target, Some(target));
        pump(Duration::from_millis(200));
        assert_eq!(
            button_events(&recorder.events()),
            0,
            "the picking click must not reach anything"
        );
        assert_eq!(app.snapshot().target, Some(target), "engine uses it");

        step("pick target: a right-click cancels and keeps the saved position");
        ui.invoke_pick_target_clicked();
        assert!(ui.get_picking());
        recorder.clear();
        assert_eq!(
            click_unmarked_button(Point::new(target.x + 70, target.y + 50), true),
            2
        );
        pump_until("pick cancelled", Duration::from_secs(2), || {
            !ui.get_picking()
        });
        assert!(
            !ui.get_notice_text().to_lowercase().contains("cancel"),
            "no message for a cancelled pick"
        );
        assert_eq!(ui.get_target_text(), expected);
        assert_eq!(
            ui.get_coordinates_text(),
            format!("Coordinates: {expected}")
        );
        assert_eq!(app.config().click.target, Some(target));
        assert_eq!(saved(path).click.target, Some(target));
        pump(Duration::from_millis(200));
        assert_eq!(
            button_events(&recorder.events()),
            0,
            "the cancelling click must not reach anything (no context menu)"
        );

        step("pick target: the Cancel button ends picking and removes the hook");
        ui.invoke_pick_target_clicked();
        assert!(ui.get_picking());
        ui.invoke_pick_target_clicked(); // now labelled Cancel
        assert!(!ui.get_picking() && !app.picking());
        assert!(!ui.get_notice_text().to_lowercase().contains("cancel"));
        recorder.clear();
        assert_eq!(click_unmarked_button(target, false), 2);
        assert!(
            recorder.wait_for(Duration::from_secs(2), |e| button_events(e) >= 2),
            "after cancelling, clicks are no longer intercepted"
        );
        assert_eq!(app.config().click.target, Some(target));

        step("pick target: the engine's own clicks never pick or cancel");
        ui.invoke_pick_target_clicked();
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("engine clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 3
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        assert!(ui.get_picking(), "still picking after the engine clicked");
        assert_eq!(app.config().click.target, Some(target));
        assert_paired(
            &recorder
                .events()
                .into_iter()
                .filter(|e| e.message.is_down() || e.message.is_up())
                .collect::<Vec<_>>(),
        );

        step("Current position mode: picking is disabled and the position is kept");
        ui.invoke_mode_selected(false);
        assert!(!ui.get_fixed_mode() && !ui.get_pick_enabled());
        assert!(
            !ui.get_picking() && !app.picking(),
            "switching mode cancels"
        );
        assert_eq!(ui.get_target_text(), expected, "still shown");
        assert_eq!(
            ui.get_coordinates_text(),
            format!("Coordinates: {expected}")
        );
        assert_eq!(saved(path).click.target, Some(target), "still saved");
        ui.invoke_pick_target_clicked();
        assert!(!ui.get_picking(), "no picking in Current position mode");
        ui.invoke_mode_selected(true);
        assert!(ui.get_pick_enabled());
        assert_eq!(app.config().click.target, Some(target));
        assert_eq!(ui.get_target_text(), expected);

        recorder.set_record_all(false);
        recorder.clear();

        step("fixed mode: clicks land on the target within the jitter radius");
        WindowsInput::new()
            .move_cursor(Point::new(target.x + 200, target.y + 100))
            .unwrap();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 5
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        for e in events.iter().filter(|e| e.message.is_down()) {
            let (dx, dy) = (e.point.x - target.x, e.point.y - target.y);
            assert!(
                dx * dx + dy * dy <= 9,
                "{:?} too far from {target}",
                e.point
            );
        }
        recorder.clear();

        step("pick a second position");
        let second = point_off_own_windows(Point::new(40, 30));
        recorder.set_record_all(true);
        ui.invoke_pick_target_clicked();
        assert_eq!(click_unmarked_button(second, false), 2);
        let expected = format!("{}, {}", second.x, second.y);
        pump_until("new target displayed", Duration::from_secs(2), || {
            ui.get_target_text() == expected
        });
        assert_eq!(saved(path).click.target, Some(second));
        pump(Duration::from_millis(200));
        recorder.set_record_all(false);
        recorder.clear();

        step("invalid settings are rejected with feedback and never applied");
        let before = saved(path);
        ui.set_interval_text("5".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("outside the allowed range"),
            "{}",
            ui.get_settings_error()
        );
        ui.invoke_settings_commit();
        assert!(
            ui.get_settings_error().starts_with("Not applied"),
            "{}",
            ui.get_settings_error()
        );
        assert_eq!(app.config().click.interval_ms, 100);
        ui.set_interval_text("100".into());
        ui.set_hold_text("80".into());
        ui.invoke_settings_commit();
        assert!(
            ui.get_settings_error().contains("Hold of 80 ms"),
            "{}",
            ui.get_settings_error()
        );
        ui.set_hold_text("abc".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("\"abc\" is not a whole number")
        );
        assert_eq!(saved(path), before, "nothing invalid was saved");

        step("valid settings are applied and saved");
        ui.set_hold_text("10".into());
        ui.set_interval_text("150".into());
        ui.invoke_settings_edited();
        assert!(ui.get_settings_pending());
        ui.invoke_settings_commit();
        assert_eq!(ui.get_settings_error(), "");
        assert!(!ui.get_settings_pending());
        assert_eq!(app.config().click.interval_ms, 150);
        assert_eq!(saved(path).click.interval_ms, 150);

        step("interval unit toggles between ms and CPS without drift");
        assert!(!ui.get_interval_in_cps());
        ui.invoke_interval_unit_clicked();
        assert!(ui.get_interval_in_cps());
        assert_eq!(ui.get_interval_text(), "6.67", "150 ms shown as CPS");
        assert_eq!(
            app.config().click.interval_ms,
            150,
            "toggling changes nothing stored"
        );
        for _ in 0..10 {
            ui.invoke_interval_unit_clicked();
            ui.invoke_interval_unit_clicked();
        }
        assert_eq!(ui.get_interval_text(), "6.67");
        assert_eq!(
            app.config().click.interval_ms,
            150,
            "no drift after repeated toggling"
        );
        // Entering a rate stores the whole-millisecond interval and shows its exact rate.
        ui.set_interval_text("14.3".into());
        ui.invoke_settings_edited();
        ui.invoke_settings_commit();
        assert_eq!(app.config().click.interval_ms, 70);
        assert_eq!(saved(path).click.interval_ms, 70);
        assert_eq!(ui.get_interval_text(), "14.29");
        ui.invoke_interval_unit_clicked();
        assert!(!ui.get_interval_in_cps());
        assert_eq!(ui.get_interval_text(), "70");
        // Limits apply in CPS too, and an invalid entry blocks switching units.
        ui.invoke_interval_unit_clicked();
        ui.set_interval_text("250".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("faster than the maximum of 100 CPS"),
            "{}",
            ui.get_settings_error()
        );
        ui.invoke_interval_unit_clicked();
        assert!(
            ui.get_interval_in_cps(),
            "stays in CPS while the value is invalid"
        );
        assert!(ui.get_settings_error().starts_with("Not applied"));
        assert_eq!(app.config().click.interval_ms, 70);
        // Back to 150 ms for the rest of the test.
        ui.set_interval_text("6.67".into());
        ui.invoke_settings_commit();
        assert_eq!(app.config().click.interval_ms, 150);
        ui.invoke_interval_unit_clicked();
        assert_eq!(ui.get_interval_text(), "150");
        assert_eq!(ui.get_settings_error(), "");

        step("clicking empty space outside an edited field commits it");
        let gui = main_window("Cadence").id;
        let client = client_rect(gui).expect("client area");
        // Inside the window's left padding, outside every card and control.
        let empty = Point::new(client.left + 5, (client.top + client.bottom) / 2);
        let type_interval = |digits: &[u16]| {
            focus(gui);
            ui.invoke_focus_interval_field();
            pump(Duration::from_millis(100));
            let mut keys = vec![
                (VK_CONTROL, false),
                (VK_A, false),
                (VK_A, true),
                (VK_CONTROL, true),
            ];
            for &d in digits {
                keys.extend([(d, false), (d, true)]);
            }
            press(gui, &keys);
        };
        type_interval(&[VK_1, VK_7, VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "175"
        });
        assert!(ui.get_settings_pending());
        assert_eq!(
            app.config().click.interval_ms,
            150,
            "not applied while editing"
        );
        assert_eq!(window_at(empty), gui, "refusing to click: not our window");
        assert_eq!(click_unmarked(empty), 2);
        pump_until(
            "committed by the outside click",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 175,
        );
        assert_eq!(saved(path).click.interval_ms, 175);
        assert_eq!(ui.get_settings_error(), "");
        assert!(!ui.get_settings_pending());

        step("an invalid value is rejected on an outside click; the previous value stays");
        type_interval(&[VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "5"
        });
        assert_eq!(window_at(empty), gui);
        click_unmarked(empty);
        pump_until("rejected", Duration::from_secs(2), || {
            ui.get_settings_error().starts_with("Not applied")
        });
        assert_eq!(app.config().click.interval_ms, 175);
        assert_eq!(saved(path).click.interval_ms, 175);

        step("Enter still commits");
        type_interval(&[VK_1, VK_5, VK_0]);
        press(gui, &tap(VK_RETURN));
        pump_until("committed by Enter", Duration::from_secs(2), || {
            app.config().click.interval_ms == 150
        });
        assert_eq!(ui.get_settings_error(), "");
        let writes_before = std::fs::metadata(path).unwrap().modified().unwrap();
        // Focus leaving the field afterwards commits nothing new (no extra write).
        click_unmarked(empty);
        pump(Duration::from_millis(300));
        assert_eq!(
            std::fs::metadata(path).unwrap().modified().unwrap(),
            writes_before
        );

        step("switching tabs commits a pending edit");
        type_interval(&[VK_1, VK_6, VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "165"
        });
        assert_eq!(app.config().click.interval_ms, 150);
        ui.invoke_release_field_focus(); // what a tab click does first
        ui.set_current_tab(1);
        pump_until(
            "committed by the tab switch",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 165,
        );
        ui.set_current_tab(0);
        pump(Duration::from_millis(100));

        step("keyboard: the tab bar is one Tab stop and arrow keys switch tabs");
        focus(gui);
        ui.invoke_release_field_focus();
        pump(Duration::from_millis(100));
        let mut presses = 0;
        while !ui.get_tab_bar_focused() {
            assert!(presses < 6, "Tab never reached the tab bar");
            press(gui, &tap(VK_TAB));
            pump(Duration::from_millis(80));
            presses += 1;
        }
        assert_eq!(ui.get_current_tab(), 0);
        press(gui, &tap(VK_RIGHT));
        pump_until("Right selects Hotkeys", Duration::from_secs(2), || {
            ui.get_current_tab() == 1
        });
        press(gui, &tap(VK_LEFT));
        pump_until("Left selects Settings", Duration::from_secs(2), || {
            ui.get_current_tab() == 0
        });
        press(gui, &tap(VK_END));
        pump_until("End selects the last tab", Duration::from_secs(2), || {
            ui.get_current_tab() == 1
        });
        press(gui, &tap(VK_HOME));
        pump_until("Home selects the first tab", Duration::from_secs(2), || {
            ui.get_current_tab() == 0
        });
        assert!(
            ui.get_tab_bar_focused(),
            "arrow keys keep the focus on the tab bar"
        );
        press(
            gui,
            &[
                (VK_SHIFT, false),
                (VK_TAB, false),
                (VK_TAB, true),
                (VK_SHIFT, true),
            ],
        );
        pump_until(
            "Shift+Tab leaves the tab bar",
            Duration::from_secs(2),
            || !ui.get_tab_bar_focused(),
        );

        step("keyboard: the mode control is one Tab stop and arrow keys switch modes");
        ui.invoke_release_field_focus();
        pump(Duration::from_millis(100));
        let mut presses = 0;
        while !ui.get_mode_control_focused() {
            assert!(presses < 8, "Tab never reached the mode control");
            press(gui, &tap(VK_TAB));
            pump(Duration::from_millis(80));
            presses += 1;
        }
        assert!(ui.get_fixed_mode());
        press(gui, &tap(VK_RIGHT));
        pump_until(
            "Right selects Current position",
            Duration::from_secs(2),
            || !ui.get_fixed_mode(),
        );
        assert_eq!(saved(path).click.mode, TargetMode::Cursor);
        press(gui, &tap(VK_RIGHT)); // already the last option
        pump(Duration::from_millis(100));
        assert!(!ui.get_fixed_mode());
        press(gui, &tap(VK_LEFT));
        pump_until(
            "Left selects Fixed position",
            Duration::from_secs(2),
            || ui.get_fixed_mode(),
        );
        press(gui, &tap(VK_END));
        pump_until(
            "End selects the last option",
            Duration::from_secs(2),
            || !ui.get_fixed_mode(),
        );
        press(gui, &tap(VK_HOME));
        pump_until(
            "Home selects the first option",
            Duration::from_secs(2),
            || ui.get_fixed_mode(),
        );
        assert!(ui.get_mode_control_focused(), "arrow keys keep the focus");
        assert_eq!(app.config().click.mode, TargetMode::Fixed);
        assert_eq!(app.config().click.target, Some(second), "position kept");
        ui.invoke_release_field_focus();
        pump(Duration::from_millis(100));

        step("keyboard: moving focus to the tab bar commits an edit, then arrows switch");
        type_interval(&[VK_1, VK_5, VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "155"
        });
        let mut presses = 0;
        while !ui.get_tab_bar_focused() {
            assert!(presses < 8, "Shift+Tab never reached the tab bar");
            press(
                gui,
                &[
                    (VK_SHIFT, false),
                    (VK_TAB, false),
                    (VK_TAB, true),
                    (VK_SHIFT, true),
                ],
            );
            pump(Duration::from_millis(80));
            presses += 1;
        }
        pump_until(
            "committed when the field lost focus",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 155,
        );
        press(gui, &tap(VK_RIGHT));
        pump_until("switched by keyboard", Duration::from_secs(2), || {
            ui.get_current_tab() == 1
        });
        press(gui, &tap(VK_LEFT));
        pump_until("back to Settings", Duration::from_secs(2), || {
            ui.get_current_tab() == 0
        });

        step("a button action commits a pending edit first");
        type_interval(&[VK_1, VK_6, VK_0]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "160"
        });
        ui.invoke_about_clicked(); // buttons do not take keyboard focus
        pump_until(
            "committed before the action",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 160,
        );
        // Restore 150 for the rest of the test.
        type_interval(&[VK_1, VK_5, VK_0]);
        press(gui, &tap(VK_RETURN));
        pump_until("restored", Duration::from_secs(2), || {
            app.config().click.interval_ms == 150
        });

        step("position jitter radius: validated, applied by the engine and saved");
        ui.set_radius_text("51".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("51 px exceeds the maximum of 50 px"),
            "{}",
            ui.get_settings_error()
        );
        ui.invoke_settings_commit();
        assert!(ui.get_settings_error().starts_with("Not applied"));
        assert_eq!(app.config().click.position_jitter_radius_px, 3);
        assert_eq!(saved(path).click.position_jitter_radius_px, 3);

        ui.set_radius_text("0".into());
        ui.invoke_settings_commit();
        assert_eq!(ui.get_settings_error(), "");
        assert_eq!(app.config().click.position_jitter_radius_px, 0);
        assert_eq!(saved(path).click.position_jitter_radius_px, 0);
        let target = app.config().click.target.expect("target set earlier");
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 5
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        assert!(
            events
                .iter()
                .filter(|e| e.message.is_down())
                .all(|e| e.point == target),
            "radius 0 must click exactly on the target"
        );

        ui.set_radius_text("8".into());
        ui.invoke_settings_commit();
        assert_eq!(ui.get_settings_error(), "");
        assert_eq!(app.config().click.position_jitter_radius_px, 8);
        assert_eq!(saved(path).click.position_jitter_radius_px, 8);
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(10), || {
            downs(&recorder.events()) >= 30
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        let distances: Vec<i32> = events
            .iter()
            .filter(|e| e.message.is_down())
            .map(|e| {
                let (dx, dy) = (e.point.x - target.x, e.point.y - target.y);
                dx * dx + dy * dy
            })
            .collect();
        assert!(
            distances.iter().all(|&d| d <= 64),
            "outside the 8 px radius: {distances:?}"
        );
        assert!(
            distances.iter().any(|&d| d > 9),
            "the larger radius was not applied (all within 3 px): {distances:?}"
        );
        recorder.clear();

        step("always on top is applied to the real window and saved");
        ui.set_keep_on_top(true);
        ui.invoke_keep_on_top_toggled(true);
        pump_until("topmost", Duration::from_secs(2), || {
            main_window("Cadence").topmost
        });
        assert!(saved(path).window.always_on_top);
        ui.set_keep_on_top(false);
        ui.invoke_keep_on_top_toggled(false);
        pump_until("not topmost", Duration::from_secs(2), || {
            !main_window("Cadence").topmost
        });
        assert!(!saved(path).window.always_on_top);

        let gui = main_window("Cadence").id;
        let index =
            |a: HotkeyAction| HotkeyAction::ALL.iter().position(|x| *x == a).unwrap() as i32;
        let binding = |a: HotkeyAction| {
            ui.get_hotkeys()
                .row_data(index(a) as usize)
                .unwrap()
                .binding
                .to_string()
        };

        step("only the four hotkey actions are offered");
        let labels: Vec<String> = ui
            .get_hotkeys()
            .iter()
            .map(|r| r.label.to_string())
            .collect();
        assert_eq!(
            labels,
            ["Toggle start/stop", "Set target", "Toggle overlay", "Exit"]
        );

        step("record a Ctrl+Shift+letter hotkey by pressing it");
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::Exit));
        pump(Duration::from_millis(100));
        assert!(
            !app.hotkeys_active(),
            "hotkeys are suspended while recording"
        );
        press(
            gui,
            &[
                (VK_CONTROL, false),
                (VK_SHIFT, false),
                (VK_K, false),
                (VK_K, true),
                (VK_SHIFT, true),
                (VK_CONTROL, true),
            ],
        );
        let ctrl_shift_k = Hotkey::new(
            Modifiers {
                ctrl: true,
                alt: false,
                shift: true,
            },
            Key::K,
        );
        pump_until("binding recorded", Duration::from_secs(2), || {
            app.config().hotkeys.exit == Some(ctrl_shift_k)
        });
        assert_eq!(ui.get_capturing_index(), -1);
        assert!(app.hotkeys_active());
        assert_eq!(binding(HotkeyAction::Exit), "Ctrl+Shift+K");
        assert_eq!(saved(path).hotkeys.exit, Some(ctrl_shift_k));

        step("a key that would block typing is refused; Esc cancels");
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::SetTarget));
        pump(Duration::from_millis(100));
        press(gui, &tap(VK_A));
        pump_until("refusal shown", Duration::from_secs(2), || {
            ui.get_hotkeys_error().contains("would block normal typing")
        });
        assert_eq!(
            ui.get_capturing_index(),
            index(HotkeyAction::SetTarget),
            "still recording"
        );
        press(gui, &tap(VK_ESCAPE));
        pump_until("recording cancelled", Duration::from_secs(2), || {
            ui.get_capturing_index() == -1
        });
        assert_eq!(
            app.config().hotkeys.set_target,
            Some(Hotkey::plain(Key::F15))
        );
        assert!(app.hotkeys_active());

        step("a combination held by another program cannot even be recorded");
        // Windows delivers a registered combination to its owner only, so the
        // key press never reaches this window: recording simply continues.
        let other = hold_hotkey(Key::F17);
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::SetTarget));
        pump(Duration::from_millis(100));
        press(gui, &tap(VK_F17));
        pump(Duration::from_millis(500));
        assert_eq!(ui.get_capturing_index(), index(HotkeyAction::SetTarget));
        assert_eq!(
            app.config().hotkeys.set_target,
            Some(Hotkey::plain(Key::F15))
        );
        press(gui, &tap(VK_ESCAPE));
        pump_until("recording cancelled", Duration::from_secs(2), || {
            ui.get_capturing_index() == -1
        });
        assert_eq!(binding(HotkeyAction::SetTarget), "F15");
        assert!(app.hotkeys_active());
        assert_eq!(other.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);

        step("a function key is recorded");
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::SetTarget));
        pump(Duration::from_millis(100));
        press(gui, &tap(VK_F16));
        pump_until("F16 recorded", Duration::from_secs(2), || {
            app.config().hotkeys.set_target == Some(Hotkey::plain(Key::F16))
        });

        step("clear a binding");
        ui.invoke_hotkey_clear(index(HotkeyAction::SetTarget));
        assert_eq!(app.config().hotkeys.set_target, None);
        assert_eq!(binding(HotkeyAction::SetTarget), "Not set");

        step("global hotkey works while the GUI is not focused");
        recorder.clear();
        let m = monitors()[0];
        let r = Rect::new(m.right - 260, m.bottom - 260, m.right - 60, m.bottom - 60);
        let other_window =
            TestWindow::create(r, WindowKind::TopmostPopup, "unfocused test").unwrap();
        focus(other_window.id());
        assert_ne!(foreground_window(), gui);
        press(other_window.id(), &tap(VK_F13));
        pump_until("running via hotkey", Duration::from_secs(3), || {
            ui.get_status() == RunStatus::Running && downs(&recorder.events()) >= 2
        });
        press(other_window.id(), &tap(VK_F13));
        pump_until("stopped via hotkey", Duration::from_secs(3), || {
            ui.get_status() == RunStatus::Stopped
        });
        drop(other_window);

        step("closing the window hides it to the tray; clicking continues");
        let tray = app.tray().expect("notification-area icon");
        assert!(tray.get_window_visible());
        assert!(!tray.get_running());
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicking", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 2
        });
        assert!(tray.get_running(), "tray reflects the running state");
        assert!(post_close(main_window("Cadence").id), "WM_CLOSE posted");
        pump_until("hidden", Duration::from_secs(2), || !app.window_visible());
        assert!(
            !own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("Cadence")),
            "main window hidden"
        );
        assert!(!tray.get_window_visible());
        let before = downs(&recorder.events());
        pump(Duration::from_millis(500));
        assert!(
            downs(&recorder.events()) > before,
            "still clicking while hidden"
        );
        assert_eq!(ui.get_status(), RunStatus::Running);

        step("global hotkey works while hidden");
        let other_window = TestWindow::create(r, WindowKind::TopmostPopup, "hidden test").unwrap();
        focus(other_window.id());
        press(other_window.id(), &tap(VK_F13));
        pump_until(
            "stopped via hotkey while hidden",
            Duration::from_secs(3),
            || ui.get_status() == RunStatus::Stopped,
        );
        assert!(!tray.get_running());
        drop(other_window);

        step("tray: start and stop clicking while hidden");
        recorder.clear();
        tray.invoke_start_stop();
        pump_until("running via tray", Duration::from_secs(3), || {
            tray.get_running() && downs(&recorder.events()) >= 2
        });
        tray.invoke_start_stop();
        pump_until("stopped via tray", Duration::from_secs(3), || {
            !tray.get_running()
        });
        pump(Duration::from_millis(200));
        assert_paired(&recorder.events());

        step("tray: show restores a usable window");
        tray.invoke_toggle_window();
        let titled = || {
            own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("Cadence"))
        };
        pump_until("shown with its title", Duration::from_secs(3), || {
            app.window_visible() && titled()
        });
        let shown = main_window("Cadence");
        assert_eq!(shown.title, "Cadence");
        assert!(tray.get_window_visible());
        assert_eq!(ui.get_status(), RunStatus::Stopped);
        tray.invoke_toggle_window();
        pump_until("hidden again", Duration::from_secs(2), || {
            !app.window_visible()
        });
        tray.invoke_show_window(); // left click on the icon
        pump_until("shown by click", Duration::from_secs(3), || {
            app.window_visible() && titled()
        });

        step("about dialog with Slint attribution");
        ui.invoke_about_clicked();
        pump_until("about window", Duration::from_secs(2), || {
            own_windows()
                .iter()
                .any(|w| w.visible && w.title == "About Cadence")
        });

        step("tray Exit while clicking stops the engine and shuts down completely");
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 2
        });
        tray.invoke_exit();
        let report = app.shutdown_report().expect("tray Exit shut down");
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(app.close(), Some(report), "close is idempotent");
        pump(Duration::from_millis(300));
        let count = recorder.events().len();
        pump(Duration::from_millis(300));
        let events = recorder.events();
        assert_eq!(events.len(), count, "no clicks after close");
        assert_paired(&events);
        assert!(
            !own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("Cadence")),
            "main window hidden"
        );
        assert!(
            !own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("About"))
        );
        // Hotkeys were unregistered: they can be registered by someone else now.
        let free = initial_config().validate().unwrap().hotkeys;
        let (probe, regs) = HotkeyService::start(&free, |_| {}).unwrap();
        assert!(regs.iter().all(|r| r.result.is_ok()), "{regs:?}");
        assert_eq!(probe.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
        drop(app);
    }

    fn second_session(path: &Path) {
        step("settings persist across restarts");
        let app = App::new(options(path)).expect("app restarts");
        let ui = app.ui();
        app.show_window();
        pump(Duration::from_millis(300));
        assert_eq!(ui.get_config_warning(), "");
        assert_eq!(ui.get_interval_text(), "150");
        assert_eq!(ui.get_radius_text(), "8");
        assert_eq!(app.config().click.position_jitter_radius_px, 8);
        assert!(ui.get_fixed_mode());
        assert!(!ui.get_keep_on_top());
        assert_ne!(ui.get_target_text(), "Not set");
        assert_eq!(app.snapshot().target, app.config().click.target);
        let rows = ui.get_hotkeys();
        assert_eq!(rows.row_count(), 4);
        assert_eq!(rows.row_data(3).unwrap().binding, "Ctrl+Shift+K", "exit");
        assert_eq!(rows.row_data(2).unwrap().binding, "Not set", "overlay");
        assert_eq!(rows.row_data(1).unwrap().binding, "Not set", "set target");
        assert!(app.close().unwrap().is_clean());
        drop(app);
    }

    fn hotkey_index(action: HotkeyAction) -> i32 {
        HotkeyAction::ALL.iter().position(|a| *a == action).unwrap() as i32
    }

    /// The middle of the main window's client area.
    fn center_of(window: cadence_platform::diagnostics::WindowId) -> Point {
        let r = client_rect(window).expect("client area");
        Point::new((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    fn size_of(r: Rect) -> (i32, i32) {
        (r.right - r.left, r.bottom - r.top)
    }

    /// Asserts that overlay mode is active, verified natively.
    fn assert_overlay_on(app: &App) {
        assert!(app.overlay_active());
        assert!(app.ui().get_overlay_active(), "the window shows it");
        assert!(
            app.tray().unwrap().get_overlay_active(),
            "the tray shows it"
        );
        let s = app.native_overlay_styles().expect("native state");
        assert!(s.click_through && s.layered, "{s:?}");
        assert_eq!(s.alpha, Some(OVERLAY_ALPHA), "{s:?}");
    }

    /// Asserts that the window is back to normal, verified natively.
    fn assert_overlay_off(app: &App) {
        assert!(!app.overlay_active());
        assert!(!app.ui().get_overlay_active());
        assert!(!app.tray().unwrap().get_overlay_active());
        let s = app.native_overlay_styles().expect("native state");
        assert!(s.is_normal(), "{s:?}");
    }

    /// Overlay mode is off for a hidden window. Between test steps Slint may
    /// drop a hidden window's native window (see the tray step below); a
    /// window that does not exist cannot be click-through either.
    fn assert_overlay_off_while_hidden(app: &App) {
        assert!(!app.overlay_active());
        assert!(!app.ui().get_overlay_active());
        assert!(!app.tray().unwrap().get_overlay_active());
        if let Ok(s) = app.native_overlay_styles() {
            assert!(s.is_normal(), "{s:?}");
        }
    }

    fn compact_and_overlay_session(dir: &Path, recorder: &MouseHookRecorder) {
        let path = dir.join("overlay.toml");
        let mut cfg = initial_config();
        cfg.click.mode = TargetMode::Cursor;
        cfg.hotkeys.overlay = Some(Hotkey::plain(Key::F18));
        config_store::save(&path, &cfg).unwrap();

        step("compact layout: launch in the full layout");
        let app = App::new(options(&path)).expect("app starts");
        let ui = app.ui();
        let tray = app.tray().expect("notification-area icon");
        app.show_window();
        pump(Duration::from_millis(500));
        let gui = main_window("Cadence").id;
        assert!(!ui.get_compact_mode());
        assert_eq!(ui.get_overlay_key(), "F18", "shown on the Overlay button");
        let full = size_of(client_rect(gui).unwrap());
        assert_overlay_off(&app);

        step("compact layout: a small window with status, Start/Stop and a summary");
        ui.invoke_compact_toggled();
        pump_until("compact window", Duration::from_secs(3), || {
            size_of(client_rect(gui).unwrap()).1 < full.1 / 2
        });
        pump(Duration::from_millis(200));
        let compact = size_of(client_rect(gui).unwrap());
        assert!(ui.get_compact_mode());
        assert!(compact.0 < full.0, "narrower: {compact:?} vs {full:?}");
        assert_eq!(
            compact.1 as f32,
            (ui.get_layout_preferred_height() * ui.window().scale_factor()).round(),
            "exactly as tall as the compact layout"
        );
        assert_eq!(ui.get_summary_text(), "Current position · 100 ms");
        assert_eq!(ui.get_toggle_key(), "F13", "Start/Stop shows its hotkey");
        assert!(saved(&path).window.compact, "the layout is saved");

        step("compact layout: switching while clicking keeps clicking and the settings");
        let settings = app.config();
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 3
        });
        let clicks = app.snapshot().run_clicks;
        ui.invoke_compact_toggled();
        pump_until("full window", Duration::from_secs(3), || {
            size_of(client_rect(gui).unwrap()) == full
        });
        assert!(!ui.get_compact_mode());
        assert!(!saved(&path).window.compact);
        let before = downs(&recorder.events());
        pump(Duration::from_millis(400));
        assert!(downs(&recorder.events()) > before, "still clicking");
        assert_eq!(ui.get_status(), RunStatus::Running);
        assert!(
            app.snapshot().run_clicks > clicks,
            "the same run continues (not restarted)"
        );
        assert_eq!(
            app.config().click,
            settings.click,
            "click settings unchanged"
        );
        assert_eq!(app.config().hotkeys, settings.hotkeys, "hotkeys unchanged");
        assert_eq!(ui.get_interval_text(), "100", "fields kept");
        assert_eq!(ui.get_hotkeys().row_count(), 4);

        step("overlay: translucent, click-through and in front; clicking goes on");
        ui.invoke_compact_toggled();
        pump_until("compact again", Duration::from_secs(3), || {
            size_of(client_rect(gui).unwrap()) == compact
        });
        assert!(!main_window("Cadence").topmost);
        ui.invoke_overlay_clicked();
        assert_overlay_on(&app);
        pump_until("always on top", Duration::from_secs(2), || {
            main_window("Cadence").topmost
        });
        pump(Duration::from_millis(300));
        // Making it topmost rewrites the window styles: still an overlay.
        assert_overlay_on(&app);
        assert_eq!(
            size_of(client_rect(gui).unwrap()),
            compact,
            "overlay mode keeps the window size"
        );
        let center = center_of(gui);
        assert_ne!(
            window_at(center),
            gui,
            "the mouse passes through the window"
        );
        assert!(!app.config().window.always_on_top, "the setting is kept");
        let before = downs(&recorder.events());
        pump(Duration::from_millis(400));
        assert!(downs(&recorder.events()) > before, "still clicking");

        step("overlay: the hotkey ends it while another program has the focus");
        let m = monitors()[0];
        let r = Rect::new(m.right - 260, m.bottom - 260, m.right - 60, m.bottom - 60);
        let other = TestWindow::create(r, WindowKind::TopmostPopup, "overlay keys").unwrap();
        focus(other.id());
        press(other.id(), &tap(VK_F18));
        pump_until("overlay off by hotkey", Duration::from_secs(3), || {
            !app.overlay_active()
        });
        assert_overlay_off(&app);
        pump_until("brought to the front", Duration::from_secs(2), || {
            foreground_window() == gui
        });
        pump_until("not on top any more", Duration::from_secs(2), || {
            !main_window("Cadence").topmost
        });
        assert_eq!(ui.get_status(), RunStatus::Running, "clicking goes on");

        step("overlay: the hotkey turns it on without taking the focus");
        focus(other.id());
        press(other.id(), &tap(VK_F18));
        pump_until("overlay on by hotkey", Duration::from_secs(3), || {
            app.overlay_active()
        });
        assert_overlay_on(&app);
        pump(Duration::from_millis(300));
        assert_eq!(
            foreground_window(),
            other.id(),
            "the program in use keeps the focus"
        );

        step("overlay: the keyboard cannot pause or clear its hotkey");
        ui.invoke_start_recording(hotkey_index(HotkeyAction::Overlay));
        pump(Duration::from_millis(100));
        assert_eq!(ui.get_capturing_index(), -1, "recording refused");
        assert!(app.hotkeys_active());
        ui.invoke_hotkey_clear(hotkey_index(HotkeyAction::Overlay));
        assert_eq!(app.config().hotkeys.overlay, Some(Hotkey::plain(Key::F18)));
        assert_overlay_on(&app);

        step("overlay: the tray turns it off and on");
        tray.invoke_toggle_overlay();
        assert_overlay_off(&app);
        tray.invoke_toggle_overlay();
        assert_overlay_on(&app);

        step("overlay: stopping and starting clicking from the tray keeps it on");
        tray.invoke_start_stop();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        assert_overlay_on(&app);
        recorder.clear();
        tray.invoke_start_stop();
        pump_until("clicking again", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 2
        });
        assert_overlay_on(&app);

        step("overlay: closing the window ends it; the hotkey does nothing while hidden");
        assert!(post_close(gui), "WM_CLOSE posted");
        pump_until("hidden", Duration::from_secs(2), || !app.window_visible());
        assert_overlay_off_while_hidden(&app);
        focus(other.id());
        press(other.id(), &tap(VK_F18));
        pump(Duration::from_millis(500));
        assert!(!app.window_visible(), "not shown by the hotkey");
        assert_overlay_off_while_hidden(&app);
        assert_eq!(foreground_window(), other.id(), "focus not taken");

        step("overlay: the tray item shows the window; the icon ends overlay mode");
        // In the application the hidden window keeps its native window, so
        // the tray item turns overlay mode on at once. This test leaves the
        // event loop between steps, and Slint then drops the native window of
        // a hidden window, which comes back only once the loop runs again. So
        // here the item shows the window (and reports, safely, that overlay
        // mode could not be turned on yet); it is then turned on from the
        // tray as usual.
        tray.invoke_toggle_overlay();
        pump_until("shown", Duration::from_secs(3), || {
            app.window_visible() && app.native_overlay_styles().is_ok()
        });
        if !app.overlay_active() {
            assert_overlay_off(&app);
            tray.invoke_toggle_overlay();
        }
        assert_overlay_on(&app);
        tray.invoke_show_window(); // left click on the icon
        pump_until("normal window", Duration::from_secs(2), || {
            !app.overlay_active()
        });
        assert_overlay_off(&app);
        assert!(app.window_visible());
        assert!(ui.get_compact_mode(), "still compact");
        drop(other);

        step("overlay: exiting while it is on shuts down cleanly");
        ui.invoke_overlay_clicked();
        assert_overlay_on(&app);
        tray.invoke_exit();
        let report = app.shutdown_report().expect("shut down");
        assert!(report.is_clean(), "{report:?}");
        assert!(!app.overlay_active());
        pump(Duration::from_millis(300));
        let count = recorder.events().len();
        pump(Duration::from_millis(300));
        assert_eq!(recorder.events().len(), count, "no clicks after exit");
        assert_paired(&recorder.events());
        drop(app);

        step("compact layout: restored at the next start");
        let app = App::new(options(&path)).expect("app restarts");
        app.show_window();
        pump(Duration::from_millis(500));
        assert!(app.ui().get_compact_mode());
        let gui = main_window("Cadence").id;
        assert_eq!(size_of(client_rect(gui).unwrap()), compact);
        assert_overlay_off(&app);

        step("compact layout: the full layout opens at its usual size");
        app.ui().invoke_compact_toggled();
        pump_until("full window", Duration::from_secs(3), || {
            size_of(client_rect(gui).unwrap()) == full
        });
        assert!(app.close().unwrap().is_clean());
        drop(app);
        recorder.clear();
    }

    fn about_window() -> Option<WindowInfo> {
        own_windows()
            .into_iter()
            .find(|w| w.visible && w.title == "About Cadence")
    }

    fn size_of_rect(r: Rect) -> (i32, i32) {
        (r.right - r.left, r.bottom - r.top)
    }

    /// Where the About window belongs: centred over `main`, inside the work
    /// area of its monitor.
    fn expected_about(main: &WindowInfo, about: &WindowInfo) -> Point {
        cadence::dialog::centered_over(main.rect, size_of_rect(about.rect), work_area_of(main.id))
    }

    fn near(a: Point, b: Point, tolerance: i32) -> bool {
        (a.x - b.x).abs() <= tolerance && (a.y - b.y).abs() <= tolerance
    }

    /// One part of a scenario run inside a single run of the event loop.
    enum Step {
        /// A heading, printed like the other steps.
        Say(&'static str),
        /// Runs once (and may assert).
        Act(Box<dyn FnMut()>),
        /// Polled until it holds; fails after the timeout.
        Wait(&'static str, Duration, Box<dyn FnMut() -> bool>),
    }

    fn act(f: impl FnMut() + 'static) -> Step {
        Step::Act(Box::new(f))
    }

    fn wait_for(what: &'static str, f: impl FnMut() -> bool + 'static) -> Step {
        Step::Wait(what, Duration::from_secs(3), Box::new(f))
    }

    /// Lets the event loop run for `duration` (repaints, window messages).
    fn settle(duration: Duration) -> Step {
        let mut since: Option<Instant> = None;
        Step::Wait(
            "settling",
            duration * 2,
            Box::new(move || since.get_or_insert_with(Instant::now).elapsed() >= duration),
        )
    }

    fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "a step panicked".into())
    }

    /// Runs `steps` in order inside one run of the event loop, as in the
    /// application. Unlike `pump`, the loop is not stopped between steps:
    /// stopping it lets Slint re-create the native windows of hidden windows,
    /// which never happens while the application runs. A failed assertion
    /// or a timeout ends the loop and fails the test with its message.
    fn run_in_one_event_loop(steps: Vec<Step>) {
        struct Run {
            steps: std::vec::IntoIter<Step>,
            current: Option<(Step, Instant)>,
            failure: Option<String>,
            finished: bool,
        }
        let run = Rc::new(RefCell::new(Run {
            steps: steps.into_iter(),
            current: None,
            failure: None,
            finished: false,
        }));
        let timer = Timer::default();
        let state = run.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(20), move || {
            let mut run = state.borrow_mut();
            while !run.finished {
                if run.current.is_none() {
                    match run.steps.next() {
                        Some(next) => run.current = Some((next, Instant::now())),
                        None => {
                            run.finished = true;
                            let _ = slint::quit_event_loop();
                            return;
                        }
                    }
                }
                let Some((current, started)) = run.current.as_mut() else {
                    return;
                };
                let started = *started;
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match current {
                        Step::Say(name) => {
                            step(name);
                            Ok(true)
                        }
                        Step::Act(f) => {
                            f();
                            Ok(true)
                        }
                        Step::Wait(what, timeout, f) => {
                            if f() {
                                Ok(true)
                            } else if started.elapsed() > *timeout {
                                Err(format!("timed out waiting for: {what}"))
                            } else {
                                Ok(false)
                            }
                        }
                    }));
                match outcome {
                    Ok(Ok(true)) => run.current = None,
                    Ok(Ok(false)) => return, // polled again on the next tick
                    Ok(Err(failure)) => run.failure = Some(failure),
                    Err(panic) => run.failure = Some(panic_message(panic.as_ref())),
                }
                if run.failure.is_some() {
                    run.finished = true;
                    let _ = slint::quit_event_loop();
                }
            }
        });
        slint::run_event_loop_until_quit().expect("event loop");
        drop(timer);
        let mut run = run.borrow_mut();
        if let Some(failure) = run.failure.take() {
            panic!("{failure}");
        }
        assert!(
            run.steps.next().is_none() && run.current.is_none(),
            "the event loop ended before the scenario finished"
        );
    }

    /// What the About scenario remembers between its steps.
    #[derive(Default)]
    struct AboutRun {
        /// The native About window, from its first opening.
        about: Option<cadence_platform::diagnostics::WindowId>,
        /// The main window's rectangle before About was opened.
        main: Option<Rect>,
    }

    fn record_main(state: &Rc<RefCell<AboutRun>>) -> Step {
        let state = state.clone();
        act(move || state.borrow_mut().main = Some(main_window("Cadence").rect))
    }

    /// Opens About and checks it: owned by the main window, the same native
    /// window as at its first opening, centred over the main window. The
    /// first position is set before the window exists, from its expected
    /// size, so a few pixels of frame difference are allowed then.
    fn open_about(app: &Rc<App>, state: &Rc<RefCell<AboutRun>>, tolerance: i32) -> Vec<Step> {
        let (app, state) = (app.clone(), state.clone());
        vec![
            act(move || app.ui().invoke_about_clicked()),
            wait_for("the About window", || about_window().is_some()),
            settle(Duration::from_millis(250)),
            act(move || {
                let (main, about) = (main_window("Cadence"), about_window().unwrap());
                assert_eq!(about.owner, Some(main.id), "owned by the main window");
                assert!(!about.click_through, "About takes the mouse");
                let mut state = state.borrow_mut();
                match state.about {
                    None => state.about = Some(about.id),
                    Some(first) => assert_eq!(
                        about.id, first,
                        "the same native About window as at its first opening"
                    ),
                }
                let at = Point::new(about.rect.left, about.rect.top);
                let expected = expected_about(&main, &about);
                assert!(
                    near(at, expected, tolerance),
                    "About at {at}, expected {expected}"
                );
            }),
        ]
    }

    /// Closes About (as its close button does) and checks that the main
    /// window kept the size and position it had before About was opened.
    fn close_about(state: &Rc<RefCell<AboutRun>>) -> Vec<Step> {
        let state = state.clone();
        vec![
            act(|| assert!(post_close(about_window().unwrap().id), "WM_CLOSE posted")),
            wait_for("About closed", || about_window().is_none()),
            act(move || {
                let expected = state.borrow().main.expect("recorded");
                assert_eq!(
                    main_window("Cadence").rect,
                    expected,
                    "main window unchanged"
                );
            }),
        ]
    }

    fn about_centred() -> bool {
        match (
            about_window(),
            own_windows()
                .into_iter()
                .find(|w| w.visible && w.title.starts_with("Cadence")),
        ) {
            (Some(about), Some(main)) => {
                Point::new(about.rect.left, about.rect.top) == expected_about(&main, &about)
            }
            _ => false,
        }
    }

    /// About above the main window, taking the mouse at its centre.
    fn about_on_top(about: &WindowInfo) {
        let r = about.rect;
        let center = Point::new((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        assert_eq!(
            window_at(center),
            about.id,
            "About is in front, under the mouse"
        );
    }

    fn about_is_an_owned_dialog_centred_over_the_window(dir: &Path) {
        let path = dir.join("about.toml");
        let mut cfg = initial_config();
        cfg.click.mode = TargetMode::Cursor;
        cfg.hotkeys.overlay = Some(Hotkey::plain(Key::F18));
        config_store::save(&path, &cfg).unwrap();
        let app = Rc::new(App::new(options(&path)).expect("app starts"));
        app.show_window();
        let state = Rc::new(RefCell::new(AboutRun::default()));
        let a = &app;
        let mut steps = vec![
            Step::Say(
                "about (in one event loop): the first opening creates an owned, centred window",
            ),
            wait_for("the main window", || {
                own_windows()
                    .iter()
                    .any(|w| w.visible && w.title.starts_with("Cadence"))
            }),
            settle(Duration::from_millis(400)),
            record_main(&state),
        ];
        steps.extend(open_about(a, &state, 4));
        steps.push(act(|| {
            assert!(!main_window("Cadence").topmost);
            assert!(
                !about_window().unwrap().topmost,
                "not always on top on its own"
            );
        }));
        steps.extend(close_about(&state));

        steps.push(Step::Say(
            "about: reopened several times, still the same owned window",
        ));
        for _ in 0..3 {
            steps.extend(open_about(a, &state, 1));
            steps.extend(close_about(&state));
        }

        steps.push(Step::Say(
            "about: follows the main window when it moves and is resized",
        ));
        steps.extend(open_about(a, &state, 1));
        let moved = Rc::new(std::cell::Cell::new(0));
        steps.push(act({
            let (app, moved) = (app.clone(), moved.clone());
            move || {
                let r = main_window("Cadence").rect;
                moved.set(r.left + 90);
                app.ui()
                    .window()
                    .set_position(slint::PhysicalPosition::new(r.left + 90, r.top + 40));
            }
        }));
        steps.push(wait_for("the main window moved", move || {
            main_window("Cadence").rect.left == moved.get()
        }));
        steps.push(wait_for("About centred after the move", about_centred));
        let wider = Rc::new(std::cell::Cell::new(0));
        steps.push(act({
            let (app, wider) = (app.clone(), wider.clone());
            move || {
                let (w, h) = size_of_rect(main_window("Cadence").rect);
                wider.set(w);
                let scale = app.ui().window().scale_factor();
                app.ui().window().set_size(LogicalSize::new(
                    (w as f32 + 60.0) / scale,
                    (h as f32 + 40.0) / scale,
                ));
            }
        }));
        steps.push(wait_for("the main window resized", move || {
            size_of_rect(main_window("Cadence").rect).0 > wider.get()
        }));
        steps.push(wait_for("About centred after the resize", about_centred));

        steps.push(Step::Say(
            "about: hidden with the minimized main window, back when restored",
        ));
        steps.push(act({
            let app = app.clone();
            move || app.ui().window().set_minimized(true)
        }));
        steps.push(wait_for("About hidden with its owner", || {
            about_window().is_none()
        }));
        steps.push(act({
            let app = app.clone();
            move || app.ui().window().set_minimized(false)
        }));
        steps.push(wait_for("About back", || about_window().is_some()));
        steps.push(wait_for("About centred after restoring", about_centred));
        steps.push(act({
            let state = state.clone();
            move || {
                let about = about_window().unwrap();
                assert_eq!(about.owner, Some(main_window("Cadence").id));
                assert_eq!(
                    Some(about.id),
                    state.borrow().about,
                    "the same native About window"
                );
            }
        }));
        steps.push(record_main(&state));
        steps.extend(close_about(&state));

        // The compact layout has no About button: About is open there when it
        // was opened in the full layout and the window is then made compact.
        steps.push(Step::Say(
            "about: switching to the compact layout with About open, several times",
        ));
        for _ in 0..3 {
            steps.extend(open_about(a, &state, 1));
            steps.push(act({
                let app = app.clone();
                move || app.ui().invoke_compact_toggled()
            }));
            steps.push(wait_for("the compact window", {
                let app = app.clone();
                move || {
                    app.ui().get_compact_mode() && size_of_rect(main_window("Cadence").rect).1 < 300
                }
            }));
            steps.push(wait_for(
                "About centred over the compact window",
                about_centred,
            ));
            steps.push(act({
                let state = state.clone();
                move || {
                    let about = about_window().expect("About still open");
                    assert_eq!(about.owner, Some(main_window("Cadence").id));
                    assert_eq!(
                        Some(about.id),
                        state.borrow().about,
                        "the same About window"
                    );
                }
            }));
            steps.push(settle(Duration::from_millis(200)));
            steps.push(record_main(&state));
            steps.extend(close_about(&state));
            steps.push(act({
                let app = app.clone();
                move || app.ui().invoke_compact_toggled()
            }));
            steps.push(wait_for("the full window", {
                let app = app.clone();
                move || {
                    !app.ui().get_compact_mode()
                        && size_of_rect(main_window("Cadence").rect).1 > 400
                }
            }));
            steps.push(settle(Duration::from_millis(200)));
            steps.push(record_main(&state));
        }

        steps.push(Step::Say(
            "about: usable above the overlay; closing it keeps overlay mode on",
        ));
        steps.push(act({
            let app = app.clone();
            move || {
                app.ui().invoke_overlay_clicked();
                assert!(app.overlay_active(), "overlay mode before About");
                app.ui().invoke_overlay_clicked();
                assert!(!app.overlay_active());
            }
        }));
        steps.extend(open_about(a, &state, 1));
        steps.push(act({
            let app = app.clone();
            move || {
                app.ui().invoke_overlay_clicked();
                assert!(app.overlay_active());
                let styles = app.native_overlay_styles().unwrap();
                assert!(styles.click_through, "{styles:?}");
            }
        }));
        steps.push(settle(Duration::from_millis(300)));
        steps.push(act(|| {
            let about = about_window().expect("About still open");
            assert!(!about.click_through, "About takes the mouse");
            assert_eq!(about.owner, Some(main_window("Cadence").id));
            about_on_top(&about);
        }));
        steps.extend(close_about(&state));
        steps.push(act({
            let app = app.clone();
            move || {
                assert!(app.overlay_active(), "still in overlay mode");
                let styles = app.native_overlay_styles().unwrap();
                assert!(styles.click_through && styles.alpha.is_some(), "{styles:?}");
                app.tray().unwrap().invoke_toggle_overlay();
                assert!(!app.overlay_active());
                assert!(app.native_overlay_styles().unwrap().is_normal());
                app.ui().invoke_overlay_clicked();
                assert!(app.overlay_active(), "overlay mode after About");
                app.ui().invoke_overlay_clicked();
                assert!(!app.overlay_active());
            }
        }));

        steps.push(Step::Say(
            "about: showing the window again (tray, second start) keeps About with it",
        ));
        steps.extend(open_about(a, &state, 1));
        steps.push(act({
            let app = app.clone();
            move || {
                app.ui().invoke_show_requested(); // what a second start of Cadence does
                app.tray().unwrap().invoke_show_window(); // a click on the tray icon
            }
        }));
        steps.push(settle(Duration::from_millis(400)));
        steps.push(act(|| {
            let about = about_window().expect("About still open");
            assert_eq!(about.owner, Some(main_window("Cadence").id));
            about_on_top(&about);
        }));
        steps.extend(close_about(&state));

        steps.push(Step::Say(
            "about: hiding the main window to the tray hides About too",
        ));
        steps.extend(open_about(a, &state, 1));
        steps.push(act(|| {
            assert!(post_close(main_window("Cadence").id), "WM_CLOSE posted")
        }));
        steps.push(wait_for("both hidden", {
            let app = app.clone();
            move || !app.window_visible() && about_window().is_none()
        }));
        steps.push(act({
            let app = app.clone();
            move || app.tray().unwrap().invoke_show_window()
        }));
        steps.push(wait_for("the main window shown again", {
            let app = app.clone();
            move || app.window_visible()
        }));
        steps.push(settle(Duration::from_millis(300)));
        steps.push(act(|| {
            assert!(about_window().is_none(), "About stays closed")
        }));
        steps.push(record_main(&state));
        steps.extend(open_about(a, &state, 1));

        steps.push(Step::Say(
            "about: exiting with About open shuts down cleanly",
        ));
        steps.push(act({
            let app = app.clone();
            move || {
                app.tray().unwrap().invoke_exit();
                let report = app.shutdown_report().expect("shut down");
                assert!(report.is_clean(), "{report:?}");
            }
        }));
        run_in_one_event_loop(steps);
        assert!(about_window().is_none() && !app.window_visible());
        drop(app);
    }

    fn overlay_needs_an_available_hotkey(dir: &Path) {
        step("overlay: refused without an overlay hotkey");
        let path = dir.join("no-overlay-key.toml");
        config_store::save(&path, &initial_config()).unwrap();
        let app = App::new(options(&path)).expect("app starts");
        app.show_window();
        pump(Duration::from_millis(300));
        assert_eq!(app.config().hotkeys.overlay, None);
        assert_eq!(app.ui().get_overlay_key(), "");
        app.ui().invoke_overlay_clicked();
        assert_overlay_off(&app);
        assert!(
            app.ui()
                .get_notice_text()
                .contains("Set one on the Hotkeys tab"),
            "{}",
            app.ui().get_notice_text()
        );
        app.tray().unwrap().invoke_toggle_overlay();
        assert_overlay_off(&app);
        assert!(app.close().unwrap().is_clean());
        drop(app);

        step("overlay: refused while another program holds its hotkey");
        let path = dir.join("taken-overlay-key.toml");
        let mut cfg = initial_config();
        cfg.hotkeys.overlay = Some(Hotkey::plain(Key::F18));
        config_store::save(&path, &cfg).unwrap();
        let other = hold_hotkey(Key::F18);
        let app = App::new(options(&path)).expect("app starts");
        app.show_window();
        pump(Duration::from_millis(300));
        let row = app
            .ui()
            .get_hotkeys()
            .row_data(hotkey_index(HotkeyAction::Overlay) as usize)
            .unwrap();
        assert!(row.problem.contains("another program"), "{}", row.problem);
        app.ui().invoke_overlay_clicked();
        assert_overlay_off(&app);
        let notice = app.ui().get_notice_text().to_string();
        assert!(notice.contains("another program uses F18"), "{notice}");
        assert!(app.close().unwrap().is_clean());
        assert_eq!(other.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
    }

    fn startup_conflict_is_shown(dir: &Path) {
        step("a saved hotkey that another program uses is reported at startup");
        let path = dir.join("conflict.toml");
        let mut cfg = initial_config();
        cfg.hotkeys.set_target = Some(Hotkey::plain(Key::F17));
        config_store::save(&path, &cfg).unwrap();
        let other = hold_hotkey(Key::F17);
        let app = App::new(options(&path)).expect("app starts");
        let rows = app.ui().get_hotkeys();
        let set_target = HotkeyAction::ALL
            .iter()
            .position(|a| *a == HotkeyAction::SetTarget)
            .unwrap();
        for (i, row) in rows.iter().enumerate() {
            if i == set_target {
                assert_eq!(row.binding, "F17");
                assert!(row.problem.contains("another program"), "{}", row.problem);
            } else {
                assert_eq!(row.problem, "", "row {i}");
            }
        }
        assert!(app.close().unwrap().is_clean());
        assert_eq!(other.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
    }

    fn obsolete_start_stop_bindings_are_migrated(dir: &Path) {
        step("a settings file with obsolete Start/Stop hotkeys loads and is migrated");
        let path = dir.join("obsolete.toml");
        let mut text = toml::to_string(&initial_config()).unwrap();
        text = text.replace(
            "[hotkeys]\n",
            "[hotkeys]\nstart = \"Ctrl+Alt+S\"\nstop = \"Ctrl+Alt+D\"\n",
        );
        text = text.replace("interval_ms = 100", "interval_ms = 333");
        std::fs::write(&path, &text).unwrap();
        let app = App::new(options(&path)).expect("older file loads");
        let warning = app.ui().get_config_warning().to_string();
        assert!(warning.contains("no longer supported"), "{warning}");
        assert_eq!(app.config().click.interval_ms, 333, "other settings kept");
        assert_eq!(app.config().hotkeys.toggle, Some(Hotkey::plain(Key::F13)));
        assert_eq!(app.ui().get_hotkeys().row_count(), 4);
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(
            !migrated.contains("start =") && !migrated.contains("stop ="),
            "{migrated}"
        );
        assert!(migrated.contains("interval_ms = 333"));
        assert!(
            !dir.join("obsolete.toml.bak").exists(),
            "not treated as corrupt"
        );
        assert!(app.close().unwrap().is_clean());
        drop(app);

        // The notice is shown only once.
        let again = App::new(options(&path)).unwrap();
        assert_eq!(again.ui().get_config_warning(), "");
        assert!(again.close().unwrap().is_clean());
    }

    fn corrupt_settings_are_reported(dir: &Path) {
        step("a corrupt settings file is moved aside with a visible warning");
        let path = dir.join("corrupt.toml");
        std::fs::write(&path, "interval_ms = [this is not valid").unwrap();
        let app = App::new(options(&path)).expect("app starts with defaults");
        let warning = app.ui().get_config_warning().to_string();
        assert!(warning.contains("not usable"), "{warning}");
        assert!(warning.contains("corrupt.toml.bak"), "{warning}");
        assert_eq!(app.config().click.interval_ms, 100);
        assert!(dir.join("corrupt.toml.bak").exists());
        assert!(app.close().unwrap().is_clean());
    }
}

//! End-to-end tests of the real executable (separate processes).
//!
//! Interactive - run with:
//! `cargo test -p cadence --test gui_process -- --ignored --test-threads=1`
//!
//! Each test gives the application its own settings file
//! (`CADENCE_CONFIG`, hotkeys on F13/F14/F15/F18), single-instance name
//! (`CADENCE_INSTANCE`) and log directory (`CADENCE_LOG_DIR`), so it
//! never interferes with a running Cadence (or Autoclicker). Every click is swallowed by a
//! low-level mouse hook in this test process, and keys are injected only
//! while a test-owned window has the foreground.

#![cfg(windows)]

use cadence::overlay::OVERLAY_ALPHA;
use cadence_core::backend::InputBackend;
use cadence_core::config::{Config, HotkeyConfig, TargetMode};
use cadence_core::geometry::{Point, Rect};
use cadence_core::keys::{Hotkey, Key};
use cadence_platform::WindowsInput;
use cadence_platform::diagnostics::{
    HookEvent, MouseHookRecorder, TEST_WINDOW_GRAY, TestWindow, WindowInfo, WindowKind, bmp,
    capture_screen, click_unmarked, client_rect, force_foreground, foreground_window, monitors,
    post_close, send_keys, simulate_session_end, window_at, windows_of_process,
};
use cadence_platform::{HotkeyService, SYSTEM_EVENTS_WINDOW_TITLE};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const VK_F13: u16 = 0x7C;
const VK_F14: u16 = 0x7D;
const VK_F15: u16 = 0x7E;
const VK_F18: u16 = 0x81;

/// A per-test directory with a settings file and a log directory.
struct Sandbox {
    dir: PathBuf,
    instance: String,
}

impl Sandbox {
    fn new(name: &str, always_on_top: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("cadence-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config {
            hotkeys: {
                let mut h = HotkeyConfig::none();
                h.toggle = Some(Hotkey::plain(Key::F13));
                h.set_target = Some(Hotkey::plain(Key::F15));
                h.exit = Some(Hotkey::plain(Key::F14));
                h.overlay = Some(Hotkey::plain(Key::F18));
                h
            },
            ..Config::default()
        };
        cfg.click.mode = TargetMode::Cursor;
        cfg.window.always_on_top = always_on_top;
        std::fs::write(dir.join("config.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        Self {
            dir,
            instance: format!("CadenceE2E.{name}.{}", std::process::id()),
        }
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config.toml")
    }

    fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(self.logs().join("cadence.log")).unwrap_or_default()
    }

    fn start(&self) -> App {
        self.start_with_log_dir(&self.logs())
    }

    fn start_with_log_dir(&self, log_dir: &Path) -> App {
        let child = Command::new(env!("CARGO_BIN_EXE_cadence"))
            .env("CADENCE_CONFIG", self.config())
            .env("CADENCE_INSTANCE", &self.instance)
            .env("CADENCE_LOG_DIR", log_dir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start cadence");
        App(child)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct App(Child);

impl App {
    fn visible_window(&self) -> Option<WindowInfo> {
        windows_of_process(self.0.id())
            .into_iter()
            .find(|w| w.visible && w.title.starts_with("Cadence"))
    }

    fn window(&self) -> WindowInfo {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(w) = self.visible_window() {
                return w;
            }
            assert!(Instant::now() < end, "the main window did not appear");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_hidden(&self) {
        let end = Instant::now() + Duration::from_secs(5);
        while self.visible_window().is_some() {
            assert!(Instant::now() < end, "the main window did not hide");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_title(&self, title: &str) {
        let end = Instant::now() + Duration::from_secs(5);
        while self.window().title != title {
            assert!(Instant::now() < end, "title never became {title:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn running(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(s) = self.0.try_wait().unwrap() {
                return Some(s);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn system_events_window(&self) -> WindowInfo {
        windows_of_process(self.0.id())
            .into_iter()
            .find(|w| w.title == SYSTEM_EVENTS_WINDOW_TITLE)
            .expect("hidden system-events window")
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A test-owned window in the foreground, so injected keys never reach
/// another application (and the GUI is not focused).
fn foreground_test_window() -> TestWindow {
    let m = monitors()[0];
    let r = Rect::new(m.right - 260, m.bottom - 260, m.right - 60, m.bottom - 60);
    let window = TestWindow::create(r, WindowKind::TopmostPopup, "e2e keys").unwrap();
    for _ in 0..40 {
        if window.bring_to_foreground() {
            std::thread::sleep(Duration::from_millis(80));
            if foreground_window() == window.id() {
                return window;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("inconclusive: could not take the foreground");
}

fn tap(window: &TestWindow, vk: u16) {
    assert_eq!(
        foreground_window(),
        window.id(),
        "foreground changed; not injecting"
    );
    assert_eq!(send_keys(&[(vk, false), (vk, true)]), 2);
}

fn downs(e: &[HookEvent]) -> usize {
    e.iter().filter(|e| e.message.is_down()).count()
}

fn assert_clean_stop(recorder: &MouseHookRecorder) {
    let count = recorder.events().len();
    std::thread::sleep(Duration::from_millis(400));
    let events = recorder.events();
    assert_eq!(events.len(), count, "no clicks after exit");
    for pair in events.chunks(2) {
        assert!(pair[0].message.is_down(), "{events:?}");
        assert!(
            pair.get(1).is_some_and(|e| e.message.is_up()),
            "button left held: {events:?}"
        );
    }
}

/// Exits the application with the exit hotkey and checks a clean exit.
fn exit_with_hotkey(app: &mut App) {
    let keys = foreground_test_window();
    tap(&keys, VK_F14);
    let status = app
        .wait_exit(Duration::from_secs(10))
        .expect("process exited");
    assert_eq!(status.code(), Some(0), "clean shutdown reports success");
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn closing_the_window_hides_it_and_clicking_continues() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let sb = Sandbox::new("hide", true);
    let mut app = sb.start();
    let win = app.window();
    assert_eq!(win.title, "Cadence");
    assert!(win.topmost, "always-on-top from the settings is applied");

    let keys = foreground_test_window();
    assert_ne!(foreground_window(), win.id, "the GUI is not focused");
    tap(&keys, VK_F13);
    app.wait_title("Cadence - Running");
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 3));

    // The close button hides the window; the process keeps running and clicking.
    assert!(post_close(app.window().id));
    app.wait_hidden();
    assert!(app.running());
    let before = downs(&recorder.events());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        downs(&recorder.events()) > before,
        "still clicking while hidden"
    );

    // Hotkeys keep working while hidden.
    tap(&keys, VK_F13);
    std::thread::sleep(Duration::from_millis(300));
    assert_clean_stop(&recorder);
    assert!(app.running());
    drop(keys);
    exit_with_hotkey(&mut app);
}

/// Waits until `cond` holds for the application's main window.
fn wait_window(app: &App, what: &str, cond: impl Fn(&WindowInfo) -> bool) -> WindowInfo {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let w = app.window();
        if cond(&w) {
            return w;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for: {what} ({w:?})"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn overlay_lets_clicks_reach_the_window_underneath() {
    // No mouse hook here: the click must really be delivered. It is only
    // injected after checking that a test-owned window is under the point.
    let sb = Sandbox::new("overlay", true);
    let mut app = sb.start();
    let win = app.window();
    assert!(win.topmost && !win.click_through && win.alpha.is_none());
    let client = client_rect(win.id).expect("client area");
    let p = Point::new(
        (client.left + client.right) / 2,
        (client.top + client.bottom) / 2,
    );

    // A test window under Cadence's window, around that point.
    let under = TestWindow::create(
        Rect::new(p.x - 40, p.y - 40, p.x + 40, p.y + 40),
        WindowKind::TopmostPopup,
        "under the overlay",
    )
    .unwrap();
    assert!(force_foreground(win.id), "Cadence back in front");
    wait_until("Cadence in front", || window_at(p) == win.id);

    let keys = foreground_test_window();
    assert_eq!(window_at(p), win.id, "the key window is elsewhere");
    tap(&keys, VK_F18);
    let overlay = wait_window(&app, "overlay mode", |w| w.click_through);
    assert_eq!(overlay.alpha, Some(OVERLAY_ALPHA), "translucent");
    assert!(overlay.topmost);
    assert_eq!(
        window_at(p),
        under.id(),
        "the mouse passes through Cadence's window"
    );
    assert_eq!(click_unmarked(p), 2);
    assert!(
        under.wait_for(Duration::from_secs(2), |m| m
            .iter()
            .any(|m| m.is_left_down())),
        "the click reached the window underneath"
    );
    drop(keys);

    // The hotkey ends it, and Cadence takes clicks again.
    let keys = foreground_test_window();
    tap(&keys, VK_F18);
    let normal = wait_window(&app, "normal window", |w| !w.click_through);
    assert_eq!(normal.alpha, None, "opaque (no longer layered)");
    drop(keys);
    wait_until("Cadence takes the mouse again", || window_at(p) == win.id);
    drop(under);
    assert!(app.running());
    exit_with_hotkey(&mut app);
}

/// A test-owned window in the foreground, with the cursor on it (so it is
/// not over Cadence's window, where it could change how a control looks).
fn keys_window_with_cursor() -> TestWindow {
    let keys = foreground_test_window();
    let r = client_rect(keys.id()).expect("client area");
    WindowsInput::new()
        .move_cursor(Point::new((r.left + r.right) / 2, (r.top + r.bottom) / 2))
        .unwrap();
    keys
}

/// Saves a capture if `CADENCE_TEST_CAPTURES` names a directory.
fn save_capture(name: &str, rect: Rect, pixels: &[[u8; 3]]) {
    if let Some(dir) = std::env::var_os("CADENCE_TEST_CAPTURES") {
        let (w, h) = (
            (rect.right - rect.left) as usize,
            (rect.bottom - rect.top) as usize,
        );
        std::fs::write(
            PathBuf::from(dir).join(format!("{name}.bmp")),
            bmp(w, h, pixels),
        )
        .expect("write capture");
    }
}

/// How many pixels differ from `expected` by more than `tolerance` in a channel.
fn mismatches(actual: &[[u8; 3]], expected: impl Iterator<Item = [u8; 3]>, tolerance: u8) -> usize {
    actual
        .iter()
        .zip(expected)
        .filter(|(a, e)| (0..3).any(|c| a[c].abs_diff(e[c]) > tolerance))
        .count()
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn overlay_is_drawn_translucent_and_fully_redrawn_afterwards() {
    let sb = Sandbox::new("translucent", true);
    let mut app = sb.start();
    let win = app.window();
    let client = client_rect(win.id).expect("client area");
    let center = Point::new(
        (client.left + client.right) / 2,
        (client.top + client.bottom) / 2,
    );
    // A gray test window right behind Cadence's: the only thing that can
    // show through, so the expected colours are known.
    let backdrop = TestWindow::create(
        Rect::new(
            client.left - 40,
            client.top - 60,
            client.right + 40,
            client.bottom + 40,
        ),
        WindowKind::TopmostPopup,
        "backdrop",
    )
    .unwrap();
    assert!(force_foreground(win.id), "Cadence back in front");
    wait_until("Cadence in front", || window_at(center) == win.id);
    // Inside the frame drawn in overlay mode, so only translucency differs.
    let inner = Rect::new(
        client.left + 4,
        client.top + 4,
        client.right - 4,
        client.bottom - 4,
    );
    let keys = keys_window_with_cursor();
    std::thread::sleep(Duration::from_millis(500));
    let opaque = capture_screen(inner).expect("capture");
    save_capture("1-opaque", inner, &opaque);
    let pixels = opaque.len();

    tap(&keys, VK_F18);
    wait_window(&app, "overlay mode", |w| w.click_through);
    std::thread::sleep(Duration::from_millis(500));
    let translucent = capture_screen(inner).expect("capture");
    save_capture("2-overlay", inner, &translucent);
    // Windows blends the window over the backdrop with its opacity. Checked
    // below the tab row, whose view buttons overlay mode replaces by a hint.
    let a = u16::from(OVERLAY_ALPHA);
    let blend = |p: &[u8; 3]| {
        p.map(|c| {
            ((u16::from(c) * a + u16::from(TEST_WINDOW_GRAY[0]) * (255 - a) + 127) / 255) as u8
        })
    };
    let width = (inner.right - inner.left) as usize;
    let below_tabs = pixels / 4 / width * width;
    let checked = pixels - below_tabs;
    let wrong = mismatches(
        &translucent[below_tabs..],
        opaque[below_tabs..].iter().map(blend),
        6,
    );
    assert!(
        wrong * 200 <= checked,
        "{wrong} of {checked} pixels are not the window blended over the backdrop"
    );
    let unchanged = pixels - mismatches(&translucent, opaque.iter().copied(), 6);
    assert!(
        unchanged * 2 < pixels,
        "translucency must be visible ({unchanged} of {pixels} pixels unchanged)"
    );
    drop(keys);

    let keys = keys_window_with_cursor();
    tap(&keys, VK_F18);
    wait_window(&app, "normal window", |w| !w.click_through);
    drop(keys);
    // Cadence was brought to the front; give the focus to a test window
    // again, as for the first capture.
    let keys = keys_window_with_cursor();
    std::thread::sleep(Duration::from_millis(500));
    let again = capture_screen(inner).expect("capture");
    save_capture("3-normal-again", inner, &again);
    let changed = mismatches(&again, opaque.iter().copied(), 2);
    assert!(
        changed * 1000 <= pixels,
        "{changed} of {pixels} pixels differ: the window is not fully redrawn after overlay mode"
    );
    drop(keys);
    drop(backdrop);
    exit_with_hotkey(&mut app);
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn exit_hotkey_while_clicking_shuts_down() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let sb = Sandbox::new("exit", false);
    let mut app = sb.start();
    assert!(!app.window().topmost);
    let keys = foreground_test_window();
    tap(&keys, VK_F13);
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 2));
    tap(&keys, VK_F14);
    let status = app
        .wait_exit(Duration::from_secs(10))
        .expect("process exited");
    assert_eq!(status.code(), Some(0));
    assert_clean_stop(&recorder);
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn target_set_by_hotkey_persists_across_restarts() {
    let sb = Sandbox::new("persist", false);
    let config = sb.config();
    let mut app = sb.start();
    app.window();
    let keys = foreground_test_window();
    assert!(
        !std::fs::read_to_string(&config)
            .unwrap()
            .contains("[click.target]")
    );
    tap(&keys, VK_F15); // set target to the current cursor position
    let end = Instant::now() + Duration::from_secs(3);
    while !std::fs::read_to_string(&config)
        .unwrap()
        .contains("[click.target]")
    {
        assert!(Instant::now() < end, "target was not saved");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(keys);
    exit_with_hotkey(&mut app);
    let saved: Config = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert!(saved.click.target.is_some());

    let mut again = sb.start();
    again.window();
    exit_with_hotkey(&mut again);
    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(reloaded, saved);
    assert!(!config.with_file_name("config.toml.bak").exists());
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn a_second_instance_shows_the_first_and_exits() {
    let sb = Sandbox::new("single", false);
    let mut first = sb.start();
    first.window();
    assert!(post_close(first.window().id));
    first.wait_hidden();

    let mut second = sb.start();
    let status = second
        .wait_exit(Duration::from_secs(10))
        .expect("second instance exited");
    assert_eq!(status.code(), Some(0));
    assert!(
        windows_of_process(second.0.id()).is_empty(),
        "no window of its own"
    );
    // The first instance shows its window again.
    let shown = first.window();
    assert_eq!(shown.title, "Cadence");
    assert!(first.running());
    let log = sb.log_text();
    assert!(log.contains("another instance is already running"), "{log}");

    // After the first instance exits, the lock is free again.
    exit_with_hotkey(&mut first);
    let mut third = sb.start();
    third.window();
    assert!(
        third.running(),
        "a new instance starts after the lock is released"
    );
    exit_with_hotkey(&mut third);
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn end_of_session_shuts_down_cleanly_while_clicking() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let sb = Sandbox::new("session", false);
    let mut app = sb.start();
    app.window();
    let keys = foreground_test_window();
    tap(&keys, VK_F13);
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 2));
    drop(keys);
    let reply = simulate_session_end(app.system_events_window().id, Duration::from_secs(10));
    assert_eq!(reply, Some(1), "logoff/shutdown is not blocked");
    let status = app
        .wait_exit(Duration::from_secs(10))
        .expect("process exited");
    assert_eq!(status.code(), Some(0));
    assert_clean_stop(&recorder);
    let log = sb.log_text();
    assert!(log.contains("the Windows session is ending"), "{log}");
    assert!(
        log.contains("engine and hotkey threads stopped cleanly"),
        "{log}"
    );
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn the_log_records_startup_problems_and_shutdown() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let sb = Sandbox::new("log", false);
    // Another "program" holds F15, the set-target binding.
    let taken = Config {
        hotkeys: {
            let mut h = HotkeyConfig::none();
            h.toggle = Some(Hotkey::plain(Key::F15));
            h
        },
        ..Config::default()
    };
    let (other, _) = HotkeyService::start(&taken.validate().unwrap().hotkeys, |_| {}).unwrap();

    let mut app = sb.start();
    app.window();
    let keys = foreground_test_window();
    tap(&keys, VK_F13);
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 3));
    tap(&keys, VK_F13);
    std::thread::sleep(Duration::from_millis(300));
    drop(keys);
    exit_with_hotkey(&mut app);
    let _ = other.shutdown(Duration::from_secs(2));

    let log = sb.log_text();
    for expected in [
        "INFO  [cadence::gui] Cadence ",
        "hotkey toggle = F13",
        "hotkey set target = F15 could not be registered",
        "clicking started (current position mode)",
        "clicking stopped after",
        "shutting down",
        "engine and hotkey threads stopped cleanly",
        "exited",
    ] {
        assert!(log.contains(expected), "missing {expected:?} in:\n{log}");
    }
    assert!(
        log.contains("WARN  [cadence::gui] hotkey set target"),
        "{log}"
    );
    // No per-click lines and no account name.
    assert!(log.lines().count() < 30, "{log}");
    let home = std::env::var("USERPROFILE").unwrap();
    assert!(
        !log.contains(&home),
        "the profile path must not be logged:\n{log}"
    );
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn an_unusable_log_directory_does_not_prevent_startup_or_shutdown() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let sb = Sandbox::new("nolog", false);
    let blocker = sb.dir.join("not-a-directory");
    std::fs::write(&blocker, b"x").unwrap();
    let mut app = sb.start_with_log_dir(&blocker.join("logs"));
    app.window();
    let keys = foreground_test_window();
    tap(&keys, VK_F13);
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 2));
    drop(keys);
    exit_with_hotkey(&mut app);
    assert_clean_stop(&recorder);
}

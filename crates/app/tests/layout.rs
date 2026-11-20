//! The window's layouts, rendered offscreen with Slint's software renderer.
//! No window is shown, so this runs with the normal test suite.
//!
//! Set `CADENCE_LAYOUT_SNAPSHOTS` to a directory to also save every rendered
//! layout there as a BMP file, to look at.

#![cfg(windows)]

use cadence::ui::{AboutWindow, HotkeyRow, MainWindow, Palette, RunStatus};
use cadence_platform::diagnostics::bmp;
use slint::language::ColorScheme;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, LogicalSize, Model, ModelRc, Rgb8Pixel, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// A window for each component (the main window, the About window).
#[derive(Clone, Default)]
struct Offscreen(Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>);

impl Offscreen {
    fn last(&self) -> Rc<MinimalSoftwareWindow> {
        self.0.borrow().last().expect("a window").clone()
    }
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        self.0.borrow_mut().push(window.clone());
        Ok(window)
    }
}

/// The size the window takes for the current layout, measured at that
/// layout's width (descriptions wrap, so the height depends on it).
///
/// Slint creates and removes `if` elements when it next renders, so right
/// after a property change the layout size is still the old one. Showing the
/// (already shown) window does that work at once; the application relies on
/// this when it resizes the window for a new layout, so it is checked here.
fn layout_size(window: &MinimalSoftwareWindow, ui: &MainWindow) -> (f32, f32) {
    ui.show().unwrap();
    let width = ui.get_layout_preferred_width();
    window.set_size(LogicalSize::new(width, ui.get_layout_preferred_height()));
    ui.show().unwrap();
    (width, ui.get_layout_preferred_height())
}

/// What the application shows for typical settings.
fn populate(ui: &MainWindow) {
    let row = |label: &str, binding: &str| HotkeyRow {
        label: label.into(),
        binding: binding.into(),
        problem: "".into(),
        bound: true,
    };
    ui.set_hotkeys(ModelRc::new(VecModel::from(vec![
        row("Toggle start/stop", "F6"),
        row("Set target", "F7"),
        row("Toggle overlay", "F9"),
        row("Exit", "Ctrl+Shift+F8"),
    ])));
    ui.set_toggle_key("F6".into());
    ui.set_overlay_key("F9".into());
    ui.set_interval_text("100".into());
    ui.set_jitter_text("10".into());
    ui.set_hold_text("10".into());
    ui.set_radius_text("3".into());
    ui.set_target_text("640, 360".into());
    ui.set_summary_text("Fixed position 640, 360 · 100 ms".into());
}

/// Renders the window at its layout size; saves it if snapshots are on.
fn render(window: &MinimalSoftwareWindow, ui: &MainWindow, name: &str) {
    let (w, h) = layout_size(window, ui);
    draw(window, w, h, name);
}

/// Renders `window` at `width` x `height`; saves it if snapshots are on.
fn draw(window: &MinimalSoftwareWindow, w: f32, h: f32, name: &str) {
    window.set_size(LogicalSize::new(w, h));
    let size = window.size();
    let (width, height) = (size.width as usize, size.height as usize);
    let mut pixels = vec![Rgb8Pixel::default(); width * height];
    window.request_redraw();
    assert!(
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width);
        }),
        "{name}: nothing was drawn"
    );
    if let Some(dir) = std::env::var_os("CADENCE_LAYOUT_SNAPSHOTS") {
        let path = PathBuf::from(dir).join(format!("{name}.bmp"));
        let rgb: Vec<[u8; 3]> = pixels.iter().map(|p| [p.r, p.g, p.b]).collect();
        std::fs::write(&path, bmp(width, height, &rgb)).expect("write snapshot");
    }
}

/// Renders the layouts in the dark colour scheme (snapshots only). Here,
/// controls that exist when the scheme changes may keep some colours of the
/// old one, so every rendering follows a layout switch, which recreates them
/// (the application gets its scheme before the window is first drawn).
fn render_dark(window: &MinimalSoftwareWindow, ui: &MainWindow) {
    ui.global::<Palette>().set_color_scheme(ColorScheme::Dark);
    for (compact, overlay, name) in [
        (true, false, "dark-compact"),
        (false, false, "dark-full"),
        (true, true, "dark-compact-overlay"),
        (false, true, "dark-full-overlay"),
    ] {
        ui.set_compact_mode(compact);
        ui.set_overlay_active(overlay);
        render(window, ui, name);
    }
    ui.set_overlay_active(false);
    ui.set_status(RunStatus::Running);
    ui.set_status_headline("Running".into());
    ui.set_compact_mode(true);
    render(window, ui, "dark-compact-running");
    ui.set_current_tab(1);
    ui.set_compact_mode(false);
    render(window, ui, "dark-full-hotkeys-running");
    ui.set_current_tab(0);
    ui.set_status(RunStatus::Stopped);
    ui.set_status_headline("Stopped".into());
    ui.global::<Palette>().set_color_scheme(ColorScheme::Light);
}

#[test]
fn compact_and_overlay_layouts() {
    let platform = Offscreen::default();
    slint::platform::set_platform(Box::new(platform.clone())).unwrap();
    let ui = MainWindow::new().unwrap();
    let window = platform.last();
    populate(&ui);
    ui.show().unwrap();

    let full = layout_size(&window, &ui);
    assert_eq!(full.0, 420.0, "the full layout keeps its width");
    assert!(full.1 > 400.0, "{full:?}");
    render(&window, &ui, "full");

    // The compact layout: status, summary and the view buttons only.
    ui.set_compact_mode(true);
    let compact = layout_size(&window, &ui);
    assert_eq!(compact, (360.0, 142.0), "the compact window is 360 x 142");
    render(&window, &ui, "compact");

    // Running and a long headline (it is elided, not cut off by the button).
    ui.set_status(RunStatus::Running);
    ui.set_status_headline("Running".into());
    render(&window, &ui, "compact-running");
    ui.set_status(RunStatus::Waiting);
    ui.set_status_headline("Release modifier keys…".into());
    assert_eq!(layout_size(&window, &ui), compact);
    ui.set_status(RunStatus::Stopped);
    ui.set_status_headline("Stopped".into());

    // A notice takes the summary's place, so the compact window keeps its size.
    ui.set_notice_text("No target set. Use Pick target or the set-target hotkey first.".into());
    assert_eq!(layout_size(&window, &ui), compact);
    render(&window, &ui, "compact-notice");
    ui.set_notice_text("".into());

    // So does an error (the headline already says "Error").
    ui.set_status(RunStatus::Error);
    ui.set_status_headline("Error".into());
    ui.set_error_text("Could not press the mouse button: Access is denied (Win32 error 5)".into());
    assert_eq!(layout_size(&window, &ui), compact);
    render(&window, &ui, "compact-error");
    ui.set_error_text("".into());
    ui.set_status(RunStatus::Stopped);
    ui.set_status_headline("Stopped".into());

    // Overlay mode replaces the view buttons with a hint of the same height,
    // in both layouts: turning it on or off never resizes the window.
    ui.set_overlay_active(true);
    assert_eq!(layout_size(&window, &ui), compact);
    render(&window, &ui, "compact-overlay");
    ui.set_compact_mode(false);
    assert_eq!(layout_size(&window, &ui), full);
    render(&window, &ui, "full-overlay");
    ui.set_overlay_active(false);

    // Back and forth: each layout always has the same size.
    for _ in 0..3 {
        ui.set_compact_mode(true);
        assert_eq!(layout_size(&window, &ui), compact);
        ui.set_compact_mode(false);
        assert_eq!(layout_size(&window, &ui), full);
    }

    // The settings shown in the full layout survive a round trip through the
    // compact one (they live in the window's properties, not the controls).
    ui.set_compact_mode(true);
    ui.set_compact_mode(false);
    assert_eq!(ui.get_interval_text(), "100");
    assert_eq!(ui.get_radius_text(), "3");
    assert_eq!(ui.get_hotkeys().row_count(), 4);
    render(&window, &ui, "full-again");

    // The About window, in the same visual language.
    let about = AboutWindow::new().unwrap();
    about.set_version("1.1.0".into());
    about.show().unwrap();
    draw(&platform.last(), 380.0, 470.0, "about");

    if std::env::var_os("CADENCE_LAYOUT_SNAPSHOTS").is_some() {
        render_dark(&window, &ui);
        about
            .global::<Palette>()
            .set_color_scheme(ColorScheme::Dark);
        draw(&platform.last(), 380.0, 470.0, "dark-about");
    }
}

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

/// A rendered window.
struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 3]>,
}

impl Frame {
    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        self.pixels[y * self.width + x]
    }

    /// The pixels of the `width` x `height` area at (`x`, `y`).
    fn area(&self, x: usize, y: usize, width: usize, height: usize) -> Vec<[u8; 3]> {
        (y..y + height)
            .flat_map(|row| (x..x + width).map(move |col| (col, row)))
            .map(|(col, row)| self.at(col, row))
            .collect()
    }

    /// The status panel's bounds (x, y, width, height) as drawn: where its
    /// border starts to differ from the window background around it, down
    /// the middle of the window and across the panel's status row. From 4 px
    /// inside the window, past overlay mode's 2 px frame along its edge.
    fn panel_bounds(&self) -> (usize, usize, usize, usize) {
        const INSET: usize = 4;
        let background = self.at(INSET, INSET);
        let middle = self.width / 2;
        let top = (INSET..self.height)
            .find(|&y| self.at(middle, y) != background)
            .expect("the panel's top edge");
        let bottom = (top..self.height)
            .find(|&y| self.at(middle, y) == background)
            .expect("the window background below the panel");
        let row = top + 20;
        let left = (INSET..self.width)
            .find(|&x| self.at(x, row) != background)
            .expect("the panel's left edge");
        let right = (INSET..self.width - INSET)
            .rev()
            .find(|&x| self.at(x, row) != background)
            .expect("the panel's right edge");
        (left, top, right + 1 - left, bottom - top)
    }
}

/// The status panel's bounds (x, y, width, height), from the layout.
fn panel_layout(ui: &MainWindow) -> (f32, f32, f32, f32) {
    (
        ui.get_status_panel_x(),
        ui.get_status_panel_y(),
        ui.get_status_panel_width(),
        ui.get_status_panel_height(),
    )
}

/// The full layout's status panel, laid out and as drawn.
const FULL_PANEL: (usize, usize, usize, usize) = (14, 14, 392, 91);

/// The compact layout's status panel: the full layout's, at the compact
/// window's width (60 px narrower, the difference between the windows'
/// widths), with the same height, margins and contents.
const COMPACT_PANEL: (usize, usize, usize, usize) = (14, 14, 332, 91);

fn as_lengths((x, y, w, h): (usize, usize, usize, usize)) -> (f32, f32, f32, f32) {
    (x as f32, y as f32, w as f32, h as f32)
}

/// Checks the compact layout's status panel, laid out and as drawn.
fn check_compact_panel(ui: &MainWindow, frame: &Frame, name: &str) {
    assert_eq!(
        panel_layout(ui),
        as_lengths(COMPACT_PANEL),
        "{name}: laid out"
    );
    assert_eq!(frame.panel_bounds(), COMPACT_PANEL, "{name}: as drawn");
}

/// The status panel drawn the same in both layouts: every part that does not
/// depend on the width (the corners, border, paddings and spacing, the status
/// row, the divider and the overlay button), pixel for pixel. Only the space
/// between the headline and the button, and the layout button's label, differ.
fn check_same_panel(full: &Frame, compact: &Frame) {
    let (x, y, full_width, height) = FULL_PANEL;
    let compact_width = COMPACT_PANEL.2;
    // From the left edge: 170 px, all rows.
    assert!(
        full.area(x, y, 170, height) == compact.area(x, y, 170, height),
        "the panel's left part differs between the layouts"
    );
    // From the right edge: 150 px of the status row, down to the divider
    // (1 + 10 + 32 + 10 px below the top edge, included).
    let right = |width: usize| x + width - 150;
    assert!(
        full.area(right(full_width), y, 150, 54) == compact.area(right(compact_width), y, 150, 54),
        "the panel's status row differs between the layouts at the right"
    );
    // The right edge (border and padding), all rows.
    let edge = |width: usize| x + width - 6;
    assert!(
        full.area(edge(full_width), y, 6, height)
            == compact.area(edge(compact_width), y, 6, height),
        "the panel's right edge differs between the layouts"
    );
}

/// Renders the window at its layout size; saves it if snapshots are on.
fn render(window: &MinimalSoftwareWindow, ui: &MainWindow, name: &str) -> Frame {
    let (w, h) = layout_size(window, ui);
    draw(window, w, h, name)
}

/// Renders `window` at `width` x `height`; saves it if snapshots are on.
fn draw(window: &MinimalSoftwareWindow, w: f32, h: f32, name: &str) -> Frame {
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
    let rgb: Vec<[u8; 3]> = pixels.iter().map(|p| [p.r, p.g, p.b]).collect();
    if let Some(dir) = std::env::var_os("CADENCE_LAYOUT_SNAPSHOTS") {
        let path = PathBuf::from(dir).join(format!("{name}.bmp"));
        std::fs::write(&path, bmp(width, height, &rgb)).expect("write snapshot");
    }
    Frame {
        width,
        height,
        pixels: rgb,
    }
}

/// Renders the layouts in the dark colour scheme (snapshots only). Here,
/// controls that exist when the scheme changes may keep some colours of the
/// old one (the application gets its scheme before the window is first
/// drawn). Every rendering follows a layout switch, which recreates most of
/// them; the status panel, shared by both layouts, may still show some.
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
    let full_frame = render(&window, &ui, "full");
    assert_eq!(panel_layout(&ui), as_lengths(FULL_PANEL), "full: laid out");
    assert_eq!(full_frame.panel_bounds(), FULL_PANEL, "full: as drawn");

    // The compact layout: the same status panel, and a summary line below it.
    ui.set_compact_mode(true);
    let compact = layout_size(&window, &ui);
    assert_eq!(compact, (360.0, 142.0), "the compact window is 360 x 142");
    let frame = render(&window, &ui, "compact");
    check_compact_panel(&ui, &frame, "compact");
    check_same_panel(&full_frame, &frame);

    // Running and a long headline (it is elided, not cut off by the button).
    ui.set_status(RunStatus::Running);
    ui.set_status_headline("Running".into());
    let frame = render(&window, &ui, "compact-running");
    check_compact_panel(&ui, &frame, "compact, running");
    ui.set_status(RunStatus::Waiting);
    ui.set_status_headline("Release modifier keys…".into());
    assert_eq!(layout_size(&window, &ui), compact);
    ui.set_status(RunStatus::Stopped);
    ui.set_status_headline("Stopped".into());

    // A notice takes the summary's place, so the compact window keeps its size.
    ui.set_notice_text("No target set. Use Pick target or the set-target hotkey first.".into());
    assert_eq!(layout_size(&window, &ui), compact);
    let frame = render(&window, &ui, "compact-notice");
    check_compact_panel(&ui, &frame, "compact, notice");
    ui.set_notice_text("".into());

    // So does an error (the headline already says "Error").
    ui.set_status(RunStatus::Error);
    ui.set_status_headline("Error".into());
    ui.set_error_text("Could not press the mouse button: Access is denied (Win32 error 5)".into());
    assert_eq!(layout_size(&window, &ui), compact);
    let frame = render(&window, &ui, "compact-error");
    check_compact_panel(&ui, &frame, "compact, error");
    ui.set_error_text("".into());
    ui.set_status(RunStatus::Stopped);
    ui.set_status_headline("Stopped".into());

    // Overlay mode replaces the view buttons with a hint of the same height,
    // in both layouts: turning it on or off never resizes the window.
    ui.set_overlay_active(true);
    assert_eq!(layout_size(&window, &ui), compact);
    let frame = render(&window, &ui, "compact-overlay");
    check_compact_panel(&ui, &frame, "compact, overlay");
    ui.set_compact_mode(false);
    assert_eq!(layout_size(&window, &ui), full);
    let frame = render(&window, &ui, "full-overlay");
    assert_eq!(frame.panel_bounds(), FULL_PANEL, "full, overlay: as drawn");
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

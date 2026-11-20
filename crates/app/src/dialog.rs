//! Where a dialog (the About window) goes: centred over the window it
//! belongs to, and moved as little as needed to stay inside the work area of
//! that window's monitor (a dialog larger than the compact window would
//! otherwise stick out past the screen edge).

#![forbid(unsafe_code)]

use cadence_core::geometry::{Point, Rect};

/// The top-left position of a dialog of `size` (width, height) centred over
/// `owner`, kept inside `work` when given. All in physical screen pixels,
/// outer window bounds.
pub fn centered_over(owner: Rect, size: (i32, i32), work: Option<Rect>) -> Point {
    let (width, height) = size;
    // Rounded down, also when the dialog is larger than its owner.
    let x = owner.left + (owner.right - owner.left - width).div_euclid(2);
    let y = owner.top + (owner.bottom - owner.top - height).div_euclid(2);
    match work {
        Some(work) => Point::new(
            fit(x, width, work.left, work.right),
            fit(y, height, work.top, work.bottom),
        ),
        None => Point::new(x, y),
    }
}

/// Moves `start` (with `length`) inside `[low, high)`; if it does not fit,
/// it starts at `low`, so the title bar stays reachable.
fn fit(start: i32, length: i32, low: i32, high: i32) -> i32 {
    start.min(high - length).max(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect::new(0, 0, 1920, 1040);

    #[test]
    fn a_dialog_is_centred_over_its_owner() {
        let owner = Rect::new(100, 50, 522, 687);
        let p = centered_over(owner, (382, 501), Some(WORK));
        assert_eq!(p, Point::new(120, 118));
        // Same centre (to the pixel, rounding down).
        assert_eq!(p.x + 382 / 2, (100 + 522) / 2);
        assert_eq!(p.y + 501 / 2, (50 + 687) / 2);
    }

    #[test]
    fn a_dialog_larger_than_its_owner_overlaps_it_evenly() {
        // The compact window is smaller than the About window.
        let owner = Rect::new(800, 400, 1162, 574);
        let p = centered_over(owner, (382, 501), Some(WORK));
        assert_eq!(p, Point::new(790, 236));
    }

    #[test]
    fn a_dialog_stays_inside_the_work_area() {
        // The owner at the top-left corner: the dialog is moved down/right
        // just enough, not centred on the screen.
        let owner = Rect::new(0, 0, 362, 174);
        assert_eq!(
            centered_over(owner, (382, 501), Some(WORK)),
            Point::new(0, 0)
        );
        // ...and at the bottom-right corner (above the taskbar).
        let owner = Rect::new(1558, 866, 1920, 1040);
        assert_eq!(
            centered_over(owner, (382, 501), Some(WORK)),
            Point::new(1538, 539)
        );
        // A second monitor left of the first, with negative coordinates.
        let left = Rect::new(-1280, 0, 0, 984);
        let owner = Rect::new(-1300, 100, -938, 274);
        assert_eq!(
            centered_over(owner, (382, 501), Some(left)),
            Point::new(-1280, 0)
        );
    }

    #[test]
    fn a_dialog_larger_than_the_work_area_keeps_its_title_bar_visible() {
        let small = Rect::new(0, 0, 300, 400);
        let owner = Rect::new(0, 0, 300, 400);
        assert_eq!(
            centered_over(owner, (382, 501), Some(small)),
            Point::new(0, 0)
        );
    }

    #[test]
    fn without_a_work_area_it_is_simply_centred() {
        let owner = Rect::new(0, 0, 362, 174);
        assert_eq!(
            centered_over(owner, (382, 501), None),
            Point::new(-10, -164)
        );
    }
}

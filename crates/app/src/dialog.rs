//! Where a dialog (the About window) opens: beside the window it belongs to,
//! not over it, so both stay visible, and inside the work area of that
//! window's monitor. Only the opening position: after that the dialog's
//! position is its own.

#![forbid(unsafe_code)]

use cadence_core::geometry::{Point, Rect};

/// The space between a window and a dialog opened beside it, in logical
/// pixels (window frames, invisible resize borders included).
pub const GAP: f32 = 12.0;

/// The top-left position of a dialog of `size` (width, height) opened beside
/// `owner`, `gap` pixels apart: to its right, or to its left when there is
/// no room on the right, top edges aligned. All in physical screen pixels,
/// outer window bounds.
///
/// Kept inside `work` when given. When the dialog fits on neither side, it
/// goes on the side with more room, moved in just enough to stay inside the
/// work area (it then overlaps the owner as little as possible).
pub fn beside(owner: Rect, size: (i32, i32), work: Option<Rect>, gap: i32) -> Point {
    let (width, height) = size;
    let right = owner.right + gap;
    let left = owner.left - gap - width;
    let Some(work) = work else {
        return Point::new(right, owner.top);
    };
    let x = if right + width <= work.right {
        right
    } else if left >= work.left {
        left
    } else if work.right - owner.right >= owner.left - work.left {
        fit(right, width, work.left, work.right)
    } else {
        fit(left, width, work.left, work.right)
    };
    Point::new(x, fit(owner.top, height, work.top, work.bottom))
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
    const ABOUT: (i32, i32) = (382, 501);

    fn overlaps(a: Rect, p: Point, (width, height): (i32, i32)) -> bool {
        p.x < a.right && a.left < p.x + width && p.y < a.bottom && a.top < p.y + height
    }

    #[test]
    fn a_dialog_opens_to_the_right_of_its_owner() {
        let owner = Rect::new(100, 50, 522, 687);
        let p = beside(owner, ABOUT, Some(WORK), 12);
        assert_eq!(p, Point::new(534, 50));
        assert!(!overlaps(owner, p, ABOUT));
        // The compact window: smaller than the dialog, still beside it.
        let owner = Rect::new(800, 400, 1162, 574);
        assert_eq!(beside(owner, ABOUT, Some(WORK), 12), Point::new(1174, 400));
    }

    #[test]
    fn without_room_on_the_right_it_opens_on_the_left() {
        let owner = Rect::new(1400, 100, 1822, 737);
        let p = beside(owner, ABOUT, Some(WORK), 12);
        assert_eq!(p, Point::new(1006, 100));
        assert!(!overlaps(owner, p, ABOUT));
        // Exactly enough room on the right: the right.
        let owner = Rect::new(1000, 100, 1526, 737);
        assert_eq!(beside(owner, ABOUT, Some(WORK), 12).x, 1538);
    }

    #[test]
    fn it_stays_inside_the_work_area_vertically() {
        // Near the bottom (above the taskbar): moved up, still beside.
        let owner = Rect::new(100, 866, 462, 1040);
        assert_eq!(beside(owner, ABOUT, Some(WORK), 12), Point::new(474, 539));
        // Above the top of the work area: moved down.
        let owner = Rect::new(100, -20, 462, 154);
        assert_eq!(beside(owner, ABOUT, Some(WORK), 12), Point::new(474, 0));
        // A second monitor left of the first, with negative coordinates.
        let left = Rect::new(-1280, 0, 0, 984);
        let owner = Rect::new(-1200, 100, -838, 274);
        assert_eq!(beside(owner, ABOUT, Some(left), 12), Point::new(-826, 100));
    }

    #[test]
    fn with_room_on_neither_side_it_overlaps_as_little_as_possible() {
        // A wide owner, more room on the right (270 px): at the right edge.
        let owner = Rect::new(220, 100, 1650, 900);
        let p = beside(owner, ABOUT, Some(WORK), 12);
        assert_eq!(p, Point::new(1538, 100));
        // More room on the left (350 px): at the left edge.
        let owner = Rect::new(350, 100, 1750, 900);
        assert_eq!(beside(owner, ABOUT, Some(WORK), 12), Point::new(0, 100));
    }

    #[test]
    fn a_dialog_larger_than_the_work_area_keeps_its_title_bar_visible() {
        let small = Rect::new(0, 0, 300, 400);
        let owner = Rect::new(0, 0, 300, 400);
        assert_eq!(beside(owner, ABOUT, Some(small), 12), Point::new(0, 0));
    }

    #[test]
    fn without_a_work_area_it_opens_to_the_right() {
        let owner = Rect::new(0, 0, 362, 174);
        assert_eq!(beside(owner, ABOUT, None, 12), Point::new(374, 0));
    }
}

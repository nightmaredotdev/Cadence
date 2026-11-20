//! Window placement: where a dialog (the About window) opens over the
//! window it belongs to, and how a window is kept inside the work area of
//! its monitor (the monitor minus the taskbar). Only opening positions: after
//! that a dialog's position is its own.

#![forbid(unsafe_code)]

use cadence_core::geometry::{Point, Rect};

/// The top-left position of a dialog of `size` (width, height) opened over
/// `owner`: offset towards the owner's upper right, overlapping it, with
/// part of the owner left uncovered. All in physical screen pixels, outer
/// window bounds.
///
/// The offsets follow from both sizes, so the result looks alike for the
/// full and the compact window and at any scale:
/// - across, the dialog's centre is a quarter of the owner's width in from
///   the owner's right edge (never centred: a quarter of the owner's width
///   right of centre), but at least a quarter of the owner stays uncovered
///   on the left;
/// - down, its top edge is a sixth of the smaller of the two heights below
///   the owner's top edge (the owner's title bar mostly stays visible).
///
/// Inside `work` when given: when it would stick out on the right, it is
/// mirrored to the owner's upper left; then it is moved in as little as
/// needed.
pub fn over_upper_right(owner: Rect, size: (i32, i32), work: Option<Rect>) -> Point {
    let (width, height) = size;
    let (owner_width, owner_height) = (owner.right - owner.left, owner.bottom - owner.top);
    let dx = (owner_width * 3 / 4 - width / 2).max(owner_width / 4);
    let dy = owner_height.min(height) / 6;
    let right = owner.left + dx;
    let y = owner.top + dy;
    let Some(work) = work else {
        return Point::new(right, y);
    };
    let left = owner.right - dx - width;
    let x = if right + width <= work.right {
        right
    } else if left >= work.left {
        left
    } else {
        fit(right, width, work.left, work.right)
    };
    Point::new(x, fit(y, height, work.top, work.bottom))
}

/// The top-left position that keeps `rect` inside `work`, moved as little as
/// possible (its own position when it already fits). When it is larger than
/// the work area, its top-left corner goes to the work area's.
pub fn keep_inside(rect: Rect, work: Rect) -> Point {
    Point::new(
        fit(rect.left, rect.right - rect.left, work.left, work.right),
        fit(rect.top, rect.bottom - rect.top, work.top, work.bottom),
    )
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
    /// The About window and the full and compact main windows at 100 %
    /// (outer bounds), and the About and full windows at 150 %.
    const ABOUT: (i32, i32) = (396, 445);
    const FULL: (i32, i32) = (436, 644);
    const COMPACT: (i32, i32) = (436, 158);
    const ABOUT_150: (i32, i32) = (594, 668);
    const FULL_150: (i32, i32) = (654, 966);

    fn at(x: i32, y: i32, (width, height): (i32, i32)) -> Rect {
        Rect::new(x, y, x + width, y + height)
    }

    fn overlap(a: Rect, b: Rect) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    #[test]
    fn a_dialog_opens_over_the_upper_right_of_the_full_window() {
        let owner = at(200, 100, FULL);
        let p = over_upper_right(owner, ABOUT, Some(WORK));
        // 436 * 3/4 - 396/2 = 129 across, 445/6 = 74 down.
        assert_eq!(p, Point::new(329, 174));
        let dialog = at(p.x, p.y, ABOUT);
        assert!(overlap(dialog, owner));
        // Part of the owner uncovered on the left, at the top and below.
        assert!(dialog.left - owner.left >= FULL.0 / 4);
        assert!(dialog.top > owner.top && dialog.bottom < owner.bottom);
        // Not centred: a quarter of the owner's width right of centre.
        let centred = owner.left + (FULL.0 - ABOUT.0) / 2;
        assert_eq!(p.x - centred, FULL.0 / 4);
    }

    #[test]
    fn the_offsets_follow_the_sizes_of_both_windows() {
        // The compact window, shorter than the dialog: a sixth of its height.
        let owner = at(200, 100, COMPACT);
        let p = over_upper_right(owner, ABOUT, Some(WORK));
        assert_eq!(p, Point::new(329, 126));
        assert!(overlap(at(p.x, p.y, ABOUT), owner));
        // At 150 % everything scales: 654 * 3/4 - 594/2 = 193, 668/6 = 111.
        let owner = at(200, 100, FULL_150);
        assert_eq!(
            over_upper_right(owner, ABOUT_150, Some(WORK)),
            Point::new(393, 211)
        );
        // A dialog much wider than its owner still leaves a quarter of it.
        let owner = at(200, 100, (300, 400));
        assert_eq!(
            over_upper_right(owner, (600, 200), Some(WORK)),
            Point::new(275, 133)
        );
    }

    #[test]
    fn near_the_right_edge_it_opens_over_the_upper_left() {
        let owner = at(1450, 100, FULL);
        let p = over_upper_right(owner, ABOUT, Some(WORK));
        // Mirrored: its right edge 129 px in from the owner's right edge.
        assert_eq!(p, Point::new(1450 + 436 - 129 - 396, 174));
        let dialog = at(p.x, p.y, ABOUT);
        assert!(overlap(dialog, owner));
        assert!(owner.right - dialog.right >= FULL.0 / 4);
    }

    #[test]
    fn it_stays_inside_the_work_area() {
        // Near the bottom (above the taskbar): moved up.
        let owner = at(200, 900, COMPACT);
        assert_eq!(
            over_upper_right(owner, ABOUT, Some(WORK)),
            Point::new(329, 595)
        );
        // A second monitor left of the first, with negative coordinates.
        let left = Rect::new(-1280, 0, 0, 984);
        let owner = at(-1200, 100, FULL);
        assert_eq!(
            over_upper_right(owner, ABOUT, Some(left)),
            Point::new(-1071, 174)
        );
        // No room on either side (a narrow work area): moved in.
        let narrow = Rect::new(0, 0, 500, 1040);
        let owner = at(50, 100, FULL);
        assert_eq!(
            over_upper_right(owner, ABOUT, Some(narrow)),
            Point::new(104, 174)
        );
        // Without a work area: the plain offsets.
        let owner = at(0, 0, FULL);
        assert_eq!(over_upper_right(owner, ABOUT, None), Point::new(129, 74));
    }

    #[test]
    fn a_window_is_moved_as_little_as_needed_to_fit() {
        // Already inside: unchanged.
        assert_eq!(keep_inside(at(100, 100, FULL), WORK), Point::new(100, 100));
        // Hanging below the bottom (the full window grown from a compact one
        // near the bottom edge): moved up just enough.
        assert_eq!(keep_inside(at(100, 900, FULL), WORK), Point::new(100, 396));
        // ...and past the right and left edges.
        assert_eq!(
            keep_inside(at(1700, 100, FULL), WORK),
            Point::new(1484, 100)
        );
        assert_eq!(keep_inside(at(-50, 100, FULL), WORK), Point::new(0, 100));
        // Taller than the work area: the title bar at its top.
        let short = Rect::new(0, 0, 1920, 600);
        assert_eq!(keep_inside(at(100, 300, FULL), short), Point::new(100, 0));
    }
}

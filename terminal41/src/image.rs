use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Instant;

use image41::DecodedImage;

use crate::screen::row::Row;

/// Inline image placed in the terminal grid.
#[derive(Debug, Clone)]
pub struct PlacedImage {
    /// Decoded image pixels and frames.
    pub image: DecodedImage,
    /// Terminal-local image id used for storage and renderer diffing.
    pub id: u64,
    /// Kitty protocol image id, when this placement came from kitty graphics.
    pub kitty_image_id: Option<u32>,
    /// Kitty protocol placement id, unique only together with `kitty_image_id`.
    pub kitty_placement_id: Option<u32>,
    /// Absolute row index in `grid.rows` where the image top-left is placed.
    pub row: usize,
    /// Column position of the image top-left.
    pub col: u32,
    /// Final rendered pixel width. For sixel this matches `image.width`; for
    /// kitty this can differ when the app requested `c=` columns of display
    /// and the renderer scales the quad to fit.
    pub display_width: u32,
    /// Final rendered pixel height. For sixel this matches `image.height`;
    /// for kitty this can differ when the app requested `r=` rows of display.
    pub display_height: u32,
    /// Pixel offset from the left edge of the placement's first cell.
    pub cell_x_offset: u32,
    /// Pixel offset from the top edge of the placement's first cell.
    pub cell_y_offset: u32,
    /// Kitty z-index. Negative images render below text; zero and positive
    /// images render above text. Sixel/iTerm placements use zero.
    pub z_index: i32,
    /// Wall-clock timestamp of placement. Drives the animation clock for
    /// multi-frame images (`Instant::now() - placed_at` modulo
    /// `image.cycle_duration()` selects the current frame).
    pub placed_at: Instant,
}

/// A snapshot of an image visible in the current viewport.
pub struct VisibleImage {
    /// Decoded image pixels and frames.
    pub image: DecodedImage,
    /// Terminal-local image id.
    pub id: u64,
    /// Kitty protocol image id, when available.
    pub kitty_image_id: Option<u32>,
    /// Row of the image's top edge relative to the top of the viewport.
    /// Negative when the image's top is scrolled above the viewport; the
    /// renderer clips the image to the terminal content rectangle so only the
    /// visible terminal portion is drawn.
    pub screen_row: i32,
    /// Column position.
    pub screen_col: u32,
    /// Pixel offset from the placement cell.
    pub cell_x_offset: u32,
    /// Pixel offset from the placement cell.
    pub cell_y_offset: u32,
    /// Final rendered pixel width (see [`PlacedImage::display_width`]).
    pub display_width: u32,
    /// Final rendered pixel height (see [`PlacedImage::display_height`]).
    pub display_height: u32,
    /// Index into `image.frames` to render right now. Always `0` for static
    /// images; selected by [`DecodedImage::frame_at`] for animated ones.
    pub frame_index: usize,
    /// Vertical stacking order.
    pub z_index: i32,
}

/// Remove any existing image that would overlap a new image placed at
/// `(top_row, col)` spanning `height_rows` grid rows. Column match is
/// exact — sixel apps re-place images at the same column on redraw, and
/// requiring an exact col keeps two images at the same row but different
/// columns (tiled previews, side-by-side thumbnails) from clobbering each
/// other without needing a cell-width value the terminal doesn't track.
pub(super) fn remove_overlapping(
    images: &mut BTreeMap<u64, PlacedImage>,
    top_row: usize,
    height_rows: usize,
    col: u32,
    cell_height: u32,
) {
    let new_bottom = top_row + height_rows;
    images.retain(|_, img| {
        if img.col != col {
            return true;
        }
        let old_rows = img.display_height.div_ceil(cell_height).max(1) as usize;
        let old_bottom = img.row + old_rows;
        // Keep only if disjoint on rows (half-open intervals).
        old_bottom <= top_row || img.row >= new_bottom
    });
}

/// Translate images whose top row lies within `[abs_top, abs_bottom]` by
/// `delta` rows — the visible effect of a DECSTBM region scroll. Images
/// whose new top falls outside the region are removed, matching xterm's
/// "content scrolled out of the region is gone" behavior.
pub(super) fn shift_in_region(
    images: &mut BTreeMap<u64, PlacedImage>,
    abs_top: usize,
    abs_bottom: usize,
    delta: i64,
) {
    images.retain(|_, img| {
        if img.row < abs_top || img.row > abs_bottom {
            return true;
        }
        let new_row = img.row as i64 + delta;
        if new_row < abs_top as i64 || new_row > abs_bottom as i64 {
            return false;
        }
        img.row = new_row as usize;
        true
    });
}

/// Remove any image whose top row lies within `[start, end)`. Used by ED 2
/// and alt-screen clear to drop images that sit on cleared cells.
pub(super) fn clear_in_range(
    images: &mut BTreeMap<u64, PlacedImage>,
    start: usize,
    end: usize,
) {
    images.retain(|_, img| img.row < start || img.row >= end);
}

/// Remove images whose anchor cell lies inside a cleared cell range.
///
/// Rows are absolute indices in `grid.rows`; columns are half-open terminal
/// cell coordinates. Images are stored as independent placements, but their
/// lifetime follows the cell where the escape sequence placed them.
pub(super) fn clear_anchored_cells(
    images: &mut BTreeMap<u64, PlacedImage>,
    top_row: usize,
    bottom_row: usize,
    left_col: usize,
    right_col: usize,
) {
    if top_row >= bottom_row || left_col >= right_col {
        return;
    }

    images.retain(|_, img| {
        img.row < top_row
            || img.row >= bottom_row
            || img.col < left_col as u32
            || img.col >= right_col as u32
    });
}

/// Move image anchors left inside a rectangular cell range, matching a cell
/// copy-left operation. Anchors shifted out of the rectangle are removed.
pub(super) fn shift_anchored_cells_left(
    images: &mut BTreeMap<u64, PlacedImage>,
    top_row: usize,
    bottom_row: usize,
    left_col: usize,
    right_col: usize,
    count: usize,
) {
    if top_row >= bottom_row || left_col >= right_col || count == 0 {
        return;
    }

    let count = count.min(right_col - left_col);
    let removed_until = left_col + count;
    images.retain(|_, img| {
        if img.row < top_row || img.row >= bottom_row {
            return true;
        }
        let col = img.col as usize;
        if col < left_col || col >= right_col {
            return true;
        }
        if col < removed_until {
            return false;
        }
        img.col = (col - count) as u32;
        true
    });
}

/// Move image anchors right inside a rectangular cell range, matching a cell
/// copy-right operation. Anchors shifted out of the rectangle are removed.
pub(super) fn shift_anchored_cells_right(
    images: &mut BTreeMap<u64, PlacedImage>,
    top_row: usize,
    bottom_row: usize,
    left_col: usize,
    right_col: usize,
    count: usize,
) {
    if top_row >= bottom_row || left_col >= right_col || count == 0 {
        return;
    }

    let count = count.min(right_col - left_col);
    let kept_until = right_col - count;
    images.retain(|_, img| {
        if img.row < top_row || img.row >= bottom_row {
            return true;
        }
        let col = img.col as usize;
        if col < left_col || col >= right_col {
            return true;
        }
        if col >= kept_until {
            return false;
        }
        img.col = (col + count) as u32;
        true
    });
}

/// Move image anchors up inside a rectangular cell range, matching a
/// rectangular scroll-up operation.
pub(super) fn shift_anchored_cells_up(
    images: &mut BTreeMap<u64, PlacedImage>,
    top_row: usize,
    bottom_row: usize,
    left_col: usize,
    right_col: usize,
    count: usize,
) {
    if top_row >= bottom_row || left_col >= right_col || count == 0 {
        return;
    }

    let count = count.min(bottom_row - top_row);
    let removed_until = top_row + count;
    images.retain(|_, img| {
        if img.row < top_row || img.row >= bottom_row {
            return true;
        }
        let col = img.col as usize;
        if col < left_col || col >= right_col {
            return true;
        }
        if img.row < removed_until {
            return false;
        }
        img.row -= count;
        true
    });
}

/// Move image anchors down inside a rectangular cell range, matching a
/// rectangular scroll-down operation.
pub(super) fn shift_anchored_cells_down(
    images: &mut BTreeMap<u64, PlacedImage>,
    top_row: usize,
    bottom_row: usize,
    left_col: usize,
    right_col: usize,
    count: usize,
) {
    if top_row >= bottom_row || left_col >= right_col || count == 0 {
        return;
    }

    let count = count.min(bottom_row - top_row);
    let kept_until = bottom_row - count;
    images.retain(|_, img| {
        if img.row < top_row || img.row >= bottom_row {
            return true;
        }
        let col = img.col as usize;
        if col < left_col || col >= right_col {
            return true;
        }
        if img.row >= kept_until {
            return false;
        }
        img.row += count;
        true
    });
}

/// Save image positions as logical-line anchors that survive reflow.
///
/// Each image is mapped to (id, logical_lines_below, row_offset_in_line).
/// The count of hard line boundaries between the image and the grid end is
/// invariant through reflow, so it can be used to relocate the image after.
pub(super) fn anchor_images(
    rows: &VecDeque<Row>,
    images: &BTreeMap<u64, PlacedImage>,
) -> Vec<(u64, usize, usize)> {
    images
        .values()
        .map(|img| {
            let lines_below = (img.row + 1..rows.len())
                .filter(|&r| !rows[r].wrapped)
                .count();

            let mut row_offset = 0;
            let mut r = img.row;
            while r > 0 && rows[r].wrapped {
                row_offset += 1;
                r -= 1;
            }

            (img.id, lines_below, row_offset)
        })
        .collect()
}

/// Restore image row positions from logical-line anchors produced by
/// [`anchor_images`]. Images whose logical line was trimmed away are removed.
pub(super) fn restore_images(
    rows: &VecDeque<Row>,
    anchors: &[(u64, usize, usize)],
    images: &mut BTreeMap<u64, PlacedImage>,
) {
    for &(id, lines_below, row_offset) in anchors {
        let mut count = 0;
        let mut found = None;
        for r in (0..rows.len()).rev() {
            if r == 0 || !rows[r].wrapped {
                if count == lines_below {
                    found = Some(r);
                    break;
                }
                count += 1;
            }
        }

        match found {
            Some(start) => {
                let mut end = start + 1;
                while end < rows.len() && rows[end].wrapped {
                    end += 1;
                }
                let new_row = start + row_offset.min(end - start - 1);
                if let Some(img) = images.get_mut(&id) {
                    img.row = new_row;
                }
            }
            None => {
                images.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestTerm;

    fn place_image(
        term: &mut TestTerm,
        row: usize,
        col: u32,
        height_px: u32,
    ) -> u64 {
        let id = term.images.next_image_id;
        term.images.next_image_id += 1;
        term.active.images.insert(
            id,
            PlacedImage {
                image: DecodedImage::single_frame(1, height_px, vec![]),
                id,
                kitty_image_id: None,
                kitty_placement_id: None,
                row,
                col,
                display_width: 1,
                display_height: height_px,
                cell_x_offset: 0,
                cell_y_offset: 0,
                z_index: 0,
                placed_at: Instant::now(),
            },
        );
        id
    }

    #[test]
    fn sixel_redraw_at_same_position_replaces_previous() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let id_a = place_image(&mut term, 5, 0, 32);

        remove_overlapping(&mut term.active.images, 5, 2, 0, 16);

        assert!(!term.active.images.contains_key(&id_a));
    }

    #[test]
    fn sixel_different_columns_coexist() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let id_a = place_image(&mut term, 5, 0, 32);
        let id_b = place_image(&mut term, 5, 10, 32);

        remove_overlapping(&mut term.active.images, 5, 2, 0, 16);

        assert!(!term.active.images.contains_key(&id_a));
        assert!(term.active.images.contains_key(&id_b));
    }

    #[test]
    fn scroll_region_shifts_images_up() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        term.process(b"\x1b[1;8r");
        let id = place_image(&mut term, 5, 0, 16);
        term.process(b"\x1b[H");
        term.process(b"\x1b[2M");

        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 3, "image should shift up by 2 rows");
    }

    #[test]
    fn scroll_region_drops_image_pushed_out_of_top() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        term.process(b"\x1b[1;8r");
        let id = place_image(&mut term, 2, 0, 16);
        term.process(b"\x1b[H");
        term.process(b"\x1b[5M");

        assert!(
            !term.active.images.contains_key(&id),
            "image scrolled past region top should be dropped"
        );
    }

    #[test]
    fn scroll_region_preserves_images_outside_region() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        term.process(b"\x1b[2;5r");
        let id = place_image(&mut term, 8, 0, 16);
        term.process(b"\x1b[2H");
        term.process(b"\x1b[2M");

        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 8, "image outside region is unaffected");
    }

    #[test]
    fn ed_2_removes_visible_images() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 3, 0, 16);
        term.process(b"\x1b[2J");

        assert!(
            !term.active.images.contains_key(&id),
            "ED 2 should drop images on the visible area"
        );
    }

    #[test]
    fn el_removes_image_when_anchor_cell_is_cleared() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 3, 32);

        term.process(b"\x1b[6;4H\x1b[K");

        assert!(
            !term.active.images.contains_key(&id),
            "EL should drop an image whose anchor cell is erased"
        );
    }

    #[test]
    fn printing_removes_image_when_anchor_cell_is_overwritten() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 3, 32);

        term.process(b"\x1b[6;4Ha");

        assert!(
            !term.active.images.contains_key(&id),
            "printing over an image anchor cell should drop the image"
        );
    }

    #[test]
    fn insert_chars_moves_image_anchor_right() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 3, 16);

        term.process(b"\x1b[6;4H\x1b[2@");

        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.col, 5);
    }

    #[test]
    fn delete_chars_moves_image_anchor_left() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 5, 16);

        term.process(b"\x1b[6;4H\x1b[2P");

        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.col, 3);
    }

    #[test]
    fn insert_chars_drops_image_anchor_pushed_past_right_edge() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 9, 16);

        term.process(b"\x1b[6;9H\x1b[2@");

        assert!(
            !term.active.images.contains_key(&id),
            "ICH should drop anchors shifted past the row edge"
        );
    }

    #[test]
    fn insert_columns_moves_image_anchor_right() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 3, 16);

        term.process(b"\x1b[6;4H\x1b[2'}");

        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.col, 5);
    }

    #[test]
    fn delete_columns_moves_image_anchor_left() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 5, 16);

        term.process(b"\x1b[6;4H\x1b[2'~");

        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.col, 3);
    }

    #[test]
    fn el_keeps_image_when_only_covered_cells_are_cleared() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 5, 3, 32);

        term.process(b"\x1b[7;1H\x1b[2K");

        assert!(
            term.active.images.contains_key(&id),
            "clearing cells covered by image pixels should not drop the anchor"
        );
    }
}

use std::collections::BTreeMap;
use std::collections::VecDeque;

use crate::sixel::DecodedImage;
use crate::terminal::row::Row;

#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub image: DecodedImage,
    pub id: u64,
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
}

/// A reference to an image visible in the current viewport.
pub struct VisibleImage<'a> {
    pub image: &'a DecodedImage,
    pub id: u64,
    /// Row of the image's top edge relative to the top of the viewport.
    /// Negative when the image's top is scrolled above the viewport; the
    /// renderer emits a quad extending above the screen, which the GPU clips
    /// so only the visible portion is drawn.
    pub screen_row: i32,
    /// Column position.
    pub screen_col: u32,
    /// Final rendered pixel width (see [`PlacedImage::display_width`]).
    pub display_width: u32,
    /// Final rendered pixel height (see [`PlacedImage::display_height`]).
    pub display_height: u32,
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
        let old_rows = img.image.height.div_ceil(cell_height).max(1) as usize;
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

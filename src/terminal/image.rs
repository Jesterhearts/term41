use std::collections::BTreeMap;
use std::collections::VecDeque;

use crate::sixel::SixelImage;
use crate::terminal::row::Row;

#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub image: SixelImage,
    pub id: u64,
    /// Absolute row index in `grid.rows` where the image top-left is placed.
    pub row: usize,
    /// Column position of the image top-left.
    pub col: u32,
}

/// A reference to an image visible in the current viewport.
pub struct VisibleImage<'a> {
    pub image: &'a SixelImage,
    pub id: u64,
    /// Row of the image's top edge relative to the top of the viewport.
    /// Negative when the image's top is scrolled above the viewport; the
    /// renderer emits a quad extending above the screen, which the GPU clips
    /// so only the visible portion is drawn.
    pub screen_row: i32,
    /// Column position.
    pub screen_col: u32,
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

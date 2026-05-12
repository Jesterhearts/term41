use terminal41::RowSnapshot;

use super::FAILURE;
use super::RUNNING;
use super::RowGeometry;
use super::SUCCESS;
use super::push_rect;

pub fn compute_gutter_width(cell_width: u32) -> u32 {
    (cell_width / 3).max(12)
}

/// Paint a status bar for a prompt row. It spans most of the row height so the
/// prompt boundary is obvious at a glance, and leaves a small horizontal margin
/// so the coloured column doesn't butt up against col 0 of the text.
///
/// Colors:
///
/// * **Green** — command finished with exit `0`.
/// * **Red** — command finished with a non-zero exit code.
/// * **Gray** — prompt seen but no `D` yet: either the command is still
///   running, the shell doesn't emit `D`, or the command was superseded by the
///   next prompt before D arrived. All three look the same at the terminal
///   layer, so we show one "unknown" colour for all of them.
///
/// Drawn into the cached terminal row layer, not the dynamic frame overlay:
/// marker state changes with row contents, so caching it with the row avoids
/// rebuilding and uploading the whole gutter every output-heavy frame.
pub(super) fn append_gutter_marker(
    row: &RowSnapshot,
    gutter_px: f32,
    cell_h: f32,
    y: f32,
    geometry: &mut RowGeometry,
) {
    if gutter_px <= 0.0 || !row.prompt_start {
        return;
    }

    // Leave a small horizontal margin on both sides so the bar doesn't
    // touch either the window edge or the first text column.
    let bar_w = (gutter_px * 0.6).max(3.0);
    let bar_x = (gutter_px - bar_w) * 0.5;
    let bar_h = cell_h * 0.9;
    let bar_y = (cell_h - bar_h) * 0.5;
    let color = gutter_marker_color(row.exit_status);
    let y0 = y + bar_y;

    push_rect(
        bar_x,
        y0,
        bar_w,
        bar_h,
        color,
        &mut geometry.bg.vertices,
        &mut geometry.bg.indices,
    );
}

pub(super) fn gutter_marker_color(exit_status: Option<i32>) -> u32 {
    let rgb = match exit_status {
        Some(0) => SUCCESS,
        Some(_) => FAILURE,
        None => RUNNING,
    };
    u32::from_be_bytes([rgb[0], rgb[1], rgb[2], 255])
}

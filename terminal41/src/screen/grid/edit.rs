use std::collections::BTreeMap;

use crate::Row;
use crate::Viewport;
use crate::image::PlacedImage;
use crate::image::clear_anchored_cells;
use crate::image::clear_in_range;
use crate::image::shift_anchored_cells_left;
use crate::image::shift_anchored_cells_right;
use crate::screen::grid::Cursor;
use crate::screen::grid::Grid;
use crate::screen::row::LineAttr;

fn reset_row_after_full_clear(row: &mut Row) {
    row.wrapped = false;
    row.line_attr = LineAttr::Normal;
}

fn clear_row(
    grid: &mut Grid,
    row: usize,
) {
    grid.rows[row].clear_styled(
        grid.default_fg,
        grid.default_fg_source,
        grid.default_bg,
        grid.default_bg_source,
    );
}

fn clear_row_range(
    grid: &mut Grid,
    row: usize,
    range: std::ops::Range<usize>,
) {
    grid.rows[row].clear_range_styled(
        range,
        grid.default_fg,
        grid.default_fg_source,
        grid.default_bg,
        grid.default_bg_source,
    );
}

fn clear_row_selective(
    grid: &mut Grid,
    row: usize,
) {
    grid.rows[row].clear_selective_styled(
        grid.default_fg,
        grid.default_fg_source,
        grid.default_bg,
        grid.default_bg_source,
    );
}

fn clear_row_range_selective(
    grid: &mut Grid,
    row: usize,
    range: std::ops::Range<usize>,
) {
    grid.rows[row].clear_range_selective_styled(
        range,
        grid.default_fg,
        grid.default_fg_source,
        grid.default_bg,
        grid.default_bg_source,
    );
}

fn clear_wrapped_continuation_rows(
    grid: &mut Grid,
    images: &mut BTreeMap<u64, PlacedImage>,
    first: usize,
    cols: usize,
) {
    let mut row = first;
    while row < grid.rows.len() {
        let continued = grid.rows[row].wrapped;
        clear_row(grid, row);
        reset_row_after_full_clear(&mut grid.rows[row]);
        clear_anchored_cells(images, row, row + 1, 0, cols);
        row += 1;
        if !continued {
            break;
        }
    }
}

pub(crate) fn erase_in_display(
    grid: &mut Grid,
    cursor: &Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    mode: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let first_visible = viewport.top_index(grid.rows.len());
    let col = cursor.col as usize;
    let cols = viewport.cols as usize;

    match mode {
        0 => {
            let cols = grid.rows[active].cells.len();
            clear_row_range(grid, active, col..cols);
            grid.rows[active].wrapped = false;
            for r in (active + 1)..grid.rows.len() {
                clear_row(grid, r);
                reset_row_after_full_clear(&mut grid.rows[r]);
            }
            clear_anchored_cells(images, active, active + 1, col, cols);
            clear_anchored_cells(images, active + 1, grid.rows.len(), 0, cols);
        }
        1 => {
            for r in first_visible..active {
                clear_row(grid, r);
            }
            clear_row_range(grid, active, 0..col + 1);
            clear_anchored_cells(images, first_visible, active, 0, cols);
            clear_anchored_cells(images, active, active + 1, 0, col + 1);
        }
        2 => {
            for r in first_visible..grid.rows.len() {
                clear_row(grid, r);
                reset_row_after_full_clear(&mut grid.rows[r]);
            }
            clear_in_range(images, first_visible, grid.rows.len());
            clear_anchored_cells(images, first_visible, grid.rows.len(), 0, cols);
        }
        3 => {
            grid.total_popped += first_visible;
            grid.rows.drain(0..first_visible);
        }
        _ => {}
    }
}

pub(crate) fn erase_in_display_selective(
    grid: &mut Grid,
    cursor: &Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    mode: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let first_visible = viewport.top_index(grid.rows.len());
    let col = cursor.col as usize;
    let cols = viewport.cols as usize;

    match mode {
        0 => {
            let cols = grid.rows[active].cells.len();
            clear_row_range_selective(grid, active, col..cols);
            for r in (active + 1)..grid.rows.len() {
                clear_row_selective(grid, r);
            }
            clear_anchored_cells(images, active, active + 1, col, cols);
            clear_anchored_cells(images, active + 1, grid.rows.len(), 0, cols);
        }
        1 => {
            for r in first_visible..active {
                clear_row_selective(grid, r);
            }
            clear_row_range_selective(grid, active, 0..col + 1);
            clear_anchored_cells(images, first_visible, active, 0, cols);
            clear_anchored_cells(images, active, active + 1, 0, col + 1);
        }
        2 => {
            for r in first_visible..grid.rows.len() {
                clear_row_selective(grid, r);
            }
            clear_in_range(images, first_visible, grid.rows.len());
            clear_anchored_cells(images, first_visible, grid.rows.len(), 0, cols);
        }
        _ => {}
    }
}

pub(crate) fn erase_in_line_selective(
    grid: &mut Grid,
    cursor: &Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    mode: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;

    match mode {
        0 => {
            clear_row_range_selective(grid, active, col..cols);
            clear_anchored_cells(images, active, active + 1, col, cols);
        }
        1 => {
            clear_row_range_selective(grid, active, 0..col + 1);
            clear_anchored_cells(images, active, active + 1, 0, col + 1);
        }
        2 => {
            clear_row_selective(grid, active);
            clear_anchored_cells(images, active, active + 1, 0, cols);
        }
        _ => {}
    }
}

pub(crate) fn erase_in_line(
    grid: &mut Grid,
    cursor: &Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    mode: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;

    match mode {
        0 => {
            clear_row_range(grid, active, col..cols);
            clear_anchored_cells(images, active, active + 1, col, cols);
            if grid.rows[active].wrapped {
                grid.rows[active].wrapped = false;
                clear_wrapped_continuation_rows(grid, images, active + 1, cols);
            }
        }
        1 => {
            let end = col.saturating_add(1).min(cols);
            clear_row_range(grid, active, 0..end);
            clear_anchored_cells(images, active, active + 1, 0, end);
            if end == cols && grid.rows[active].wrapped {
                grid.rows[active].wrapped = false;
                clear_wrapped_continuation_rows(grid, images, active + 1, cols);
            }
        }
        2 => {
            let had_wrapped_continuation = grid.rows[active].wrapped;
            clear_row(grid, active);
            grid.rows[active].wrapped = false;
            clear_anchored_cells(images, active, active + 1, 0, cols);
            if had_wrapped_continuation {
                clear_wrapped_continuation_rows(grid, images, active + 1, cols);
            }
        }
        _ => {}
    }
}

pub(crate) fn delete_chars(
    grid: &mut Grid,
    cursor: &mut Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    n: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;
    let count = (n as usize).min(cols - col);

    grid.rows[active].copy_within(col + count..cols, col);
    clear_row_range(grid, active, cols - count..cols);
    shift_anchored_cells_left(images, active, active + 1, col, cols, count);
}

pub(crate) fn shift_chars(
    grid: &mut Grid,
    cursor: &mut Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    n: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;
    cursor.col = col as u32;
    let count = (n as usize).min(cols - col);

    grid.rows[active].copy_within(col..cols - count, col + count);
    clear_row_range(grid, active, col..col + count);
    shift_anchored_cells_right(images, active, active + 1, col, cols, count);
}

pub(crate) fn erase_chars(
    grid: &mut Grid,
    cursor: &mut Cursor,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    n: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;
    cursor.col = col as u32;
    let end = (col + n as usize).min(cols);

    clear_row_range(grid, active, col..end);
    clear_anchored_cells(images, active, active + 1, col, end);
}

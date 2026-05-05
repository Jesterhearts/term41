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

fn clear_wrapped_continuation_rows(
    grid: &mut Grid,
    images: &mut BTreeMap<u64, PlacedImage>,
    first: usize,
    cols: usize,
) {
    let mut row = first;
    while row < grid.rows.len() {
        let continued = grid.rows[row].wrapped;
        grid.rows[row].clear(grid.default_fg, grid.default_bg);
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
            grid.rows[active].clear_range(col..cols, grid.default_fg, grid.default_bg);
            grid.rows[active].wrapped = false;
            for r in (active + 1)..grid.rows.len() {
                grid.rows[r].clear(grid.default_fg, grid.default_bg);
                reset_row_after_full_clear(&mut grid.rows[r]);
            }
            clear_anchored_cells(images, active, active + 1, col, cols);
            clear_anchored_cells(images, active + 1, grid.rows.len(), 0, cols);
        }
        1 => {
            for r in first_visible..active {
                grid.rows[r].clear(grid.default_fg, grid.default_bg);
            }
            grid.rows[active].clear_range(0..col + 1, grid.default_fg, grid.default_bg);
            clear_anchored_cells(images, first_visible, active, 0, cols);
            clear_anchored_cells(images, active, active + 1, 0, col + 1);
        }
        2 => {
            for r in first_visible..grid.rows.len() {
                grid.rows[r].clear(grid.default_fg, grid.default_bg);
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
            grid.rows[active].clear_range_selective(col..cols, grid.default_fg, grid.default_bg);
            for r in (active + 1)..grid.rows.len() {
                grid.rows[r].clear_selective(grid.default_fg, grid.default_bg);
            }
            clear_anchored_cells(images, active, active + 1, col, cols);
            clear_anchored_cells(images, active + 1, grid.rows.len(), 0, cols);
        }
        1 => {
            for r in first_visible..active {
                grid.rows[r].clear_selective(grid.default_fg, grid.default_bg);
            }
            grid.rows[active].clear_range_selective(0..col + 1, grid.default_fg, grid.default_bg);
            clear_anchored_cells(images, first_visible, active, 0, cols);
            clear_anchored_cells(images, active, active + 1, 0, col + 1);
        }
        2 => {
            for r in first_visible..grid.rows.len() {
                grid.rows[r].clear_selective(grid.default_fg, grid.default_bg);
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
            grid.rows[active].clear_range_selective(col..cols, grid.default_fg, grid.default_bg);
            clear_anchored_cells(images, active, active + 1, col, cols);
        }
        1 => {
            grid.rows[active].clear_range_selective(0..col + 1, grid.default_fg, grid.default_bg);
            clear_anchored_cells(images, active, active + 1, 0, col + 1);
        }
        2 => {
            grid.rows[active].clear_selective(grid.default_fg, grid.default_bg);
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
            grid.rows[active].clear_range(col..cols, grid.default_fg, grid.default_bg);
            clear_anchored_cells(images, active, active + 1, col, cols);
            if grid.rows[active].wrapped {
                grid.rows[active].wrapped = false;
                clear_wrapped_continuation_rows(grid, images, active + 1, cols);
            }
        }
        1 => {
            let end = col.saturating_add(1).min(cols);
            grid.rows[active].clear_range(0..end, grid.default_fg, grid.default_bg);
            clear_anchored_cells(images, active, active + 1, 0, end);
            if end == cols && grid.rows[active].wrapped {
                grid.rows[active].wrapped = false;
                clear_wrapped_continuation_rows(grid, images, active + 1, cols);
            }
        }
        2 => {
            let had_wrapped_continuation = grid.rows[active].wrapped;
            grid.rows[active].clear(grid.default_fg, grid.default_bg);
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
    grid.rows[active].clear_range(cols - count..cols, grid.default_fg, grid.default_bg);
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
    grid.rows[active].clear_range(col..col + count, grid.default_fg, grid.default_bg);
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

    grid.rows[active].clear_range(col..end, grid.default_fg, grid.default_bg);
    clear_anchored_cells(images, active, active + 1, col, end);
}

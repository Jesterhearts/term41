use std::collections::BTreeMap;

use crate::Row;
use crate::Viewport;
use crate::image::PlacedImage;
use crate::image::clear_in_range;
use crate::screen::grid::Cursor;
use crate::screen::grid::Grid;
use crate::screen::row::LineAttr;

fn reset_row_after_full_clear(row: &mut Row) {
    row.wrapped = false;
    row.line_attr = LineAttr::Normal;
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

    match mode {
        0 => {
            let cols = grid.rows[active].cells.len();
            grid.rows[active].clear_range(col..cols, grid.default_fg, grid.default_bg);
            for r in (active + 1)..grid.rows.len() {
                grid.rows[r].clear(grid.default_fg, grid.default_bg);
            }
        }
        1 => {
            for r in first_visible..active {
                grid.rows[r].clear(grid.default_fg, grid.default_bg);
            }
            grid.rows[active].clear_range(0..col + 1, grid.default_fg, grid.default_bg);
        }
        2 => {
            for r in first_visible..grid.rows.len() {
                grid.rows[r].clear(grid.default_fg, grid.default_bg);
                reset_row_after_full_clear(&mut grid.rows[r]);
            }
            clear_in_range(images, first_visible, grid.rows.len());
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

    match mode {
        0 => {
            let cols = grid.rows[active].cells.len();
            grid.rows[active].clear_range_selective(col..cols, grid.default_fg, grid.default_bg);
            for r in (active + 1)..grid.rows.len() {
                grid.rows[r].clear_selective(grid.default_fg, grid.default_bg);
            }
        }
        1 => {
            for r in first_visible..active {
                grid.rows[r].clear_selective(grid.default_fg, grid.default_bg);
            }
            grid.rows[active].clear_range_selective(0..col + 1, grid.default_fg, grid.default_bg);
        }
        2 => {
            for r in first_visible..grid.rows.len() {
                grid.rows[r].clear_selective(grid.default_fg, grid.default_bg);
            }
            clear_in_range(images, first_visible, grid.rows.len());
        }
        _ => {}
    }
}

pub(crate) fn erase_in_line_selective(
    grid: &mut Grid,
    cursor: &Cursor,
    viewport: &Viewport,
    mode: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;

    match mode {
        0 => grid.rows[active].clear_range_selective(col..cols, grid.default_fg, grid.default_bg),
        1 => grid.rows[active].clear_range_selective(0..col + 1, grid.default_fg, grid.default_bg),
        2 => grid.rows[active].clear_selective(grid.default_fg, grid.default_bg),
        _ => {}
    }
}

pub(crate) fn erase_in_line(
    grid: &mut Grid,
    cursor: &Cursor,
    viewport: &Viewport,
    mode: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;

    match mode {
        0 => grid.rows[active].clear_range(col..cols, grid.default_fg, grid.default_bg),
        1 => grid.rows[active].clear_range(0..col + 1, grid.default_fg, grid.default_bg),
        2 => grid.rows[active].clear(grid.default_fg, grid.default_bg),
        _ => {}
    }
}

pub(crate) fn delete_chars(
    grid: &mut Grid,
    cursor: &mut Cursor,
    viewport: &Viewport,
    n: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;
    let count = (n as usize).min(cols - col);

    grid.rows[active].copy_within(col + count..cols, col);
    grid.rows[active].clear_range(cols - count..cols, grid.default_fg, grid.default_bg);
}

pub(crate) fn shift_chars(
    grid: &mut Grid,
    cursor: &mut Cursor,
    viewport: &Viewport,
    n: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;
    cursor.col = col as u32;
    let count = (n as usize).min(cols - col);

    grid.rows[active].copy_within(col..cols - count, col + count);
    grid.rows[active].clear_range(col..col + count, grid.default_fg, grid.default_bg);
}

pub(crate) fn erase_chars(
    grid: &mut Grid,
    cursor: &mut Cursor,
    viewport: &Viewport,
    n: u16,
) {
    let active = grid.active_row_index(cursor, viewport);
    let cols = grid.rows[active].cells.len();
    let col = cursor.col as usize;
    cursor.col = col as u32;
    let end = (col + n as usize).min(cols);

    grid.rows[active].clear_range(col..end, grid.default_fg, grid.default_bg);
}

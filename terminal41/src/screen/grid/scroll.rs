use std::collections::BTreeMap;

use super::*;
use crate::image::PlacedImage;
use crate::image::shift_in_region;

pub(crate) fn scroll_up_in_region(
    grid: &mut Grid,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    top: u32,
    bottom: u32,
    n: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let abs_top = first_visible + top as usize;
    let abs_bottom = first_visible + bottom as usize;
    let n = (n as usize).min(abs_bottom - abs_top + 1);
    for _ in 0..n {
        grid.rows.remove(abs_top);
        grid.rows.insert(
            abs_bottom,
            Row::new(viewport.cols, grid.default_fg, grid.default_bg),
        );
    }
    shift_in_region(images, abs_top, abs_bottom, -(n as i64));
}

pub(crate) fn scroll_up_in_region_with_scrollback_policy(
    grid: &mut Grid,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    top: u32,
    bottom: u32,
    n: u32,
    preserve_top_origin_scrollback: bool,
) {
    if !preserve_top_origin_scrollback || top != 0 {
        scroll_up_in_region(grid, viewport, images, top, bottom, n);
        return;
    }

    let first_visible = viewport.top_index(grid.rows.len());
    let abs_bottom = first_visible + bottom as usize;
    let n = (n as usize).min(bottom as usize + 1);
    let insert_at = abs_bottom + 1;
    for _ in 0..n {
        grid.rows.insert(
            insert_at,
            Row::new(viewport.cols, grid.default_fg, grid.default_bg),
        );
    }
    shift_images_after_insert(images, insert_at, n);
    trim_scrollback_to_limit(grid, viewport);
}

pub(crate) fn scroll_down_in_region(
    grid: &mut Grid,
    viewport: &Viewport,
    images: &mut BTreeMap<u64, PlacedImage>,
    top: u32,
    bottom: u32,
    n: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let abs_top = first_visible + top as usize;
    let abs_bottom = first_visible + bottom as usize;
    let n = (n as usize).min(abs_bottom - abs_top + 1);
    for _ in 0..n {
        grid.rows.remove(abs_bottom);
        grid.rows.insert(
            abs_top,
            Row::new(viewport.cols, grid.default_fg, grid.default_bg),
        );
    }
    shift_in_region(images, abs_top, abs_bottom, n as i64);
}

fn shift_images_after_insert(
    images: &mut BTreeMap<u64, PlacedImage>,
    row: usize,
    count: usize,
) {
    for image in images.values_mut() {
        if image.row >= row {
            image.row += count;
        }
    }
}

fn trim_scrollback_to_limit(
    grid: &mut Grid,
    viewport: &Viewport,
) {
    let max_rows = viewport.rows as usize + grid.scrollback_limit as usize;
    while max_rows > 0 && grid.rows.len() > max_rows {
        grid.rows.pop_front();
        grid.total_popped += 1;
    }
}

pub(crate) fn scroll_up_in_rect(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
    n: u32,
) {
    let first_visible = grid.rows.len() - viewport.rows as usize;
    let abs_top = first_visible + top as usize;
    let abs_bottom = first_visible + bottom as usize;
    let l = left as usize;
    let r = (right as usize + 1).min(grid.rows[abs_top].cells.len());
    let n = (n as usize).min(abs_bottom - abs_top + 1);

    for row in abs_top..=(abs_bottom - n) {
        let src = row + n;
        let cells: Vec<_> = grid.rows[src].cells[l..r].to_vec();
        let fg: Vec<_> = grid.rows[src].fg[l..r].to_vec();
        let bg: Vec<_> = grid.rows[src].bg[l..r].to_vec();
        let attrs: Vec<_> = grid.rows[src].attrs[l..r].to_vec();
        let ul: Vec<_> = grid.rows[src].underline[l..r].to_vec();
        let ul_color: Vec<_> = grid.rows[src].underline_color[l..r].to_vec();
        let links: Vec<_> = grid.rows[src].links[l..r].to_vec();

        grid.rows[row].cells[l..r].clone_from_slice(&cells);
        grid.rows[row].fg[l..r].copy_from_slice(&fg);
        grid.rows[row].bg[l..r].copy_from_slice(&bg);
        grid.rows[row].attrs[l..r].copy_from_slice(&attrs);
        grid.rows[row].underline[l..r].copy_from_slice(&ul);
        grid.rows[row].underline_color[l..r].copy_from_slice(&ul_color);
        grid.rows[row].links[l..r].clone_from_slice(&links);
    }

    for row in (abs_bottom - n + 1)..=abs_bottom {
        grid.rows[row].clear_range(l..r, grid.default_fg, grid.default_bg);
    }
}

pub(crate) fn scroll_down_in_rect(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
    n: u32,
) {
    let first_visible = grid.rows.len() - viewport.rows as usize;
    let abs_top = first_visible + top as usize;
    let abs_bottom = first_visible + bottom as usize;
    let l = left as usize;
    let r = (right as usize + 1).min(grid.rows[abs_top].cells.len());
    let n = (n as usize).min(abs_bottom - abs_top + 1);

    for row in ((abs_top + n)..=abs_bottom).rev() {
        let src = row - n;
        let cells: Vec<_> = grid.rows[src].cells[l..r].to_vec();
        let fg: Vec<_> = grid.rows[src].fg[l..r].to_vec();
        let bg: Vec<_> = grid.rows[src].bg[l..r].to_vec();
        let attrs: Vec<_> = grid.rows[src].attrs[l..r].to_vec();
        let ul: Vec<_> = grid.rows[src].underline[l..r].to_vec();
        let ul_color: Vec<_> = grid.rows[src].underline_color[l..r].to_vec();
        let links: Vec<_> = grid.rows[src].links[l..r].to_vec();

        grid.rows[row].cells[l..r].clone_from_slice(&cells);
        grid.rows[row].fg[l..r].copy_from_slice(&fg);
        grid.rows[row].bg[l..r].copy_from_slice(&bg);
        grid.rows[row].attrs[l..r].copy_from_slice(&attrs);
        grid.rows[row].underline[l..r].copy_from_slice(&ul);
        grid.rows[row].underline_color[l..r].copy_from_slice(&ul_color);
        grid.rows[row].links[l..r].clone_from_slice(&links);
    }

    for row in abs_top..(abs_top + n) {
        grid.rows[row].clear_range(l..r, grid.default_fg, grid.default_bg);
    }
}

pub(crate) fn scroll_left(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    bottom: u32,
    n: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let cols = viewport.cols as usize;
    let n = (n as usize).min(cols);
    if n == 0 {
        return;
    }
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        grid.rows[abs].copy_within(n..cols, 0);
        grid.rows[abs].clear_range(cols - n..cols, grid.default_fg, grid.default_bg);
    }
}

pub(crate) fn scroll_right(
    grid: &mut Grid,
    viewport: &Viewport,
    top: u32,
    bottom: u32,
    n: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let cols = viewport.cols as usize;
    let n = (n as usize).min(cols);
    if n == 0 {
        return;
    }
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        grid.rows[abs].copy_within(0..cols - n, n);
        grid.rows[abs].clear_range(0..n, grid.default_fg, grid.default_bg);
    }
}

pub(crate) fn insert_cols(
    grid: &mut Grid,
    viewport: &Viewport,
    cursor_col: u32,
    top: u32,
    bottom: u32,
    n: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let cols = viewport.cols as usize;
    let col = cursor_col as usize;
    let n = (n as usize).min(cols - col);
    if n == 0 {
        return;
    }
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        grid.rows[abs].copy_within(col..cols - n, col + n);
        grid.rows[abs].clear_range(col..col + n, grid.default_fg, grid.default_bg);
    }
}

pub(crate) fn delete_cols(
    grid: &mut Grid,
    viewport: &Viewport,
    cursor_col: u32,
    top: u32,
    bottom: u32,
    n: u32,
) {
    let first_visible = viewport.top_index(grid.rows.len());
    let cols = viewport.cols as usize;
    let col = cursor_col as usize;
    let n = (n as usize).min(cols - col);
    if n == 0 {
        return;
    }
    for r in top..=bottom {
        let abs = first_visible + r as usize;
        grid.rows[abs].copy_within(col + n..cols, col);
        grid.rows[abs].clear_range(cols - n..cols, grid.default_fg, grid.default_bg);
    }
}

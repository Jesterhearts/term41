use super::*;

pub(crate) fn total_rows(
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    viewport.rows + screen::status_line_rows(screen)
}

pub(crate) fn status_line_visible(screen: &Screen) -> bool {
    screen::status_line_visible(screen)
}

pub(crate) fn status_line_row(screen: &Screen) -> Option<&Row> {
    screen.status_line.as_ref().map(|status| &status.row)
}

pub(crate) fn status_line_cursor_col(screen: &Screen) -> Option<u32> {
    (screen.active_display == screen::ActiveDisplay::Status && screen.cursor_visible)
        .then_some(screen.status_line.as_ref()?.cursor.col)
}

pub(crate) fn set_default_status_display(
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    palette: &ColorPalette,
    default_status_display: &mut StatusDisplayKind,
    status_display: StatusDisplayKind,
) {
    *default_status_display = status_display;
    let total_rows = total_rows(active, viewport);
    let cols = viewport.cols;
    viewport.rows =
        feature::apply_status_display_mode(active, total_rows, cols, status_display, palette);
    feature::apply_status_display_mode(stash, total_rows, cols, status_display, palette);
}

pub(crate) fn scroll_viewport_up(
    screen: &mut Screen,
    viewport: &Viewport,
    lines: u32,
) -> u32 {
    if screen::page_memory_active(screen) {
        return 0;
    }
    let max = screen.grid.scrollback_len(viewport);
    let delta = lines.min(max.saturating_sub(screen.offset));
    screen.offset += delta;
    delta
}

pub(crate) fn scroll_to_prev_prompt(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    let top = selection::screen_row_to_absolute(screen, viewport, 0);
    let popped = screen.grid.total_popped as u64;
    let target = screen
        .grid
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.prompt_start)
        .map(|(i, _)| popped + i as u64)
        .filter(|&r| r < top)
        .max();
    if let Some(target) = target {
        scroll_absolute_to_viewport_top(screen, viewport, target);
    }
}

pub(crate) fn scroll_to_next_prompt(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    let top = selection::screen_row_to_absolute(screen, viewport, 0);
    let popped = screen.grid.total_popped as u64;
    let target = screen
        .grid
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.prompt_start)
        .map(|(i, _)| popped + i as u64)
        .find(|&r| r > top);
    if let Some(target) = target {
        scroll_absolute_to_viewport_top(screen, viewport, target);
    }
}

fn scroll_absolute_to_viewport_top(
    screen: &mut Screen,
    viewport: &Viewport,
    target_abs: u64,
) {
    if screen::page_memory_active(screen) {
        return;
    }
    let popped = screen.grid.total_popped as u64;
    let Some(target_local) = target_abs.checked_sub(popped) else {
        return;
    };
    let grid_len = screen.grid.rows.len();
    let rows = viewport.rows as usize;
    if grid_len <= rows || (target_local as usize) >= grid_len {
        screen.offset = 0;
        return;
    }
    let max_top = grid_len - rows;
    let top = (target_local as usize).min(max_top);
    let offset = (grid_len - rows - top) as u32;
    let max_offset = screen.grid.scrollback_len(viewport);
    screen.offset = offset.min(max_offset);
}

pub(crate) fn scroll_viewport_down(
    screen: &mut Screen,
    lines: u32,
) -> u32 {
    if screen::page_memory_active(screen) {
        return 0;
    }
    let delta = lines.min(screen.offset);
    screen.offset -= delta;
    delta
}

pub(crate) fn reset_viewport(screen: &mut Screen) {
    if screen::page_memory_active(screen) {
        return;
    }
    screen.offset = 0;
}

pub(crate) fn visible_images<'a>(
    screen: &'a Screen,
    viewport: &Viewport,
    cell_height: u32,
    now: Instant,
) -> impl Iterator<Item = VisibleImage<'a>> {
    let view = selection::active_viewport(screen, viewport);
    let viewport_top = view.top_index(screen.grid.rows.len());
    let viewport_bottom = viewport_top + view.rows as usize;

    screen.images.values().filter_map(move |img| {
        let img_rows = img.display_height.div_ceil(cell_height).max(1) as usize;
        let img_bottom = img.row + img_rows;
        if img.row < viewport_bottom && img_bottom > viewport_top {
            let elapsed = now.saturating_duration_since(img.placed_at);
            Some(VisibleImage {
                image: &img.image,
                id: img.id,
                screen_row: img.row as i32 - viewport_top as i32,
                screen_col: img.col,
                display_width: img.display_width,
                display_height: img.display_height,
                frame_index: img.image.frame_at(elapsed),
            })
        } else {
            None
        }
    })
}

pub(crate) fn resize(
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    cols: u32,
    rows: u32,
) {
    let old_cols = viewport.cols;
    let old_active_rows = viewport.rows;
    let old_total_rows = old_active_rows + screen::status_line_rows(active);
    let old_stash_rows = old_total_rows.saturating_sub(screen::status_line_rows(stash));
    let new_active_rows = rows.saturating_sub(screen::status_line_rows(active));
    let new_stash_rows = rows.saturating_sub(screen::status_line_rows(stash));

    resize_screen(active, old_cols, old_active_rows, cols, new_active_rows);
    if screen::page_memory_active(active)
        && let Some(page_rows) = screen::page_rows(active)
    {
        screen::resize_page_memory(
            active,
            &Viewport {
                rows: new_active_rows,
                cols,
                top: 0,
            },
            page_rows,
        );
    }

    resize_screen(stash, old_cols, old_stash_rows, cols, new_stash_rows);
    if screen::page_memory_active(stash)
        && let Some(page_rows) = screen::page_rows(stash)
    {
        screen::resize_page_memory(
            stash,
            &Viewport {
                rows: new_stash_rows,
                cols,
                top: 0,
            },
            page_rows,
        );
    }

    viewport.cols = cols;
    viewport.rows = new_active_rows;
}

pub(crate) fn track_scroll(
    screen: &mut Screen,
    command_metas: &mut HashMap<u64, CommandMeta>,
    popped_before: usize,
) {
    let newly_popped = screen.grid.total_popped.saturating_sub(popped_before);
    if newly_popped > 0 {
        screen.images.retain(|_, img| img.row >= newly_popped);
        for img in screen.images.values_mut() {
            img.row -= newly_popped;
        }
        let min_abs = screen.grid.total_popped as u64;
        command_metas.retain(|&abs, _| abs >= min_abs);
    }
}

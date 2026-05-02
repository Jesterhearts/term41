use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Instant;

use crate::ColorPalette;
use crate::CommandMeta;
use crate::Row;
use crate::Screen;
use crate::StatusDisplayKind;
use crate::Viewport;
use crate::VisibleImage;
use crate::color::palette_color;
use crate::feature;
use crate::image::KITTY_UNICODE_PLACEHOLDER;
use crate::resize_screen;
use crate::screen;
use crate::screen::ResizeScreenOutcome;
use crate::selection;

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

pub(crate) fn visible_images(
    screen: &Screen,
    viewport: &Viewport,
    cell_height: u32,
    cell_width: u32,
    kitty_images: &image41::kitty::KittyImageStore,
    palette: &ColorPalette,
    now: Instant,
) -> impl Iterator<Item = VisibleImage> {
    let view = selection::active_viewport(screen, viewport);
    let viewport_top = view.top_index(screen.grid.rows.len());
    let viewport_bottom = viewport_top + view.rows as usize;

    let mut visible =
        visible_physical_images(screen, cell_height, viewport_top, viewport_bottom, now);
    append_unicode_placeholder_images(
        &mut visible,
        screen,
        view,
        cell_height,
        cell_width,
        kitty_images,
        palette,
        now,
    );
    visible.sort_by_key(visible_image_draw_order);
    visible.into_iter()
}

pub(crate) fn referenced_kitty_image_ids(
    screen: &Screen,
    kitty_images: &image41::kitty::KittyImageStore,
    palette: &ColorPalette,
) -> HashSet<u32> {
    let mut referenced = HashSet::new();
    for img in screen.images.values() {
        if let Some(image_id) = img.kitty_image_id {
            referenced.insert(image_id);
        }
    }
    append_placeholder_references(&mut referenced, screen, kitty_images, palette);
    referenced
}

fn visible_image_draw_order(img: &VisibleImage) -> (i32, i32, u32, u32, u64) {
    // Protocol z-index still chooses the image layer; page position decides
    // overlap order inside that layer so lower/rightward anchors draw last.
    (
        img.z_index,
        img.screen_row,
        img.screen_col,
        img.kitty_image_id.unwrap_or(u32::MAX),
        img.id,
    )
}

fn visible_physical_images(
    screen: &Screen,
    cell_height: u32,
    viewport_top: usize,
    viewport_bottom: usize,
    now: Instant,
) -> Vec<VisibleImage> {
    screen
        .images
        .values()
        .filter_map(move |img| {
            let img_rows = img.display_height.div_ceil(cell_height).max(1) as usize;
            let img_bottom = img.row + img_rows;
            if img.row >= viewport_bottom || img_bottom <= viewport_top {
                return None;
            }
            let elapsed = now.saturating_duration_since(img.placed_at);
            Some(VisibleImage {
                image: img.image.clone(),
                id: img.id,
                kitty_image_id: img.kitty_image_id,
                screen_row: img.row as i32 - viewport_top as i32,
                screen_col: img.col,
                cell_x_offset: img.cell_x_offset,
                cell_y_offset: img.cell_y_offset,
                display_width: img.display_width,
                display_height: img.display_height,
                frame_index: img.image.frame_at(elapsed),
                z_index: img.z_index,
            })
        })
        .collect()
}

fn append_placeholder_references(
    referenced: &mut HashSet<u32>,
    screen: &Screen,
    kitty_images: &image41::kitty::KittyImageStore,
    palette: &ColorPalette,
) {
    for row in &screen.grid.rows {
        let mut previous: Option<PlaceholderCell> = None;
        for col in 0..row.cells.len() {
            let Some(cell) = decode_placeholder_cell(row, col, previous, palette) else {
                previous = None;
                continue;
            };
            previous = Some(cell);

            if let Some((image_id, _, _)) = resolve_placeholder_image(kitty_images, cell) {
                referenced.insert(image_id);
            }
        }
    }
}

fn append_unicode_placeholder_images(
    visible: &mut Vec<VisibleImage>,
    screen: &Screen,
    viewport: Viewport,
    cell_height: u32,
    cell_width: u32,
    kitty_images: &image41::kitty::KittyImageStore,
    palette: &ColorPalette,
    now: Instant,
) {
    let viewport_top = viewport.top_index(screen.grid.rows.len());
    let viewport_bottom = viewport_top + viewport.rows as usize;
    let mut emitted = HashSet::new();

    for row_index in viewport_top..viewport_bottom.min(screen.grid.rows.len()) {
        let row = &screen.grid.rows[row_index];
        let mut previous: Option<PlaceholderCell> = None;
        for col in 0..row.cells.len().min(viewport.cols as usize) {
            let Some(cell) = decode_placeholder_cell(row, col, previous, palette) else {
                previous = None;
                continue;
            };
            previous = Some(cell);

            let Some((image_id, placement, image)) = resolve_placeholder_image(kitty_images, cell)
            else {
                continue;
            };
            if placement.columns != 0 && cell.col >= placement.columns {
                continue;
            }
            if placement.rows != 0 && cell.row >= placement.rows {
                continue;
            }
            if cell.row as usize > row_index || cell.col as usize > col {
                continue;
            }

            let anchor_row = row_index - cell.row as usize;
            let anchor_col = col as u32 - cell.col;
            if !emitted.insert((image_id, placement.placement_id, anchor_row, anchor_col)) {
                continue;
            }

            let image = virtual_source_image(image, placement);
            let (display_width, display_height) =
                virtual_display_size(&image, placement, cell_width, cell_height);
            if display_width == 0 || display_height == 0 {
                continue;
            }
            let img_rows = display_height.div_ceil(cell_height).max(1) as usize;
            if anchor_row >= viewport_bottom || anchor_row + img_rows <= viewport_top {
                continue;
            }
            let elapsed = now.saturating_duration_since(placement.created_at);
            let frame_index = image.frame_at(elapsed);
            visible.push(VisibleImage {
                image,
                id: virtual_visible_image_id(
                    image_id,
                    kitty_images.image_generation(image_id),
                    placement.generation,
                    placement.placement_id,
                ),
                kitty_image_id: Some(image_id),
                screen_row: anchor_row as i32 - viewport_top as i32,
                screen_col: anchor_col,
                cell_x_offset: placement.cell_x_offset,
                cell_y_offset: placement.cell_y_offset,
                display_width,
                display_height,
                frame_index,
                z_index: placement.z_index,
            });
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PlaceholderCell {
    row: u32,
    col: u32,
    image_id_low_candidates: [u32; 257],
    image_id_low_count: usize,
    placement_id_candidates: [u32; 257],
    placement_id_count: usize,
    image_id_msb: u32,
    fg: palette::Srgb<u8>,
    underline: Option<palette::Srgb<u8>>,
}

fn resolve_placeholder_image(
    kitty_images: &image41::kitty::KittyImageStore,
    cell: PlaceholderCell,
) -> Option<(
    u32,
    &image41::kitty::KittyVirtualPlacement,
    &image41::DecodedImage,
)> {
    for image_low in &cell.image_id_low_candidates[..cell.image_id_low_count] {
        let image_id = image_low.saturating_add(cell.image_id_msb << 24);
        for placement_id in &cell.placement_id_candidates[..cell.placement_id_count] {
            if let Some(placement) = kitty_images.virtual_placement(image_id, *placement_id)
                && let Some(image) = kitty_images.get(image_id)
            {
                return Some((image_id, placement, image));
            }
        }
    }
    None
}

fn decode_placeholder_cell(
    row: &Row,
    col: usize,
    previous: Option<PlaceholderCell>,
    palette: &ColorPalette,
) -> Option<PlaceholderCell> {
    let cell = row.cells.get(col)?;
    let mut chars = cell.chars();
    if chars.next() != Some(KITTY_UNICODE_PLACEHOLDER) {
        return None;
    }

    let mut marks = [None; 3];
    for (idx, ch) in chars.filter_map(kitty_diacritic_value).take(3).enumerate() {
        marks[idx] = Some(ch as u32);
    }

    let fg = row.fg[col];
    let underline = row.underline_color[col];
    let same_style = previous.is_some_and(|prev| prev.fg == fg && prev.underline == underline);
    let mut image_id_low_candidates = [0; 257];
    let image_id_low_count = color_id_candidates(fg, palette, &mut image_id_low_candidates);
    let mut placement_id_candidates = [0; 257];
    let placement_id_count = if let Some(color) = underline {
        color_id_candidates(color, palette, &mut placement_id_candidates)
    } else {
        placement_id_candidates[0] = 0;
        1
    };

    let mut decoded = PlaceholderCell {
        row: marks[0].unwrap_or(0),
        col: marks[1].unwrap_or(0),
        image_id_low_candidates,
        image_id_low_count,
        placement_id_candidates,
        placement_id_count,
        image_id_msb: marks[2].unwrap_or(0),
        fg,
        underline,
    };

    if let Some(prev) = previous.filter(|_| same_style) {
        match (marks[0], marks[1], marks[2]) {
            (None, None, None) => {
                decoded.row = prev.row;
                decoded.col = prev.col + 1;
                decoded.image_id_msb = prev.image_id_msb;
            }
            (Some(row), None, None) if row == prev.row => {
                decoded.col = prev.col + 1;
                decoded.image_id_msb = prev.image_id_msb;
            }
            (Some(row), Some(col), None) if row == prev.row && col == prev.col + 1 => {
                decoded.image_id_msb = prev.image_id_msb;
            }
            _ => {}
        }
    }

    Some(decoded)
}

fn color_id_candidates(
    color: palette::Srgb<u8>,
    palette: &ColorPalette,
    out: &mut [u32; 257],
) -> usize {
    let mut len = 0;
    let rgb_id = ((color.red as u32) << 16) | ((color.green as u32) << 8) | color.blue as u32;
    out[len] = rgb_id;
    len += 1;
    for index in 0..=u8::MAX {
        if palette_color(palette, index) == color {
            out[len] = index as u32;
            len += 1;
        }
    }
    len
}

fn virtual_display_size(
    image: &image41::DecodedImage,
    placement: &image41::kitty::KittyVirtualPlacement,
    cell_width: u32,
    cell_height: u32,
) -> (u32, u32) {
    match (placement.columns > 0, placement.rows > 0) {
        (true, true) => (placement.columns * cell_width, placement.rows * cell_height),
        (true, false) => {
            let width = placement.columns * cell_width;
            let height = if image.width > 0 {
                (image.height as u64 * width as u64 / image.width as u64) as u32
            } else {
                image.height
            };
            (width, height)
        }
        (false, true) => {
            let height = placement.rows * cell_height;
            let width = if image.height > 0 {
                (image.width as u64 * height as u64 / image.height as u64) as u32
            } else {
                image.width
            };
            (width, height)
        }
        (false, false) => (image.width, image.height),
    }
}

fn virtual_source_image(
    image: &image41::DecodedImage,
    placement: &image41::kitty::KittyVirtualPlacement,
) -> image41::DecodedImage {
    let cmd = image41::kitty::KittyCommand {
        src_x: placement.src_x,
        src_y: placement.src_y,
        src_w: placement.src_w,
        src_h: placement.src_h,
        ..Default::default()
    };
    image41::kitty::crop_source_rect(image.clone(), &cmd)
}

fn virtual_visible_image_id(
    image_id: u32,
    image_generation: u64,
    placement_generation: u64,
    placement_id: u32,
) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        image_id as u64,
        image_generation,
        placement_generation,
        placement_id as u64,
    ] {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | (1_u64 << 63)
}

fn kitty_diacritic_value(ch: char) -> Option<u8> {
    KITTY_DIACRITICS
        .iter()
        .position(|&candidate| candidate == ch)
        .map(|idx| idx as u8)
}

const KITTY_DIACRITICS: [char; 256] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}', '\u{06D7}', '\u{06D8}',
    '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}', '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}',
    '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}',
    '\u{082D}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}',
];

pub(crate) fn resize(
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    cols: u32,
    rows: u32,
) -> ResizeOutcome {
    let old_cols = viewport.cols;
    let old_active_rows = viewport.rows;
    let old_total_rows = old_active_rows + screen::status_line_rows(active);
    let old_stash_rows = old_total_rows.saturating_sub(screen::status_line_rows(stash));
    let new_active_rows = rows.saturating_sub(screen::status_line_rows(active));
    let new_stash_rows = rows.saturating_sub(screen::status_line_rows(stash));

    let active_outcome = resize_screen(active, old_cols, old_active_rows, cols, new_active_rows);
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

    let stash_outcome = resize_screen(stash, old_cols, old_stash_rows, cols, new_stash_rows);
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

    ResizeOutcome {
        active: active_outcome,
        stash: stash_outcome,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ResizeOutcome {
    pub active: ResizeScreenOutcome,
    pub stash: ResizeScreenOutcome,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn visible_image(
        id: u64,
        screen_row: i32,
        screen_col: u32,
        z_index: i32,
    ) -> VisibleImage {
        VisibleImage {
            image: image41::DecodedImage::single_frame(1, 1, vec![0, 0, 0, 255]),
            id,
            kitty_image_id: None,
            screen_row,
            screen_col,
            cell_x_offset: 0,
            cell_y_offset: 0,
            display_width: 1,
            display_height: 1,
            frame_index: 0,
            z_index,
        }
    }

    #[test]
    fn visible_images_draw_in_row_major_order_within_z_index() {
        let mut images = [
            visible_image(1, 2, 10, 0),
            visible_image(2, 3, 0, 0),
            visible_image(3, 2, 5, 0),
            visible_image(4, 1, 20, 0),
        ];

        images.sort_by_key(visible_image_draw_order);

        let ids: Vec<u64> = images.iter().map(|image| image.id).collect();
        assert_eq!(ids, vec![4, 3, 1, 2]);
    }

    #[test]
    fn visible_images_keep_protocol_z_index_primary() {
        let mut images = [visible_image(1, 10, 0, 0), visible_image(2, 0, 0, 1)];

        images.sort_by_key(visible_image_draw_order);

        let ids: Vec<u64> = images.iter().map(|image| image.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }
}

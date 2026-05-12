use std::sync::Arc;

use config41::CursorShape;
use font41::FontSystem;
use font41::attrs::CellAttrs;
use smol_str::SmolStr;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use terminal41::VisibleImage;

use super::BELL_FLASH_DURATION;
use super::BELL_FLASH_PEAK_ALPHA;
use super::BgVertex;
use super::CachedRowKey;
use super::ClipRect;
use super::CursorRenderState;
use super::FgVertex;
use super::FrameLayout;
use super::GeometryUpload;
use super::ImageGeometry;
use super::ImageQuad;
use super::ImageVertex;
use super::PageGeometryUpload;
use super::RenderGeometry;
use super::Renderer;
use super::RowGeometry;
use super::RowGutterMarkerKey;
use super::RowLayoutKey;
use super::RowRenderKey;
use super::TabInfo;
use super::TerminalLayer;
use super::WindowControls;
use super::append_cached_row_geometry;
use super::append_gutter_marker;
use super::blank_cached_row;
use super::cached_rows_match_snapshot_shape;
use super::chrome::cursor_state_from_snapshot;
use super::chrome::render_command_editor;
use super::chrome::render_command_palette;
use super::chrome::render_gutter_popup;
use super::chrome::render_history_confirmation_modal;
use super::chrome::render_history_deletion;
use super::chrome::render_permission_modal;
use super::chrome::render_preedit;
use super::chrome::render_recording_popup;
use super::chrome::render_search_bar;
use super::chrome::render_status_line_chrome;
use super::chrome::render_tab_bar;
use super::chrome::render_toast;
use super::clip_image_quad;
use super::collect_row_glyphs;
use super::dirty_rect_clear_geometry;
use super::drcs_geometry_class;
use super::gutter_fill_bg_for_col0;
use super::image_batch_for_page;
use super::image_render_order;
use super::image_vertex_z;
use super::invalidate_row_cache_with_neighbors;
use super::pack_color;
use super::push_fg_quad;
use super::push_rect;
use super::push_terminal_area_dirty_rect;
use super::push_terminal_dirty_rect;
use super::push_underline_quads;
use super::resolve_painted_cell;
use super::row_blink_key;
use super::row_cursor_key;
use super::row_hidden_by_sticky_prompt;
use super::row_popup_clip_key;
use super::row_suspended_by_terminal_area;
use super::snapshot_row_y;
use super::underline_style_for_render;
use super::upload_fg_geometry;
use super::upload_image_geometry;
use super::visible_row_cols;
use crate::APP_START_TIME;
use crate::renderer::GUTTER_MENU_ITEMS;
use crate::renderer::GutterPopup;
use crate::renderer::POPUP_WIDTH_CELLS;
use crate::renderer::glyph_atlas::GlyphAtlas;
use crate::renderer::gutter_popup_origin;
use crate::renderer::image_atlas::IMAGE_ATLAS_SIZE;

pub(super) fn apply_terminal_snapshot_rows(
    renderer: &mut Renderer,
    snap: &TermSnapshot,
    block_y_offset_rows: u32,
) {
    let total_rows = snap
        .rows
        .iter()
        .map(|row| row.screen_row as usize + 1)
        .max()
        .unwrap_or(0);
    if snap.reset_cached_rows
        || renderer.terminal_rows.len() != total_rows
        || renderer.terminal_block_y_offset_rows != block_y_offset_rows
        || !cached_rows_match_snapshot_shape(&renderer.terminal_rows, snap)
    {
        renderer.terminal_rows = (0..total_rows)
            .map(|row| blank_cached_row(row as u32, snap.viewport_cols, &snap.palette))
            .collect();
        renderer.terminal_row_generations = vec![u64::MAX; total_rows];
        renderer.row_geometry_cache.clear();
        renderer.row_geometry_cache.resize_with(total_rows, || None);
        renderer.terminal_layer.needs_full_repaint = true;
        renderer.terminal_block_y_offset_rows = block_y_offset_rows;
    } else if renderer.row_geometry_cache.len() != total_rows {
        renderer
            .terminal_row_generations
            .resize(total_rows, u64::MAX);
        renderer.row_geometry_cache.clear();
        renderer.row_geometry_cache.resize_with(total_rows, || None);
        renderer.terminal_layer.needs_full_repaint = true;
    }

    for row in &snap.rows {
        let idx = row.screen_row as usize;
        if idx >= renderer.terminal_rows.len() {
            renderer.terminal_rows.resize_with(idx + 1, || {
                blank_cached_row(0, snap.viewport_cols, &snap.palette)
            });
            renderer.terminal_rows[idx].screen_row = idx as u32;
            renderer.terminal_row_generations.resize(idx + 1, u64::MAX);
            renderer.row_geometry_cache.resize_with(idx + 1, || None);
        }
        if renderer
            .terminal_row_generations
            .get(idx)
            .is_some_and(|generation| *generation == row.generation)
        {
            continue;
        }
        renderer.terminal_rows[idx] = row.clone();
        renderer.terminal_row_generations[idx] = row.generation;
        invalidate_row_cache_with_neighbors(&mut renderer.row_geometry_cache, idx);
    }
}

pub(super) fn apply_terminal_snapshot_status_row(
    renderer: &mut Renderer,
    snap: &TermSnapshot,
) {
    let Some(status_row_idx) = snap.status_line_row.map(|row| row as usize) else {
        return;
    };
    let Some(row) = snap
        .rows
        .iter()
        .find(|row| row.screen_row as usize == status_row_idx)
    else {
        return;
    };
    if status_row_idx >= renderer.terminal_rows.len() {
        renderer.terminal_rows.resize_with(status_row_idx + 1, || {
            blank_cached_row(0, snap.viewport_cols, &snap.palette)
        });
        renderer
            .terminal_row_generations
            .resize(status_row_idx + 1, u64::MAX);
        renderer
            .row_geometry_cache
            .resize_with(status_row_idx + 1, || None);
    }
    renderer.terminal_rows[status_row_idx].screen_row = status_row_idx as u32;
    if renderer
        .terminal_row_generations
        .get(status_row_idx)
        .is_some_and(|generation| *generation == row.generation)
    {
        return;
    }
    renderer.terminal_rows[status_row_idx] = row.clone();
    renderer.terminal_row_generations[status_row_idx] = row.generation;
    invalidate_row_cache_with_neighbors(&mut renderer.row_geometry_cache, status_row_idx);
}

pub(super) fn frame_layout(
    renderer: &Renderer,
    font_system: &FontSystem,
    tabs: &[TabInfo],
) -> FrameLayout {
    let cell_w = font_system.cell_width as f32;
    let cell_h = font_system.cell_height as f32;
    FrameLayout {
        cell_w,
        cell_h,
        baseline: font_system.baseline_offset(),
        gutter_px: renderer.gutter_width_px(font_system.cell_width) as f32,
        tab_bar_h: if tabs.is_empty() { 0.0 } else { cell_h },
        terminal_y_offset: 0.0,
        block_y_offset: 0.0,
    }
}

pub(super) fn build_image_geometry(
    renderer: &mut Renderer,
    visible_images: &[VisibleImage],
    layout: &FrameLayout,
    under_text: bool,
) -> ImageGeometry {
    let mut geometry = ImageGeometry::default();
    let content_clip = ClipRect {
        left: layout.gutter_px,
        top: layout.tab_bar_h,
        right: renderer.surface_config.width as f32,
        bottom: renderer.surface_config.height as f32,
    };
    let mut ordered_images: Vec<&VisibleImage> = visible_images
        .iter()
        .filter(|vis| (vis.z_index < 0) == under_text)
        .collect();
    ordered_images.sort_by(|left, right| image_render_order(left, right, layout));
    let draw_count = ordered_images.len();

    for (draw_index, vis) in ordered_images.into_iter().enumerate() {
        let z = image_vertex_z(draw_index, draw_count);
        let entry = match renderer.image_atlas.ensure_cached(
            &renderer.device,
            &renderer.queue,
            vis.id,
            vis.frame_index,
            &vis.image,
        ) {
            Some(e) => e,
            None => continue,
        };

        let base_x =
            vis.screen_col as f32 * layout.cell_w + layout.gutter_px + vis.cell_x_offset as f32;
        let base_y = vis.screen_row as f32 * layout.cell_h
            + layout.tab_bar_h
            + layout.terminal_y_offset
            + layout.block_y_offset
            + vis.cell_y_offset as f32;
        let scale_x = if vis.image.width > 0 {
            vis.display_width as f32 / vis.image.width as f32
        } else {
            1.0
        };
        let scale_y = if vis.image.height > 0 {
            vis.display_height as f32 / vis.image.height as f32
        } else {
            1.0
        };

        for tile in &entry.tiles {
            let a = &tile.alloc;
            let x = base_x + tile.src_x as f32 * scale_x;
            let y = base_y + tile.src_y as f32 * scale_y;
            let w = a.width as f32 * scale_x;
            let h = a.height as f32 * scale_y;
            let u0 = a.x as f32 / IMAGE_ATLAS_SIZE as f32;
            let v0 = a.y as f32 / IMAGE_ATLAS_SIZE as f32;
            let u1 = (a.x + a.width) as f32 / IMAGE_ATLAS_SIZE as f32;
            let v1 = (a.y + a.height) as f32 / IMAGE_ATLAS_SIZE as f32;
            let Some(vertices) = clip_image_quad(
                ImageQuad {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                    u0,
                    v0,
                    u1,
                    v1,
                    z,
                },
                content_clip,
            ) else {
                continue;
            };
            let batch = image_batch_for_page(&mut geometry, tile.page_index);
            let ii = batch.vertices.len() as u32;
            batch.vertices.extend_from_slice(&vertices);
            batch
                .indices
                .extend_from_slice(&[ii, ii + 1, ii + 2, ii + 2, ii + 1, ii + 3]);
        }
    }
    geometry
}

pub(super) fn build_render_geometry(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    rows: &[RowSnapshot],
    tabs: &[TabInfo],
    new_tab_text: SmolStr,
    controls: &WindowControls,
    gutter_popup: Option<&GutterPopup>,
    recording_popup: Option<&crate::renderer::RecordingPopup>,
    permission_modal: Option<&crate::renderer::PermissionModal>,
    command_palette: Option<&crate::window_host::CommandPaletteView>,
    history_confirmation: Option<&crate::renderer::HistoryConfirmationModal>,
    history_deletion: Option<&crate::window_host::HistoryDeletionView>,
    toast: Option<&crate::renderer::Toast>,
    preedit: Option<&crate::renderer::PreeditState>,
    command_editor: Option<&commands41::CommandLineView>,
    layout: &FrameLayout,
    suspend_terminal_area: bool,
) -> RenderGeometry {
    let glyph_generation = renderer.glyph_atlas.generation();
    let font_generation = font_system.font_generation();
    let geometry = build_render_geometry_once(
        renderer,
        font_system,
        snap,
        rows,
        tabs,
        new_tab_text,
        controls,
        gutter_popup,
        recording_popup,
        permission_modal,
        command_palette,
        history_confirmation,
        history_deletion,
        toast,
        preedit,
        command_editor,
        layout,
        suspend_terminal_area,
    );
    if renderer.glyph_atlas.generation() != glyph_generation
        || font_system.font_generation() != font_generation
    {
        error!("font/glyph generation changed while building frame geometry");
    }
    geometry
}

pub(super) fn build_render_geometry_once(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    rows: &[RowSnapshot],
    tabs: &[TabInfo],
    new_tab_text: SmolStr,
    controls: &WindowControls,
    gutter_popup: Option<&GutterPopup>,
    recording_popup: Option<&crate::renderer::RecordingPopup>,
    permission_modal: Option<&crate::renderer::PermissionModal>,
    command_palette: Option<&crate::window_host::CommandPaletteView>,
    history_confirmation: Option<&crate::renderer::HistoryConfirmationModal>,
    history_deletion: Option<&crate::window_host::HistoryDeletionView>,
    toast: Option<&crate::renderer::Toast>,
    preedit: Option<&crate::renderer::PreeditState>,
    command_editor: Option<&commands41::CommandLineView>,
    layout: &FrameLayout,
    suspend_terminal_area: bool,
) -> RenderGeometry {
    let mut geometry = RenderGeometry::default();
    let cursor_state = if command_editor.is_some() {
        CursorRenderState::Hidden
    } else {
        cursor_state_from_snapshot(snap)
    };
    let popup_clip = popup_clip(renderer, gutter_popup, layout);
    let blink_off = (APP_START_TIME.get().unwrap().elapsed().as_millis() / 500) & 1 == 1;
    let rapid_blink_off = (APP_START_TIME.get().unwrap().elapsed().as_millis() / 250) & 1 == 1;
    let font_generation = font_system.font_generation();
    let force_terminal_layer_repaint = layout.terminal_y_offset != 0.0;
    let force_row_repaint_by_area = force_terminal_layer_repaint && !suspend_terminal_area;

    if force_row_repaint_by_area {
        push_terminal_area_dirty_rect(
            &mut geometry,
            layout,
            renderer.surface_config.width,
            renderer.surface_config.height,
        );
    }
    for snap_row in rows {
        if row_suspended_by_terminal_area(snap_row, snap, suspend_terminal_area) {
            continue;
        }
        let row = snap_row.screen_row;
        if row_hidden_by_sticky_prompt(snap_row, snap, layout) {
            if !force_row_repaint_by_area {
                push_terminal_dirty_rect(
                    &mut geometry,
                    snap,
                    row,
                    layout,
                    renderer.surface_config.width,
                    renderer.surface_config.height,
                );
            }
            if let Some(cache) = renderer.row_geometry_cache.get_mut(row as usize) {
                *cache = None;
            }
            continue;
        }
        if snap.search_active && row == snap.viewport_rows - 1 {
            push_terminal_dirty_rect(
                &mut geometry,
                snap,
                row,
                layout,
                renderer.surface_config.width,
                renderer.surface_config.height,
            );
            if let Some(cache) = renderer.row_geometry_cache.get_mut(row as usize) {
                *cache = None;
            }
            continue;
        }
        let cache_key = row_render_key(
            renderer,
            snap,
            snap_row,
            row,
            cursor_state,
            popup_clip.as_ref(),
            blink_off,
            rapid_blink_off,
            font_generation,
            layout,
        );
        if let Some(cached) = renderer
            .row_geometry_cache
            .get(row as usize)
            .and_then(Option::as_ref)
            && cached.key == cache_key
            && !force_row_repaint_by_area
        {
            continue;
        }

        if !force_row_repaint_by_area {
            push_terminal_dirty_rect(
                &mut geometry,
                snap,
                row,
                layout,
                renderer.surface_config.width,
                renderer.surface_config.height,
            );
        }
        let mut row_geometry = RowGeometry::default();
        append_row_geometry(
            renderer,
            font_system,
            snap,
            snap_row,
            row,
            cursor_state,
            popup_clip.as_ref(),
            blink_off,
            rapid_blink_off,
            layout,
            &mut row_geometry,
        );
        let cache_key = row_render_key(
            renderer,
            snap,
            snap_row,
            row,
            cursor_state,
            popup_clip.as_ref(),
            blink_off,
            rapid_blink_off,
            font_generation,
            layout,
        );
        append_cached_row_geometry(&mut geometry, &row_geometry);
        if row as usize >= renderer.row_geometry_cache.len() {
            renderer
                .row_geometry_cache
                .resize_with(row as usize + 1, || None);
        }
        renderer.row_geometry_cache[row as usize] = Some(CachedRowKey { key: cache_key });
    }

    append_visual_bell_overlay(renderer, &mut geometry, snap, layout);

    render_status_line_chrome(
        renderer,
        font_system,
        snap,
        layout,
        &mut geometry.bg_vertices,
        &mut geometry.bg_indices,
        &mut geometry.fg,
    );

    render_tab_bar(
        renderer,
        font_system,
        tabs,
        &snap.palette,
        new_tab_text,
        controls,
        &mut geometry.bg_vertices,
        &mut geometry.bg_indices,
        &mut geometry.fg,
        &mut geometry.overlay_bg_vertices,
        &mut geometry.overlay_bg_indices,
        &mut geometry.overlay_fg,
    );
    render_search_bar(
        renderer,
        font_system,
        snap,
        layout.tab_bar_h,
        &mut geometry.bg_vertices,
        &mut geometry.bg_indices,
        &mut geometry.fg,
    );

    if let Some(popup) = recording_popup {
        render_recording_popup(
            renderer,
            font_system,
            popup,
            layout,
            &mut geometry.overlay_bg_vertices,
            &mut geometry.overlay_bg_indices,
            &mut geometry.overlay_fg,
        );
    }

    if let Some(toast) = toast {
        render_toast(
            renderer,
            font_system,
            toast,
            layout,
            &mut geometry.top_overlay_bg_vertices,
            &mut geometry.top_overlay_bg_indices,
            &mut geometry.top_overlay_fg,
        );
    }

    if let Some(command_palette) = command_palette {
        render_command_palette(
            renderer,
            font_system,
            command_palette,
            layout,
            &mut geometry.top_overlay_bg_vertices,
            &mut geometry.top_overlay_bg_indices,
            &mut geometry.top_overlay_fg,
        );
    }

    if let Some(history_deletion) = history_deletion {
        render_history_deletion(
            renderer,
            font_system,
            history_deletion,
            layout,
            &mut geometry.top_overlay_bg_vertices,
            &mut geometry.top_overlay_bg_indices,
            &mut geometry.top_overlay_fg,
        );
    }

    if let Some(modal) = history_confirmation {
        render_history_confirmation_modal(
            renderer,
            font_system,
            modal,
            layout,
            &mut geometry.overlay_bg_vertices,
            &mut geometry.overlay_bg_indices,
            &mut geometry.overlay_fg,
        );
    }

    if let Some(modal) = permission_modal {
        render_permission_modal(
            renderer,
            font_system,
            modal,
            layout,
            &mut geometry.overlay_bg_vertices,
            &mut geometry.overlay_bg_indices,
            &mut geometry.overlay_fg,
        );
    }

    if let Some(preedit) = preedit
        && !snap.search_active
        && command_editor.is_none()
    {
        render_preedit(
            renderer,
            font_system,
            snap,
            preedit,
            layout.gutter_px,
            layout.cell_w,
            layout.cell_h,
            layout.baseline,
            layout.tab_bar_h,
            &mut geometry.bg_vertices,
            &mut geometry.bg_indices,
            &mut geometry.fg,
        );
    }
    if let Some(command_editor) = command_editor
        && !snap.search_active
    {
        render_command_editor(
            renderer,
            font_system,
            snap,
            command_editor,
            layout,
            &mut geometry.overlay_bg_vertices,
            &mut geometry.overlay_bg_indices,
            &mut geometry.overlay_fg,
        );
    }

    if let Some(popup) = gutter_popup {
        render_gutter_popup(
            renderer,
            font_system,
            popup,
            layout.gutter_px,
            layout.cell_w,
            layout.cell_h,
            layout.tab_bar_h,
            &mut geometry.top_overlay_bg_vertices,
            &mut geometry.top_overlay_bg_indices,
            &mut geometry.top_overlay_fg,
        );
    }

    geometry
}

#[allow(clippy::too_many_arguments)]
pub(super) fn row_render_key(
    renderer: &Renderer,
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    row: u32,
    cursor_state: CursorRenderState,
    popup_clip: Option<&ClipRect>,
    blink_off: bool,
    rapid_blink_off: bool,
    font_generation: u64,
    layout: &FrameLayout,
) -> RowRenderKey {
    RowRenderKey {
        layout: RowLayoutKey {
            cell_w: layout.cell_w.to_bits(),
            cell_h: layout.cell_h.to_bits(),
            baseline: layout.baseline.to_bits(),
            gutter_px: layout.gutter_px.to_bits(),
            tab_bar_h: layout.tab_bar_h.to_bits(),
            terminal_y_offset: layout.terminal_y_offset.to_bits(),
            block_y_offset: layout.block_y_offset.to_bits(),
        },
        cursor: row_cursor_key(cursor_state, row),
        blink: row_blink_key(snap, snap_row, blink_off, rapid_blink_off),
        gutter_marker: RowGutterMarkerKey {
            prompt_start: snap_row.prompt_start,
            exit_status: snap_row.exit_status,
            block_separator: snap_row.block_separator,
        },
        popup_clip: row_popup_clip_key(row, layout, popup_clip),
        background_present: renderer.background.is_some(),
        screen_reverse: snap.screen_reverse,
        bg_alpha: renderer.bg_alpha,
        viewport_cols: snap.viewport_cols,
        total_rows: snap.total_rows,
        drcs_generation: Arc::as_ptr(&snap.drcs_glyphs) as usize,
        font_generation,
        glyph_atlas_generation: renderer.glyph_atlas.generation(),
    }
}

pub(super) fn popup_clip(
    renderer: &Renderer,
    gutter_popup: Option<&GutterPopup>,
    layout: &FrameLayout,
) -> Option<ClipRect> {
    gutter_popup.map(|popup| {
        let header = if popup.duration_text.is_some() { 1 } else { 0 };
        let total = (header + GUTTER_MENU_ITEMS.len()) as f32;
        let width = layout.cell_w * POPUP_WIDTH_CELLS;
        let height = total * layout.cell_h;
        let surface_h = renderer.surface_config.height as f32;
        let (left, top) = gutter_popup_origin(
            popup,
            width,
            height,
            layout.cell_w,
            layout.cell_h,
            layout.gutter_px,
            renderer.surface_config.width as f32,
            surface_h,
        );
        let top = top.max(layout.tab_bar_h);
        ClipRect {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    })
}

pub(super) fn append_row_geometry(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    row: u32,
    cursor_state: CursorRenderState,
    popup_clip: Option<&ClipRect>,
    blink_off: bool,
    rapid_blink_off: bool,
    layout: &FrameLayout,
    geometry: &mut RowGeometry,
) {
    let y = snapshot_row_y(row, snap, layout);
    if snap_row.block_separator {
        append_block_separator_row(snap, y, layout, geometry);
        return;
    }
    let line_attr = snap_row.line_attr;
    let is_double_wide = !matches!(line_attr, LineAttr::Normal);
    let effective_cell_w = if is_double_wide {
        layout.cell_w * 2.0
    } else {
        layout.cell_w
    };
    let visible_cols = visible_row_cols(snap, snap_row);

    for col in 0..visible_cols {
        let x = col as f32 * effective_cell_w + layout.gutter_px;
        let block_cursor = cursor_state.block_cursor();
        let painted = resolve_painted_cell(
            snap,
            snap_row,
            row,
            col,
            block_cursor,
            renderer.background.is_some(),
        );
        let cell_attrs = snap_row.attrs[col as usize];
        if let Some(fill_bg) = painted.fill_bg {
            let bg_color = pack_color(&fill_bg, renderer.bg_alpha);
            if col == 0
                && layout.gutter_px > 0.0
                && let Some(gutter_bg) = gutter_fill_bg_for_col0(
                    snap,
                    snap_row,
                    row,
                    block_cursor,
                    renderer.background.is_some(),
                )
            {
                push_rect(
                    0.0,
                    y,
                    layout.gutter_px,
                    layout.cell_h,
                    pack_color(&gutter_bg, renderer.bg_alpha),
                    &mut geometry.bg.vertices,
                    &mut geometry.bg.indices,
                );
            }
            let bi = geometry.bg.vertices.len() as u32;
            geometry.bg.vertices.extend_from_slice(&[
                BgVertex {
                    pos: [x, y],
                    color: bg_color,
                },
                BgVertex {
                    pos: [x + effective_cell_w, y],
                    color: bg_color,
                },
                BgVertex {
                    pos: [x, y + layout.cell_h],
                    color: bg_color,
                },
                BgVertex {
                    pos: [x + effective_cell_w, y + layout.cell_h],
                    color: bg_color,
                },
            ]);
            geometry
                .bg
                .indices
                .extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let ul_style = underline_style_for_render(snap, snap_row.attrs[col as usize]);
        let has_link = snap_row.has_link[col as usize];
        let effective_ul = if has_link && ul_style & CellAttrs::UNDERLINE_MASK == CellAttrs::empty()
        {
            CellAttrs::SINGLE_UNDERLINE
        } else {
            ul_style
        };
        if effective_ul & CellAttrs::UNDERLINE_MASK != CellAttrs::empty() {
            let ul_rgb = snap_row.underline_color[col as usize].unwrap_or(painted.base_fg);
            let ul_packed = pack_color(&ul_rgb, 255);
            let thickness = (layout.cell_h * 0.06).max(1.0);
            let uy = y + layout.cell_h - thickness;
            push_underline_quads(
                effective_ul,
                x,
                uy,
                effective_cell_w,
                thickness,
                layout.cell_h,
                ul_packed,
                &mut geometry.bg.vertices,
                &mut geometry.bg.indices,
            );
        }

        if cell_attrs.contains(CellAttrs::OVERLINE) {
            let ol_color = pack_color(&painted.base_fg, 255);
            let thickness = (layout.cell_h * 0.06).max(1.0);
            push_rect(
                x,
                y,
                effective_cell_w,
                thickness,
                ol_color,
                &mut geometry.bg.vertices,
                &mut geometry.bg.indices,
            );
        }

        if cell_attrs.contains(CellAttrs::STRIKETHROUGH) {
            let st_color = pack_color(&painted.base_fg, 255);
            let thickness = (layout.cell_h * 0.06).max(1.0);
            let sy = y + (layout.cell_h - thickness) * 0.5;
            let bi = geometry.bg.vertices.len() as u32;
            geometry.bg.vertices.extend_from_slice(&[
                BgVertex {
                    pos: [x, sy],
                    color: st_color,
                },
                BgVertex {
                    pos: [x + effective_cell_w, sy],
                    color: st_color,
                },
                BgVertex {
                    pos: [x, sy + thickness],
                    color: st_color,
                },
                BgVertex {
                    pos: [x + effective_cell_w, sy + thickness],
                    color: st_color,
                },
            ]);
            geometry
                .bg
                .indices
                .extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }
    }

    append_gutter_marker(snap_row, layout.gutter_px, layout.cell_h, y, geometry);

    if let Some(overlay) =
        cursor_state.bar_overlay_at(row, &snap_row.fg, layout.cell_w, layout.cell_h)
    {
        let ox = overlay.x + layout.gutter_px;
        let oy = overlay.y + layout.tab_bar_h + layout.terminal_y_offset + layout.block_y_offset;
        let bi = geometry.bg.vertices.len() as u32;
        geometry.bg.vertices.extend_from_slice(&[
            BgVertex {
                pos: [ox, oy],
                color: overlay.color,
            },
            BgVertex {
                pos: [ox + overlay.w, oy],
                color: overlay.color,
            },
            BgVertex {
                pos: [ox, oy + overlay.h],
                color: overlay.color,
            },
            BgVertex {
                pos: [ox + overlay.w, oy + overlay.h],
                color: overlay.color,
            },
        ]);
        geometry
            .bg
            .indices
            .extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
    }

    append_row_glyphs(
        renderer,
        font_system,
        snap,
        snap_row,
        row,
        y,
        line_attr,
        effective_cell_w,
        visible_cols,
        cursor_state,
        popup_clip,
        blink_off,
        rapid_blink_off,
        layout,
        geometry,
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_row_glyphs(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    snap_row: &RowSnapshot,
    row: u32,
    y: f32,
    line_attr: LineAttr,
    effective_cell_w: f32,
    visible_cols: u32,
    cursor_state: CursorRenderState,
    popup_clip: Option<&ClipRect>,
    blink_off: bool,
    rapid_blink_off: bool,
    layout: &FrameLayout,
    geometry: &mut RowGeometry,
) {
    let is_double_wide = !matches!(line_attr, LineAttr::Normal);
    let block_cursor = match cursor_state {
        CursorRenderState::Visible {
            row,
            col,
            shape: CursorShape::Block,
        } => Some((row, col)),
        _ => None,
    };
    let glyphs = collect_row_glyphs(
        font_system,
        snap,
        snap_row,
        row,
        visible_cols,
        block_cursor,
        blink_off,
        rapid_blink_off,
    );

    for glyph in glyphs {
        if let Some(clip) = popup_clip {
            let cx = glyph.col as f32 * effective_cell_w + layout.gutter_px;
            if cx < clip.right
                && cx + effective_cell_w > clip.left
                && y < clip.bottom
                && y + layout.cell_h > clip.top
            {
                continue;
            }
        }

        let slot = match renderer.glyph_atlas.ensure_cached(
            &renderer.device,
            &renderer.queue,
            font_system,
            glyph.font_index,
            glyph.glyph_id,
            glyph.cells_wide,
            glyph.synth_bold,
            drcs_geometry_class(snap).map(|geometry| (geometry, snap.drcs_glyphs.clone())),
        ) {
            Some(e) => e,
            None => continue,
        };
        if slot.is_empty() {
            continue;
        }

        let sx = slot.x();
        let sy = slot.y();
        let sw = slot.width();
        let sh = slot.height();
        let scale_x = if is_double_wide { 2.0_f32 } else { 1.0 };
        let gx = glyph.col as f32 * effective_cell_w
            + slot.bearing_x as f32 * scale_x
            + glyph.x_offset * scale_x
            + layout.gutter_px;
        let gx = gx.floor();
        let gw = (sw as f32 * scale_x).ceil();

        let is_double_height = matches!(
            line_attr,
            LineAttr::DoubleHeightTop | LineAttr::DoubleHeightBottom
        );
        let (gy, gh, uv_y_top, uv_y_bot) = if is_double_height {
            let y_origin = if matches!(line_attr, LineAttr::DoubleHeightTop) {
                y
            } else {
                y - layout.cell_h
            };
            let gy_v = y_origin + (layout.baseline - slot.bearing_y as f32 - glyph.y_offset) * 2.0;
            let gh_v = 2.0 * sh as f32;
            let vis_top = gy_v.max(y);
            let vis_bot = (gy_v + gh_v).min(y + layout.cell_h);
            if vis_bot <= vis_top {
                continue;
            }
            let uv_top = sy as f32 + sh as f32 * (vis_top - gy_v) / gh_v;
            let uv_bot = sy as f32 + sh as f32 * (vis_bot - gy_v) / gh_v;
            (vis_top, vis_bot - vis_top, uv_top, uv_bot)
        } else {
            let gy = y + layout.baseline - slot.bearing_y as f32 - glyph.y_offset;
            (gy, sh as f32, sy as f32, (sy + sh) as f32)
        };
        let gy = gy.floor();
        let gh = gh.ceil();

        let baseline_y = y + layout.baseline;
        let shear = if glyph.synth_italic { 0.2126_f32 } else { 0.0 };
        let shear_at = |vy: f32| -> f32 { shear * (baseline_y - vy) };
        let fg_color = pack_color(&glyph.fg, 255);
        let flags: u32 = if slot.is_color { 1 } else { 0 };
        push_fg_quad(
            &mut geometry.fg,
            slot.page_index,
            [
                FgVertex {
                    pos: [gx + shear_at(gy), gy],
                    uv: [sx as f32, uv_y_top],
                    color: fg_color,
                    flags,
                },
                FgVertex {
                    pos: [gx + gw + shear_at(gy), gy],
                    uv: [(sx + sw) as f32, uv_y_top],
                    color: fg_color,
                    flags,
                },
                FgVertex {
                    pos: [gx + shear_at(gy + gh), gy + gh],
                    uv: [sx as f32, uv_y_bot],
                    color: fg_color,
                    flags,
                },
                FgVertex {
                    pos: [gx + gw + shear_at(gy + gh), gy + gh],
                    uv: [(sx + sw) as f32, uv_y_bot],
                    color: fg_color,
                    flags,
                },
            ],
        );
    }
}

pub(super) fn append_visual_bell_overlay(
    renderer: &mut Renderer,
    geometry: &mut RenderGeometry,
    _snap: &TermSnapshot,
    _layout: &FrameLayout,
) {
    if let Some(start) = renderer.bell_started {
        let elapsed = start.elapsed();
        if elapsed >= BELL_FLASH_DURATION {
            renderer.bell_started = None;
        } else {
            let progress = elapsed.as_secs_f32() / BELL_FLASH_DURATION.as_secs_f32();
            let alpha = (BELL_FLASH_PEAK_ALPHA * (1.0 - progress)) as u8;
            let surface_w = renderer.surface_config.width as f32;
            let surface_h = renderer.surface_config.height as f32;
            let color = u32::from_be_bytes([255, 255, 255, alpha]);
            let bi = geometry.bg_vertices.len() as u32;
            geometry.bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [0.0, 0.0],
                    color,
                },
                BgVertex {
                    pos: [surface_w, 0.0],
                    color,
                },
                BgVertex {
                    pos: [0.0, surface_h],
                    color,
                },
                BgVertex {
                    pos: [surface_w, surface_h],
                    color,
                },
            ]);
            geometry
                .bg_indices
                .extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }
    }
}

pub(super) fn upload_render_geometry(
    renderer: &mut Renderer,
    geometry: &RenderGeometry,
    under_text_image_geometry: &ImageGeometry,
    over_text_image_geometry: &ImageGeometry,
) {
    let device = &renderer.device;
    let queue = &renderer.queue;

    let (terminal_clear_vertices, terminal_clear_indices) =
        dirty_rect_clear_geometry(&geometry.terminal_dirty_rects);
    renderer.uploads.terminal_clear.upload(
        device,
        queue,
        "terminal_clear_verts",
        "terminal_clear_idx",
        &terminal_clear_vertices,
        &terminal_clear_indices,
    );
    renderer.uploads.terminal_bg.upload(
        device,
        queue,
        "terminal_bg_verts",
        "terminal_bg_idx",
        &geometry.terminal_bg_vertices,
        &geometry.terminal_bg_indices,
    );
    renderer.uploads.bg.upload(
        device,
        queue,
        "bg_verts",
        "bg_idx",
        &geometry.bg_vertices,
        &geometry.bg_indices,
    );
    renderer.uploads.overlay_bg.upload(
        device,
        queue,
        "overlay_bg_verts",
        "overlay_bg_idx",
        &geometry.overlay_bg_vertices,
        &geometry.overlay_bg_indices,
    );
    renderer.uploads.top_overlay_bg.upload(
        device,
        queue,
        "top_overlay_bg_verts",
        "top_overlay_bg_idx",
        &geometry.top_overlay_bg_vertices,
        &geometry.top_overlay_bg_indices,
    );
    upload_fg_geometry(
        device,
        queue,
        &mut renderer.uploads.terminal_fg,
        &geometry.terminal_fg,
    );
    upload_fg_geometry(device, queue, &mut renderer.uploads.fg, &geometry.fg);
    upload_fg_geometry(
        device,
        queue,
        &mut renderer.uploads.overlay_fg,
        &geometry.overlay_fg,
    );
    upload_fg_geometry(
        device,
        queue,
        &mut renderer.uploads.top_overlay_fg,
        &geometry.top_overlay_fg,
    );
    upload_image_geometry(
        device,
        queue,
        &mut renderer.uploads.under_image,
        under_text_image_geometry,
    );
    upload_image_geometry(
        device,
        queue,
        &mut renderer.uploads.over_image,
        over_text_image_geometry,
    );
}

pub(super) fn submit_render_passes(
    renderer: &mut Renderer,
    acquired: (wgpu::SurfaceTexture, wgpu::TextureView),
    geometry: RenderGeometry,
    under_text_image_geometry: ImageGeometry,
    over_text_image_geometry: ImageGeometry,
) {
    upload_render_geometry(
        renderer,
        &geometry,
        &under_text_image_geometry,
        &over_text_image_geometry,
    );
    let (frame, view) = acquired;
    let mut encoder = renderer
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

    update_terminal_layer(renderer, &mut encoder);

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bg_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });

        if let Some(background) = &renderer.background {
            pass.set_pipeline(&renderer.bg_image_pipeline);
            pass.set_bind_group(0, &renderer.screen_size_bind_group, &[]);
            pass.set_bind_group(1, background.bind_group(), &[]);
            pass.set_vertex_buffer(0, background.vbuf().slice(..));
            pass.set_index_buffer(background.ibuf().slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..6, 0, 0..1);
        }
    }

    submit_image_pass(renderer, &mut encoder, &view, &renderer.uploads.under_image);

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main_content_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });

        draw_terminal_layer_quad(
            &mut pass,
            &renderer.layer_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.terminal_layer,
        );
        draw_bg_upload(
            &mut pass,
            &renderer.bg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.uploads.bg,
        );
        draw_fg_upload(
            &mut pass,
            &renderer.fg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.glyph_atlas,
            &renderer.uploads.fg,
        );
    }

    submit_image_pass(renderer, &mut encoder, &view, &renderer.uploads.over_image);

    if renderer.uploads.overlay_bg.has_indices
        || renderer.uploads.overlay_fg.is_drawable()
        || renderer.uploads.top_overlay_bg.has_indices
        || renderer.uploads.top_overlay_fg.is_drawable()
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });

        draw_bg_upload(
            &mut pass,
            &renderer.bg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.uploads.overlay_bg,
        );
        draw_fg_upload(
            &mut pass,
            &renderer.fg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.glyph_atlas,
            &renderer.uploads.overlay_fg,
        );
        draw_bg_upload(
            &mut pass,
            &renderer.bg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.uploads.top_overlay_bg,
        );
        draw_fg_upload(
            &mut pass,
            &renderer.fg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.glyph_atlas,
            &renderer.uploads.top_overlay_fg,
        );
    }

    renderer.queue.submit(Some(encoder.finish()));
    frame.present();
}

pub(super) fn update_terminal_layer(
    renderer: &mut Renderer,
    encoder: &mut wgpu::CommandEncoder,
) {
    if !renderer.terminal_layer.needs_full_repaint
        && !renderer.uploads.terminal_clear.has_indices
        && !renderer.uploads.terminal_bg.has_indices
        && !renderer.uploads.terminal_fg.is_drawable()
    {
        return;
    }

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("terminal_layer_update"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &renderer.terminal_layer.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if renderer.terminal_layer.needs_full_repaint {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });

        draw_bg_upload(
            &mut pass,
            &renderer.bg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.uploads.terminal_clear,
        );
        draw_bg_upload(
            &mut pass,
            &renderer.bg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.uploads.terminal_bg,
        );
        draw_fg_upload(
            &mut pass,
            &renderer.fg_pipeline,
            &renderer.screen_size_bind_group,
            &renderer.glyph_atlas,
            &renderer.uploads.terminal_fg,
        );
    }

    renderer.terminal_layer.needs_full_repaint = false;
}

pub(super) fn submit_image_pass(
    renderer: &Renderer,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    image_upload: &PageGeometryUpload<ImageVertex>,
) {
    if !image_upload.is_drawable() {
        return;
    }
    let Some(vertex_buffer) = image_upload.vertex_buffer.buffer() else {
        return;
    };
    let Some(index_buffer) = image_upload.index_buffer.buffer() else {
        return;
    };

    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("image_pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &renderer.image_depth.view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.0),
                store: wgpu::StoreOp::Discard,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    pass.set_pipeline(&renderer.image_pipeline);
    pass.set_bind_group(0, &renderer.screen_size_bind_group, &[]);
    pass.set_vertex_buffer(0, vertex_buffer.slice(..));
    pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    for range in &image_upload.ranges {
        let Some(bind_group) = renderer.image_atlas.bind_group(range.page_index) else {
            continue;
        };
        pass.set_bind_group(1, bind_group, &[]);
        pass.draw_indexed(
            range.index_start..range.index_start + range.index_count,
            range.vertex_base,
            0..1,
        );
    }
}

fn append_block_separator_row(
    snap: &TermSnapshot,
    y: f32,
    layout: &FrameLayout,
    geometry: &mut RowGeometry,
) {
    let thickness = (layout.cell_h * 0.08).max(1.0);
    let inset = layout.cell_w * 0.5;
    let x = layout.gutter_px + inset;
    let width = (snap.viewport_cols as f32 * layout.cell_w - inset * 2.0).max(0.0);
    let y = y + (layout.cell_h - thickness) * 0.5;
    push_rect(
        x,
        y,
        width,
        thickness,
        pack_color(&snap.palette.fg, 72),
        &mut geometry.bg.vertices,
        &mut geometry.bg.indices,
    );
}

fn draw_terminal_layer_quad<'pass>(
    pass: &mut wgpu::RenderPass<'pass>,
    layer_pipeline: &'pass wgpu::RenderPipeline,
    screen_size_bind_group: &'pass wgpu::BindGroup,
    terminal_layer: &'pass TerminalLayer,
) {
    pass.set_pipeline(layer_pipeline);
    pass.set_bind_group(0, screen_size_bind_group, &[]);
    pass.set_bind_group(1, &terminal_layer.bind_group, &[]);
    pass.set_vertex_buffer(0, terminal_layer.vertex_buffer.slice(..));
    pass.set_index_buffer(
        terminal_layer.index_buffer.slice(..),
        wgpu::IndexFormat::Uint32,
    );
    pass.draw_indexed(0..6, 0, 0..1);
}

fn draw_bg_upload<'pass>(
    pass: &mut wgpu::RenderPass<'pass>,
    bg_pipeline: &'pass wgpu::RenderPipeline,
    screen_size_bind_group: &'pass wgpu::BindGroup,
    upload: &'pass GeometryUpload,
) {
    if !upload.has_indices {
        return;
    }
    let Some(vertex_buffer) = upload.vertex_buffer.buffer() else {
        return;
    };
    let Some(index_buffer) = upload.index_buffer.buffer() else {
        return;
    };

    pass.set_pipeline(bg_pipeline);
    pass.set_bind_group(0, screen_size_bind_group, &[]);
    pass.set_vertex_buffer(0, vertex_buffer.slice(..));
    pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..upload.index_count, 0, 0..1);
}

fn draw_fg_upload<'pass>(
    pass: &mut wgpu::RenderPass<'pass>,
    fg_pipeline: &'pass wgpu::RenderPipeline,
    screen_size_bind_group: &'pass wgpu::BindGroup,
    glyph_atlas: &'pass GlyphAtlas,
    fg_upload: &'pass PageGeometryUpload<FgVertex>,
) {
    if !fg_upload.is_drawable() {
        return;
    }
    let Some(vertex_buffer) = fg_upload.vertex_buffer.buffer() else {
        return;
    };
    let Some(index_buffer) = fg_upload.index_buffer.buffer() else {
        return;
    };

    pass.set_pipeline(fg_pipeline);
    pass.set_bind_group(0, screen_size_bind_group, &[]);
    pass.set_vertex_buffer(0, vertex_buffer.slice(..));
    pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    for range in &fg_upload.ranges {
        let Some(bind_group) = glyph_atlas.bind_group(range.page_index) else {
            continue;
        };
        pass.set_bind_group(1, bind_group, &[]);
        pass.draw_indexed(
            range.index_start..range.index_start + range.index_count,
            range.vertex_base,
            0..1,
        );
    }
}

use super::super::*;
use super::popup::push_bordered_panel;

impl Renderer {
    pub(in crate::renderer::r#impl) fn render_history_deletion(
        &mut self,
        font_system: &mut FontSystem,
        history_view: &crate::window_host::HistoryDeletionView,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let surface_cols = (surface_w / layout.cell_w).floor().max(1.0) as usize;
        let width_cells = 96.min(surface_cols).max(1);
        let text_cells = width_cells.saturating_sub(4);
        let panel_w = width_cells as f32 * layout.cell_w;
        let panel_x = ((surface_w - panel_w) * 0.5).floor().max(0.0);
        let panel_y = layout.tab_bar_h + 3.0 * layout.cell_h;
        if panel_y >= surface_h {
            return;
        }

        let available_rows = ((surface_h - panel_y) / layout.cell_h).floor().max(1.0) as usize;
        let visible_rows = history_view
            .displayed
            .len()
            .min(available_rows.saturating_sub(6).max(1));
        let first_row = history_view
            .scroll
            .min(history_view.displayed.len().saturating_sub(visible_rows));
        let panel_h = (visible_rows + 6) as f32 * layout.cell_h;

        let panel_bg = pack_color(&palette::Srgb::new(18, 20, 24), 246);
        let input_bg = pack_color(&palette::Srgb::new(27, 30, 35), 255);
        let border = pack_color(&palette::Srgb::new(112, 122, 130), 255);
        let item_bg = pack_color(&palette::Srgb::new(30, 35, 40), 255);
        let text_fg = pack_color(&palette::Srgb::new(238, 241, 242), 255);
        let hint_fg = pack_color(&palette::Srgb::new(172, 182, 188), 255);
        let dim_fg = pack_color(&palette::Srgb::new(130, 142, 150), 255);

        push_bordered_panel(
            (panel_x, panel_y, panel_w, panel_h),
            panel_bg,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel_x + 1.0,
            panel_y + layout.cell_h,
            (panel_w - 2.0).max(0.0),
            layout.cell_h,
            input_bg,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel_x,
            panel_y + 4.0 * layout.cell_h,
            panel_w,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );

        self.shape_popup_line(
            font_system,
            "Filter history entries",
            panel_x + layout.cell_w,
            panel_y,
            layout.baseline,
            layout.cell_w,
            layout.cell_h,
            hint_fg,
            fg,
        );
        let query: String = history_view
            .query
            .graphemes(true)
            .take(text_cells)
            .collect();
        self.shape_popup_line(
            font_system,
            &query,
            panel_x + layout.cell_w,
            panel_y + layout.cell_h,
            layout.baseline,
            layout.cell_w,
            layout.cell_h,
            text_fg,
            fg,
        );
        let query_cells = history_view.query.graphemes(true).count().min(text_cells);
        push_rect(
            panel_x + layout.cell_w + query_cells as f32 * layout.cell_w,
            panel_y + layout.cell_h + 2.0,
            1.0,
            (layout.cell_h - 4.0).max(1.0),
            text_fg,
            bg_vertices,
            bg_indices,
        );
        let instructions: String = "Type to filter. Enter deletes displayed entries. Empty input \
                                    or Escape cancels."
            .graphemes(true)
            .take(text_cells)
            .collect();
        self.shape_popup_line(
            font_system,
            &instructions,
            panel_x + layout.cell_w,
            panel_y + 2.0 * layout.cell_h,
            layout.baseline,
            layout.cell_w,
            layout.cell_h,
            hint_fg,
            fg,
        );
        self.shape_popup_line(
            font_system,
            &format!("{} displayed", history_view.displayed.len()),
            panel_x + layout.cell_w,
            panel_y + 3.0 * layout.cell_h,
            layout.baseline,
            layout.cell_w,
            layout.cell_h,
            dim_fg,
            fg,
        );

        for (visible_idx, entry_idx) in history_view
            .displayed
            .iter()
            .skip(first_row)
            .take(visible_rows)
            .enumerate()
        {
            let Some(entry) = history_view.entries.get(*entry_idx) else {
                continue;
            };
            let row_y = panel_y + (visible_idx + 5) as f32 * layout.cell_h;
            push_rect(
                panel_x + 1.0,
                row_y,
                (panel_w - 2.0).max(0.0),
                layout.cell_h,
                item_bg,
                bg_vertices,
                bg_indices,
            );
            let command_cells = text_cells.saturating_mul(2) / 3;
            let cwd_cells = text_cells.saturating_sub(command_cells + 1);
            let command: String = entry.command.graphemes(true).take(command_cells).collect();
            let cwd: String = entry.cwd.graphemes(true).take(cwd_cells).collect();
            self.shape_popup_line(
                font_system,
                &command,
                panel_x + layout.cell_w,
                row_y,
                layout.baseline,
                layout.cell_w,
                layout.cell_h,
                text_fg,
                fg,
            );
            self.shape_popup_line(
                font_system,
                &cwd,
                panel_x + (command_cells + 2) as f32 * layout.cell_w,
                row_y,
                layout.baseline,
                layout.cell_w,
                layout.cell_h,
                dim_fg,
                fg,
            );
        }

        render_history_deletion_scrollbar(
            history_view,
            first_row,
            visible_rows,
            panel_x + panel_w - layout.cell_w,
            panel_y + 5.0 * layout.cell_h,
            layout.cell_w,
            layout.cell_h,
            bg_vertices,
            bg_indices,
        );
    }
}

fn render_history_deletion_scrollbar(
    history_view: &crate::window_host::HistoryDeletionView,
    first_row: usize,
    visible_rows: usize,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
) {
    let visible = visible_rows.max(1);
    let total = history_view.displayed.len();
    if total <= visible {
        return;
    }
    let track_h = visible as f32 * cell_h;
    let track_w = (cell_w * 0.18).max(2.0);
    let track_x = x + (cell_w - track_w) * 0.5;
    push_rect(
        track_x,
        y,
        track_w,
        track_h,
        pack_color(&Srgb::new(54, 62, 78), 220),
        bg_vertices,
        bg_indices,
    );

    let thumb_h = (track_h * visible as f32 / total as f32).max(cell_h * 0.45);
    let max_start = total.saturating_sub(visible).max(1);
    let scroll_ratio = first_row as f32 / max_start as f32;
    let thumb_y = y + (track_h - thumb_h).max(0.0) * scroll_ratio;
    push_rect(
        track_x,
        thumb_y,
        track_w,
        thumb_h,
        pack_color(&Srgb::new(145, 160, 190), 255),
        bg_vertices,
        bg_indices,
    );
}

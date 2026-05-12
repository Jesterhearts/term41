use super::super::*;

impl Renderer {
    pub(in crate::renderer::r#impl) fn render_command_palette(
        &mut self,
        font_system: &mut FontSystem,
        palette_view: &crate::CommandPaletteView,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let surface_cols = (surface_w / layout.cell_w).floor().max(1.0) as usize;
        let max_label_cells = palette_view
            .items
            .iter()
            .map(|item| item.label.graphemes(true).count())
            .max()
            .unwrap_or(0);
        let max_text_cells = max_label_cells.max(palette_view.query.graphemes(true).count());
        let width_cells = (max_text_cells + 2).clamp(28, 64).min(surface_cols);
        let text_cells = width_cells.saturating_sub(2);
        let panel_w = width_cells as f32 * layout.cell_w;
        let panel_x = ((surface_w - panel_w) * 0.5).floor().max(0.0);
        let panel_y = layout.tab_bar_h + 3.0 * layout.cell_h;
        if panel_y >= surface_h {
            return;
        }

        let available_rows = ((surface_h - panel_y) / layout.cell_h).floor().max(1.0) as usize;
        let visible_rows = palette_view
            .items
            .len()
            .min(available_rows.saturating_sub(3));
        let selected = palette_view
            .selected
            .min(palette_view.items.len().saturating_sub(1));
        let first_row = selected
            .saturating_add(1)
            .saturating_sub(visible_rows)
            .min(palette_view.items.len().saturating_sub(visible_rows));
        let panel_h = (visible_rows + 3) as f32 * layout.cell_h;

        let panel_bg = pack_color(&palette::Srgb::new(18, 20, 24), 246);
        let input_bg = pack_color(&palette::Srgb::new(27, 30, 35), 255);
        let border = pack_color(&palette::Srgb::new(112, 122, 130), 255);
        let selected_bg = pack_color(&palette::Srgb::new(43, 72, 76), 255);
        let text_fg = pack_color(&palette::Srgb::new(238, 241, 242), 255);
        let selected_fg = pack_color(&palette::Srgb::new(255, 255, 255), 255);

        push_rect(
            panel_x,
            panel_y,
            panel_w,
            panel_h,
            panel_bg,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel_x,
            panel_y,
            panel_w,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel_x,
            panel_y + panel_h - 1.0,
            panel_w,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel_x,
            panel_y,
            1.0,
            panel_h,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel_x + panel_w - 1.0,
            panel_y,
            1.0,
            panel_h,
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
            panel_y + 2.0 * layout.cell_h,
            panel_w,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );

        let query: String = palette_view
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
        let query_cells = palette_view.query.graphemes(true).count().min(text_cells);
        push_rect(
            panel_x + layout.cell_w + query_cells as f32 * layout.cell_w,
            panel_y + layout.cell_h + 2.0,
            1.0,
            (layout.cell_h - 4.0).max(1.0),
            text_fg,
            bg_vertices,
            bg_indices,
        );

        for (visible_idx, (item_idx, item)) in palette_view
            .items
            .iter()
            .enumerate()
            .skip(first_row)
            .take(visible_rows)
            .enumerate()
        {
            let row_y = panel_y + (visible_idx + 2) as f32 * layout.cell_h;
            let is_selected = item_idx == selected;
            if is_selected {
                push_rect(
                    panel_x + 1.0,
                    row_y,
                    (panel_w - 2.0).max(0.0),
                    layout.cell_h,
                    selected_bg,
                    bg_vertices,
                    bg_indices,
                );
            }

            let text: String = item.label.graphemes(true).take(text_cells).collect();
            self.shape_popup_line(
                font_system,
                &text,
                panel_x + layout.cell_w,
                row_y,
                layout.baseline,
                layout.cell_w,
                layout.cell_h,
                if is_selected { selected_fg } else { text_fg },
                fg,
            );
        }
    }
}

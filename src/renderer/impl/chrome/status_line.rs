use super::super::*;

impl Renderer {
    pub(in crate::renderer::r#impl) fn render_status_line_chrome(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some(row) = snap.status_line_row else {
            return;
        };
        let y = snapshot_row_y(row, snap, layout);
        let border = pack_color(&snap.palette.status_line_fg, 255);
        let left = 0.0;
        let width = layout.gutter_px + snap.viewport_cols as f32 * layout.cell_w;
        let thickness = 1.0_f32.max((layout.cell_h * 0.04).round());
        push_rect(left, y, width, thickness, border, bg_vertices, bg_indices);
        push_rect(
            left,
            y + layout.cell_h - thickness,
            width,
            thickness,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            left,
            y,
            thickness,
            layout.cell_h,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            left + width - thickness,
            y,
            thickness,
            layout.cell_h,
            border,
            bg_vertices,
            bg_indices,
        );

        if layout.gutter_px <= 0.0 {
            return;
        }

        let row = status_line_label_row("⟫", &snap.palette);
        let shaped = font_system.shape_row(&row.cells, &row.attrs);
        let baseline = font_system.baseline_offset();
        let cell_w = font_system.cell_width as f32;
        let marker_x = ((layout.gutter_px - cell_w) * 0.5).max(0.0);

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
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
            let gx = marker_x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let flags: u32 = if slot.is_color { 1 } else { 0 };
            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx.floor(), gy.floor()],
                        uv: [sx as f32, sy as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor() + sw as f32, gy.floor()],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor(), gy.floor() + sh as f32],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor() + sw as f32, gy.floor() + sh as f32],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: border,
                        flags,
                    },
                ],
            );
        }
    }
}

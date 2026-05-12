use super::super::*;

impl Renderer {
    pub(in crate::renderer::r#impl) fn render_recording_popup(
        &mut self,
        font_system: &mut FontSystem,
        popup: &crate::renderer::RecordingPopup,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        if popup.lines.is_empty() {
            return;
        }

        let baseline = font_system.baseline_offset();
        let margin_x = layout.cell_w;
        let margin_y = layout.cell_h * 0.5;
        let max_chars = popup
            .lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(1);
        let popup_w = (max_chars as f32 + 2.0) * layout.cell_w;
        let popup_h = popup.lines.len() as f32 * layout.cell_h + margin_y * 2.0;
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let popup_x = ((surface_w - popup_w) * 0.5).max(layout.gutter_px);
        let popup_y = ((surface_h - popup_h + layout.tab_bar_h) * 0.5).max(layout.tab_bar_h);

        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 244);
        let border = pack_color(&palette::Srgb::new(92, 92, 118), 255);
        let text_fg = pack_color(&palette::Srgb::new(232, 232, 236), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let border_w = 1.0_f32;
        for bx in [popup_x, popup_x + popup_w - border_w] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [bx, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx, popup_y + popup_h],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y + popup_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        for (i, line) in popup.lines.iter().enumerate() {
            self.shape_popup_line(
                font_system,
                line,
                popup_x + margin_x,
                popup_y + margin_y + i as f32 * layout.cell_h,
                baseline,
                layout.cell_w,
                layout.cell_h,
                text_fg,
                fg,
            );
        }
    }
}

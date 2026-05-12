use super::super::*;

impl Renderer {
    /// Shape a short text string and emit foreground glyph quads at the
    /// given position. Used by the tab bar for both tab labels and window
    /// control button glyphs.
    pub(in crate::renderer::r#impl) fn shape_and_render_label(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        x: f32,
        y: f32,
        baseline: f32,
        cell_w: f32,
        centered_width: Option<f32>,
        fitted_height: Option<f32>,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let cells: Vec<smol_str::SmolStr> = text
            .graphemes(true)
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);
        let mut glyphs = Vec::with_capacity(shaped.len());

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

            glyphs.push(LabelGlyph {
                slot,
                col: sg.col,
                x_offset: sg.x_offset,
                y_offset: sg.y_offset,
            });
        }

        let x = match (centered_width, label_ink_bounds(&glyphs, cell_w)) {
            (Some(width), Some((left, right))) => centered_ink_origin_x(x, width, left, right),
            _ => x,
        };
        let y = match (fitted_height, label_ink_y_bounds(&glyphs, baseline)) {
            (Some(height), Some((top, bottom))) => fitted_ink_origin_y(y, height, top, bottom),
            _ => y,
        };

        for glyph in glyphs {
            let sx = glyph.slot.x();
            let sy = glyph.slot.y();
            let sw = glyph.slot.width();
            let sh = glyph.slot.height();

            let gx = x + glyph.col as f32 * cell_w + glyph.slot.bearing_x as f32 + glyph.x_offset;
            let gx = gx.floor();

            let gy = y + baseline - glyph.slot.bearing_y as f32 - glyph.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;

            let flags: u32 = if glyph.slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                glyph.slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                ],
            );
        }
    }
}

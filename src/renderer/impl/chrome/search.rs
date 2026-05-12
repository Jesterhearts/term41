use super::super::*;

impl Renderer {
    /// Paint the bottom-of-viewport search bar. The bar is a dark quad
    /// stretching across the viewport's last row, with a prompt + typed
    /// query + match counter shaped through the normal glyph atlas. A
    /// small caret marks the query's end so the user can see where their
    /// next keystroke will land.
    pub(in crate::renderer::r#impl) fn render_search_bar(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        y_offset: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some(search) = &snap.search else {
            return;
        };

        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let cols = snap.viewport_cols;
        let rows = snap.viewport_rows;
        if rows == 0 || cols == 0 {
            return;
        }

        // Build the visible label. The counter only appears once there are
        // matches to count — an empty query draws just the prompt so the
        // user sees something immediately on `Ctrl+Shift+F`.
        let counter = if search.match_count == 0 {
            if search.query.is_empty() {
                String::new()
            } else {
                "  (no match)".to_string()
            }
        } else {
            format!("  ({}/{})", search.active_idx + 1, search.match_count)
        };
        let label = format!("Find: {}{}", search.query, counter);

        // Truncate to fit the viewport width. We measure by char count —
        // one cell per char is the same approximation we use throughout
        // the ASCII-dominant pieces of this code.
        let max_chars = cols as usize;
        let label_graphemes: Vec<&str> = label.graphemes(true).take(max_chars).collect();

        // Caret sits at the end of the typed query, in column terms. The
        // prompt is exactly "Find: " (6 chars); the caret lives right
        // after the query text, clamped to the truncated label width.
        let prompt_len = "Find: ".chars().count() as u32;
        let caret_col = (prompt_len + search.query.chars().count() as u32).min(cols - 1);

        // Bar background: a dark opaque strip across the last row.
        let bar_y = (rows - 1) as f32 * cell_h + y_offset;
        let bar_w = cols as f32 * cell_w;
        let bar_bg = pack_color(&palette::Srgb::new(24, 24, 32), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [0.0, bar_y],
                color: bar_bg,
            },
            BgVertex {
                pos: [bar_w, bar_y],
                color: bar_bg,
            },
            BgVertex {
                pos: [0.0, bar_y + cell_h],
                color: bar_bg,
            },
            BgVertex {
                pos: [bar_w, bar_y + cell_h],
                color: bar_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Caret: a thin bright bar at the query insertion point so the
        // user can see where their next keystroke will go.
        let caret_x = caret_col as f32 * cell_w;
        let caret_w = (cell_w * 0.1).max(1.0);
        let caret_color = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [caret_x, bar_y + cell_h * 0.1],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x + caret_w, bar_y + cell_h * 0.1],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x, bar_y + cell_h * 0.9],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x + caret_w, bar_y + cell_h * 0.9],
                color: caret_color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Label glyphs. Shape through the normal text pipeline so the bar
        // respects whatever font variants are loaded and goes through the
        // atlas LRU like any other glyph.
        let cells: Vec<smol_str::SmolStr> = label_graphemes
            .iter()
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        let label_fg = pack_color(&palette::Srgb::new(220, 220, 220), 255);
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

            let gx = sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = bar_y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;

            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: label_fg,
                        flags,
                    },
                ],
            );
        }
    }
}

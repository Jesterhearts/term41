use super::super::*;

/// Paint the IME preedit: a darkened strip at the cursor cell, an
/// underline that marks the whole composition as uncommitted, a
/// thicker underline over the caret/highlighted segment the IME
/// reported (when it did), and the composed glyphs themselves.
///
/// Composition length is clipped to the remaining columns on the
/// cursor's row; long compositions trail off past the right edge
/// rather than wrapping. The IME's candidate popup sits below this
/// overlay via `set_ime_cursor_area`, so the user can still see their
/// options.
pub(in crate::renderer::r#impl) fn render_preedit(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    preedit: &crate::renderer::PreeditState,
    gutter_px: f32,
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    tab_bar_h: f32,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
    fg: &mut FgGeometry,
) {
    let Some((cursor_row, cursor_col)) = snap.cursor else {
        return;
    };
    if preedit.text.is_empty() {
        return;
    }

    let origin_x = cursor_col as f32 * cell_w + gutter_px;
    let origin_y = cursor_row as f32 * cell_h + tab_bar_h;

    let max_chars = snap.viewport_cols.saturating_sub(cursor_col) as usize;
    if max_chars == 0 {
        return;
    }

    // Per-char iteration keeps the math simple — the overlay treats
    // every codepoint as one cell wide. That's wrong for CJK
    // full-width chars in general, but the preedit is a transient
    // overlay on top of the grid, and the candidate popup does the
    // real work of showing full-width layout options.
    let visible_graphemes: Vec<&str> = preedit.text.graphemes(true).take(max_chars).collect();
    let visible_len = visible_graphemes.len();
    if visible_len == 0 {
        return;
    }

    // Solid dark panel so the glyph being composed doesn't bleed
    // through the cells it's sitting on. Alpha is 255 because we
    // want full occlusion; the whole surface is already composited
    // with the window opacity.
    let panel_bg = pack_color(&palette::Srgb::new(40, 40, 55), 255);
    let panel_w = visible_len as f32 * cell_w;
    let bi = bg_vertices.len() as u32;
    bg_vertices.extend_from_slice(&[
        BgVertex {
            pos: [origin_x, origin_y],
            color: panel_bg,
        },
        BgVertex {
            pos: [origin_x + panel_w, origin_y],
            color: panel_bg,
        },
        BgVertex {
            pos: [origin_x, origin_y + cell_h],
            color: panel_bg,
        },
        BgVertex {
            pos: [origin_x + panel_w, origin_y + cell_h],
            color: panel_bg,
        },
    ]);
    bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

    // Thin underline across the whole composition — the universal
    // "this text isn't committed yet" hint.
    let underline_color = pack_color(&palette::Srgb::new(180, 180, 220), 255);
    let underline_h = (cell_h * 0.08).max(1.5);
    let underline_y = origin_y + cell_h - underline_h;
    let bi = bg_vertices.len() as u32;
    bg_vertices.extend_from_slice(&[
        BgVertex {
            pos: [origin_x, underline_y],
            color: underline_color,
        },
        BgVertex {
            pos: [origin_x + panel_w, underline_y],
            color: underline_color,
        },
        BgVertex {
            pos: [origin_x, underline_y + underline_h],
            color: underline_color,
        },
        BgVertex {
            pos: [origin_x + panel_w, underline_y + underline_h],
            color: underline_color,
        },
    ]);
    bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

    // The IME may mark a selected segment (the part the user is
    // currently editing inside a longer composition) via a byte range.
    // Paint a thicker bar over that segment so the user can see where
    // their next keystroke lands. Empty / full-span ranges just mean
    // "caret at position"; we skip them to avoid double-drawing the
    // whole underline.
    if let Some((start_byte, end_byte)) = preedit.cursor
        && start_byte != end_byte
    {
        let (seg_start_char, seg_end_char) =
            byte_range_to_char_range(&preedit.text, start_byte, end_byte, visible_len);
        if seg_end_char > seg_start_char {
            let seg_x = origin_x + seg_start_char as f32 * cell_w;
            let seg_w = (seg_end_char - seg_start_char) as f32 * cell_w;
            let seg_h = (cell_h * 0.14).max(2.5);
            let seg_y = origin_y + cell_h - seg_h;
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [seg_x, seg_y],
                    color: underline_color,
                },
                BgVertex {
                    pos: [seg_x + seg_w, seg_y],
                    color: underline_color,
                },
                BgVertex {
                    pos: [seg_x, seg_y + seg_h],
                    color: underline_color,
                },
                BgVertex {
                    pos: [seg_x + seg_w, seg_y + seg_h],
                    color: underline_color,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }
    }

    // Shape the composing text through the same pipeline normal cells
    // use so fonts, ligatures, and fallback chains behave identically.
    let cells: Vec<smol_str::SmolStr> = visible_graphemes
        .iter()
        .map(|g| {
            let mut builder = SmolStrBuilder::new();
            builder.push_str(g);
            builder.finish()
        })
        .collect();
    let attrs = vec![CellAttrs::default(); cells.len()];
    let shaped = font_system.shape_row(&cells, &attrs);

    let glyph_fg = pack_color(&palette::Srgb::new(235, 235, 245), 255);
    for sg in &shaped {
        let slot = match renderer.glyph_atlas.ensure_cached(
            &renderer.device,
            &renderer.queue,
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

        let gx = origin_x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
        let gx = gx.floor();

        let gy = origin_y + baseline - slot.bearing_y as f32 - sg.y_offset;
        let gy = gy.ceil();

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
                    color: glyph_fg,
                    flags,
                },
                FgVertex {
                    pos: [gx + gw, gy],
                    uv: [(sx + sw) as f32, sy as f32],
                    color: glyph_fg,
                    flags,
                },
                FgVertex {
                    pos: [gx, gy + gh],
                    uv: [sx as f32, (sy + sh) as f32],
                    color: glyph_fg,
                    flags,
                },
                FgVertex {
                    pos: [gx + gw, gy + gh],
                    uv: [(sx + sw) as f32, (sy + sh) as f32],
                    color: glyph_fg,
                    flags,
                },
            ],
        );
    }
}

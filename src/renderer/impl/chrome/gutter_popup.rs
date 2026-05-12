use super::super::*;

/// Paint the gutter popup: a dark panel with an optional duration header
/// and four action items, one per row. The hovered item gets a brighter
/// background so the user sees where their click will land.
pub(in crate::renderer::r#impl) fn render_gutter_popup(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    popup: &GutterPopup,
    gutter_px: f32,
    cell_w: f32,
    cell_h: f32,
    tab_bar_h: f32,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
    fg: &mut FgGeometry,
) {
    let baseline = font_system.baseline_offset();
    let surface_h = renderer.surface_config.height as f32;

    let header_rows = if popup.duration_text.is_some() { 1 } else { 0 };
    let total_rows = header_rows + GUTTER_MENU_ITEMS.len();
    let popup_w = cell_w * POPUP_WIDTH_CELLS;
    let popup_h = total_rows as f32 * cell_h;
    let (popup_x, popup_y) = gutter_popup_origin(
        popup,
        popup_w,
        popup_h,
        cell_w,
        cell_h,
        gutter_px,
        renderer.surface_config.width as f32,
        surface_h,
    );
    let popup_y = popup_y.max(tab_bar_h);

    // Panel background.
    let panel_bg = pack_color(&palette::Srgb::new(30, 30, 38), 255);
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

    // Thin border at top and bottom.
    let border_color = pack_color(&palette::Srgb::new(80, 80, 100), 255);
    let border_h = 1.0_f32;
    for by in [popup_y, popup_y + popup_h - border_h] {
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, by],
                color: border_color,
            },
            BgVertex {
                pos: [popup_x + popup_w, by],
                color: border_color,
            },
            BgVertex {
                pos: [popup_x, by + border_h],
                color: border_color,
            },
            BgVertex {
                pos: [popup_x + popup_w, by + border_h],
                color: border_color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
    }

    let margin = cell_w * 0.5;
    let max_chars = ((popup_w - margin * 2.0) / cell_w).max(1.0) as usize;

    // Duration header.
    if let Some(ref dur) = popup.duration_text {
        let label: String = dur.chars().take(max_chars).collect();
        let dim_fg = pack_color(&palette::Srgb::new(140, 140, 160), 255);
        super::shape_popup_line(
            renderer,
            font_system,
            &label,
            popup_x + margin,
            popup_y,
            baseline,
            cell_w,
            cell_h,
            dim_fg,
            fg,
        );
    }

    // Menu items.
    let normal_fg = pack_color(&palette::Srgb::new(220, 220, 220), 255);
    let hover_bg = pack_color(&palette::Srgb::new(55, 55, 70), 255);

    for (i, item) in GUTTER_MENU_ITEMS.iter().enumerate() {
        let row_y = popup_y + (header_rows + i) as f32 * cell_h;

        // Hover highlight.
        if popup.hovered_item == Some(i) {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, row_y],
                    color: hover_bg,
                },
                BgVertex {
                    pos: [popup_x + popup_w, row_y],
                    color: hover_bg,
                },
                BgVertex {
                    pos: [popup_x, row_y + cell_h],
                    color: hover_bg,
                },
                BgVertex {
                    pos: [popup_x + popup_w, row_y + cell_h],
                    color: hover_bg,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let label: String = item.label.chars().take(max_chars).collect();
        super::shape_popup_line(
            renderer,
            font_system,
            &label,
            popup_x + margin,
            row_y,
            baseline,
            cell_w,
            cell_h,
            normal_fg,
            fg,
        );
    }
}

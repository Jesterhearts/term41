use super::super::*;

pub(in crate::renderer::r#impl) fn render_toast(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    toast: &crate::renderer::Toast,
    layout: &FrameLayout,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
    fg: &mut FgGeometry,
) {
    let text_chars = toast.text.chars().count();
    if text_chars == 0 {
        return;
    }

    let width_cells = (text_chars + 2).clamp(3, 100);
    let text_capacity = width_cells.saturating_sub(2);
    let text: String = toast.text.chars().take(text_capacity).collect();
    let popup_w = width_cells as f32 * layout.cell_w;
    let popup_h = 3.0 * layout.cell_h;
    let surface_w = renderer.surface_config.width as f32;
    let surface_h = renderer.surface_config.height as f32;
    let popup_x = (surface_w - popup_w).max(layout.gutter_px);
    let popup_y = (surface_h - popup_h).max(layout.tab_bar_h);

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

    super::shape_popup_line(
        renderer,
        font_system,
        &text,
        popup_x + layout.cell_w,
        popup_y + layout.cell_h,
        font_system.baseline_offset(),
        layout.cell_w,
        layout.cell_h,
        text_fg,
        fg,
    );
}

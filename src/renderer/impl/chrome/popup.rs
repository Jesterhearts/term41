use super::super::*;

pub(in crate::renderer::r#impl) fn shape_centered_popup_line(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    text: &str,
    panel: (f32, f32, f32, f32),
    y: f32,
    baseline: f32,
    cell_w: f32,
    color: u32,
    fg: &mut FgGeometry,
) {
    let width = text.chars().count() as f32 * cell_w;
    let x = panel.0 + (panel.2 - width) * 0.5;
    shape_popup_line(
        renderer,
        font_system,
        text,
        x,
        y,
        baseline,
        cell_w,
        0.0,
        color,
        fg,
    );
}

/// Shape a single line of popup text and push its glyph quads.
pub(in crate::renderer::r#impl) fn shape_popup_line(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    text: &str,
    x: f32,
    y: f32,
    baseline: f32,
    cell_w: f32,
    _cell_h: f32,
    color: u32,
    fg: &mut FgGeometry,
) {
    let cells: Vec<smol_str::SmolStr> = text
        .chars()
        .map(|c| {
            let mut buf = [0u8; 4];
            smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf))
        })
        .collect();
    let attrs = vec![CellAttrs::default(); cells.len()];
    let shaped = font_system.shape_row(&cells, &attrs);

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

        let gx = x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
        let gx = gx.floor();

        let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
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

pub(super) fn push_bordered_panel(
    panel: (f32, f32, f32, f32),
    panel_bg: u32,
    border: u32,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
) {
    push_rect(
        panel.0,
        panel.1,
        panel.2,
        panel.3,
        panel_bg,
        bg_vertices,
        bg_indices,
    );
    push_rect(
        panel.0,
        panel.1,
        panel.2,
        1.0,
        border,
        bg_vertices,
        bg_indices,
    );
    push_rect(
        panel.0,
        panel.1 + panel.3 - 1.0,
        panel.2,
        1.0,
        border,
        bg_vertices,
        bg_indices,
    );
    push_rect(
        panel.0,
        panel.1,
        1.0,
        panel.3,
        border,
        bg_vertices,
        bg_indices,
    );
    push_rect(
        panel.0 + panel.2 - 1.0,
        panel.1,
        1.0,
        panel.3,
        border,
        bg_vertices,
        bg_indices,
    );
}

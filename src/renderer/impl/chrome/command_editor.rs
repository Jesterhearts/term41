use super::super::*;

#[allow(clippy::too_many_arguments)]
pub(in crate::renderer::r#impl) fn render_command_editor(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    editor: &commands41::CommandLineView,
    layout: &FrameLayout,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
    fg: &mut FgGeometry,
) {
    let Some(completion) = command_completion_layout(snap, layout) else {
        return;
    };
    if let Some(text) = editor.completion.as_deref() {
        super::shape_and_render_label(
            renderer,
            font_system,
            &truncate_command_label(text, completion.cols),
            completion.x,
            completion.y,
            layout.baseline,
            layout.cell_w,
            None,
            None,
            pack_color(&Srgb::new(125, 136, 155), 255),
            fg,
        );
    }
    if editor.candidates.is_empty() {
        return;
    }
    let list_cells = editor
        .candidates
        .iter()
        .map(|candidate| unicode_width::UnicodeWidthStr::width(candidate.as_str()) + 2)
        .max()
        .unwrap_or(1)
        .min(snap.viewport_cols as usize)
        .max(1);
    let list_w = list_cells as f32 * layout.cell_w;
    let list_h = editor.candidates.len().min(snap.viewport_rows as usize) as f32 * layout.cell_h;
    let list_x = completion
        .x
        .min(layout.gutter_px + snap.viewport_cols as f32 * layout.cell_w - list_w);
    let list_y = command_completion_list_y(&completion, list_h, snap, layout);
    push_rect(
        list_x,
        list_y,
        list_w,
        list_h,
        pack_color(&Srgb::new(22, 25, 34), 245),
        bg_vertices,
        bg_indices,
    );
    for (idx, candidate) in editor
        .candidates
        .iter()
        .take(snap.viewport_rows as usize)
        .enumerate()
    {
        let row_y = list_y + idx as f32 * layout.cell_h;
        let active = idx == editor.candidate_index;
        if active {
            push_rect(
                list_x,
                row_y,
                list_w,
                layout.cell_h,
                pack_color(&Srgb::new(42, 55, 78), 245),
                bg_vertices,
                bg_indices,
            );
        }
        super::shape_and_render_label(
            renderer,
            font_system,
            &truncate_command_label(candidate, list_cells.saturating_sub(1)),
            list_x + layout.cell_w,
            row_y,
            layout.baseline,
            layout.cell_w,
            None,
            None,
            if active {
                pack_color(&Srgb::new(225, 232, 255), 255)
            } else {
                pack_color(&Srgb::new(170, 180, 200), 255)
            },
            fg,
        );
    }
}

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
    let Some(box_layout) = command_editor_box_layout(snap, layout) else {
        return;
    };
    let border = 2.0;
    let editor_x = box_layout.editor_x;
    let box_x = box_layout.box_x;
    let box_y = box_layout.box_y;
    let box_w = box_layout.box_w;
    let editor_w = box_layout.editor_w;
    let editor_rows = box_layout.editor_rows;
    let box_h = box_layout.box_h;
    let content_x = box_layout.content_x;
    let lines = command_editor_line_ranges(&editor.text);
    let cursor = editor.cursor.min(editor.text.len());
    if !editor.text.is_char_boundary(cursor) {
        return;
    }
    let (cursor_line, cursor_line_start) = command_editor_cursor_line(&lines, cursor);
    let visible_start = command_editor_visible_line_start(lines.len(), cursor_line, editor_rows);
    let visible_end = (visible_start + editor_rows).min(lines.len());
    let has_overflow = lines.len() > editor_rows;
    let scrollbar_cols = u32::from(has_overflow);
    let content_cols = snap.viewport_cols.saturating_sub(1 + scrollbar_cols).max(1) as usize;

    push_rect(
        editor_x,
        box_y,
        editor_w,
        box_h,
        pack_color(&Srgb::new(18, 21, 29), 248),
        bg_vertices,
        bg_indices,
    );
    push_rect(
        editor_x,
        box_y,
        editor_w,
        border,
        pack_color(&Srgb::new(88, 150, 255), 255),
        bg_vertices,
        bg_indices,
    );

    if has_overflow {
        render_command_editor_scrollbar(
            box_x,
            box_y,
            box_w,
            box_h,
            border,
            layout,
            visible_start,
            editor_rows,
            lines.len(),
            bg_vertices,
            bg_indices,
        );
    }

    if let Some(selection) = editor.selection {
        let (selection_start, selection_end) = selection.ordered();
        for (visible_idx, &(line_start, line_end)) in
            lines[visible_start..visible_end].iter().enumerate()
        {
            let start = selection_start.max(line_start);
            let end = selection_end.min(line_end);
            if start >= end {
                continue;
            }
            let start_col = editor.text[line_start..start].graphemes(true).count();
            let end_col = editor.text[line_start..end]
                .graphemes(true)
                .count()
                .min(content_cols);
            if start_col >= end_col || start_col >= content_cols {
                continue;
            }
            push_rect(
                content_x + start_col as f32 * layout.cell_w,
                box_y + visible_idx as f32 * layout.cell_h,
                (end_col - start_col) as f32 * layout.cell_w,
                layout.cell_h,
                pack_color(&Srgb::new(55, 84, 132), 210),
                bg_vertices,
                bg_indices,
            );
        }
    }

    for (visible_idx, &(line_start, line_end)) in
        lines[visible_start..visible_end].iter().enumerate()
    {
        let line_y = box_y + visible_idx as f32 * layout.cell_h;
        for span in &editor.spans {
            if span.start >= span.end || span.end > editor.text.len() {
                continue;
            }
            let start = span.start.max(line_start);
            let end = span.end.min(line_end);
            if start >= end {
                continue;
            }
            let segment = &editor.text[start..end];
            if segment.trim().is_empty() {
                continue;
            }
            let col = editor.text[line_start..start].graphemes(true).count();
            if col >= content_cols {
                continue;
            }
            let label = truncate_graphemes(segment, content_cols - col);
            super::shape_and_render_label(
                renderer,
                font_system,
                &label,
                content_x + col as f32 * layout.cell_w,
                line_y,
                layout.baseline,
                layout.cell_w,
                None,
                None,
                command_highlight_color(span.kind),
                fg,
            );
        }
    }

    let cursor_line_visible = cursor_line >= visible_start && cursor_line < visible_end;
    let visible_cursor_line = cursor_line.saturating_sub(visible_start);
    let cursor_cell = editor.text[cursor_line_start..cursor]
        .graphemes(true)
        .count()
        .min(content_cols - 1);

    if let Some(completion) = editor.completion.as_deref()
        && cursor_line_visible
        && cursor_cell < content_cols
    {
        super::shape_and_render_label(
            renderer,
            font_system,
            &truncate_graphemes(completion, content_cols - cursor_cell),
            content_x + cursor_cell as f32 * layout.cell_w,
            box_y + visible_cursor_line as f32 * layout.cell_h,
            layout.baseline,
            layout.cell_w,
            None,
            None,
            pack_color(&Srgb::new(125, 136, 155), 255),
            fg,
        );
    }

    if cursor_line_visible {
        match editor.cursor_style {
            commands41::CommandEditorCursorStyle::Beam => {
                push_rect(
                    content_x + cursor_cell as f32 * layout.cell_w,
                    box_y + visible_cursor_line as f32 * layout.cell_h + 2.0,
                    2.0,
                    layout.cell_h - 4.0,
                    pack_color(&Srgb::new(230, 235, 255), 255),
                    bg_vertices,
                    bg_indices,
                );
            }
            commands41::CommandEditorCursorStyle::Block => {
                push_rect(
                    content_x + cursor_cell as f32 * layout.cell_w,
                    box_y + visible_cursor_line as f32 * layout.cell_h + 1.0,
                    layout.cell_w,
                    layout.cell_h - 2.0,
                    pack_color(&Srgb::new(230, 235, 255), 175),
                    bg_vertices,
                    bg_indices,
                );
            }
        }
    }

    if editor.candidates.is_empty() {
        return;
    }

    let list_cells = editor
        .candidates
        .iter()
        .map(|candidate| candidate.graphemes(true).count() + 2)
        .max()
        .unwrap_or(1)
        .min(content_cols)
        .max(1);
    let list_w = list_cells as f32 * layout.cell_w;
    let list_h = editor.candidates.len() as f32 * layout.cell_h;
    let cursor_y = box_y + visible_cursor_line as f32 * layout.cell_h;
    let editor_cursor_screen_row = box_layout.placement.top_row + visible_cursor_line as u32;
    let list_y =
        match command_editor_popup_side_for_row(editor_cursor_screen_row, snap.viewport_rows) {
            CommandEditorPopupSide::Below => {
                let preferred = cursor_y + layout.cell_h;
                preferred
                    .min(layout.tab_bar_h + snap.viewport_rows as f32 * layout.cell_h - list_h)
                    .max(layout.tab_bar_h)
            }
            CommandEditorPopupSide::Above => (cursor_y - list_h).max(layout.tab_bar_h),
        };

    push_rect(
        content_x,
        list_y,
        list_w,
        list_h,
        pack_color(&Srgb::new(22, 25, 34), 245),
        bg_vertices,
        bg_indices,
    );
    for (idx, candidate) in editor.candidates.iter().enumerate() {
        let row_y = list_y + idx as f32 * layout.cell_h;
        let active = idx == editor.candidate_index;
        if active {
            push_rect(
                content_x,
                row_y,
                list_w,
                layout.cell_h,
                pack_color(&Srgb::new(42, 55, 78), 245),
                bg_vertices,
                bg_indices,
            );
        }
        let label = truncate_graphemes(candidate, list_cells.saturating_sub(1));
        super::shape_and_render_label(
            renderer,
            font_system,
            &label,
            content_x + layout.cell_w,
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

fn command_editor_line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            ranges.push((start, idx));
            start = idx + ch.len_utf8();
        }
    }
    ranges.push((start, text.len()));
    ranges
}

fn command_editor_cursor_line(
    lines: &[(usize, usize)],
    cursor: usize,
) -> (usize, usize) {
    for (idx, &(start, end)) in lines.iter().enumerate() {
        if cursor <= end {
            return (idx, start);
        }
    }
    lines
        .last()
        .map(|&(start, _)| (lines.len().saturating_sub(1), start))
        .unwrap_or((0, 0))
}

fn command_editor_visible_line_start(
    line_count: usize,
    cursor_line: usize,
    visible_rows: usize,
) -> usize {
    let visible = visible_rows.max(1);
    if line_count <= visible {
        return 0;
    }
    cursor_line.saturating_add(1).saturating_sub(visible)
}

#[allow(clippy::too_many_arguments)]
fn render_command_editor_scrollbar(
    box_x: f32,
    box_y: f32,
    box_w: f32,
    box_h: f32,
    border: f32,
    layout: &FrameLayout,
    visible_start: usize,
    visible_rows: usize,
    total_lines: usize,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
) {
    let visible = visible_rows.max(1);
    if total_lines <= visible {
        return;
    }
    let track_h = (box_h - border).max(1.0);
    let track_w = (layout.cell_w * 0.18).max(2.0);
    let track_x = box_x + box_w - layout.cell_w * 0.5 - track_w * 0.5;
    let track_y = box_y + border;
    push_rect(
        track_x,
        track_y,
        track_w,
        track_h,
        pack_color(&Srgb::new(54, 62, 78), 220),
        bg_vertices,
        bg_indices,
    );

    let thumb_h = (track_h * visible as f32 / total_lines as f32).max(layout.cell_h * 0.45);
    let max_start = total_lines.saturating_sub(visible).max(1);
    let scroll_ratio = visible_start as f32 / max_start as f32;
    let thumb_y = track_y + (track_h - thumb_h).max(0.0) * scroll_ratio;
    push_rect(
        track_x,
        thumb_y,
        track_w,
        thumb_h,
        pack_color(&Srgb::new(145, 160, 190), 255),
        bg_vertices,
        bg_indices,
    );
}

fn truncate_graphemes(
    text: &str,
    max_cells: usize,
) -> String {
    let mut graphemes = text.graphemes(true);
    let mut out = String::new();
    for _ in 0..max_cells {
        let Some(grapheme) = graphemes.next() else {
            return out;
        };
        out.push_str(grapheme);
    }
    if graphemes.next().is_some() && max_cells >= 3 {
        out.truncate(
            out.grapheme_indices(true)
                .nth(max_cells - 3)
                .map_or(0, |(idx, _)| idx),
        );
        out.push_str("...");
    }
    out
}

use font41::attrs::CellAttrs;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use terminal41::Terminal;
use terminal41::view;

pub(super) fn apply_to_snapshot(
    snap: &mut TermSnapshot,
    sticky_prompt_row: Option<RowSnapshot>,
) {
    let Some(mut sticky_prompt_row) = sticky_prompt_row else {
        return;
    };
    if snap.on_alt_screen || snap.viewport_rows == 0 {
        return;
    }

    sticky_prompt_row.screen_row = 0;
    let mut sticky_prompt_row = Some(sticky_prompt_row);
    for row in &mut snap.rows {
        if row.screen_row == 0 && snap.status_line_row != Some(row.screen_row) {
            *row = sticky_prompt_row
                .take()
                .expect("sticky prompt row is replaced once");
            break;
        }
    }

    if let Some(row) = sticky_prompt_row {
        snap.rows.push(row);
        snap.rows.sort_by_key(|row| row.screen_row);
    }
    snap.reset_cached_rows = true;
}

pub(super) fn row_snapshot(
    terminal: &Terminal,
    snap: &TermSnapshot,
) -> Option<RowSnapshot> {
    if terminal.on_alt_screen() || snap.on_alt_screen || snap.viewport_rows == 0 {
        return None;
    }

    let prompt =
        view::sticky_prompt_above_view(terminal.active(), terminal.viewport(), snap.viewport_rows)?;
    Some(row_snapshot_for_sticky_prompt(
        terminal,
        prompt.row,
        prompt.rendered_row,
        prompt.active_absolute_row,
        sticky_prompt_generation(snap.generation, prompt.rendered_row),
    ))
}

fn row_snapshot_for_sticky_prompt(
    terminal: &Terminal,
    row: &terminal41::Row,
    rendered_row: u64,
    active_absolute_row: Option<u64>,
    generation: u64,
) -> RowSnapshot {
    let is_double = !matches!(row.line_attr, LineAttr::Normal);
    let cols = if is_double {
        terminal.viewport().cols / 2
    } else {
        terminal.viewport().cols
    };

    let mut snapshot = RowSnapshot {
        screen_row: 0,
        generation,
        cells: row.cells().to_vec(),
        attrs: row.attributes().to_vec(),
        fg: row.foregrounds().to_vec(),
        bg: row.backgrounds().to_vec(),
        fg_source: row.fg_sources().to_vec(),
        bg_source: row.bg_sources().to_vec(),
        underline_color: row.underline_colors().to_vec(),
        has_link: row.hyperlinks().iter().map(|link| link.is_some()).collect(),
        line_attr: row.line_attr,
        selected: (0..cols)
            .map(|col| {
                terminal41::selection::is_rendered_cell_selected(
                    terminal.selection(),
                    rendered_row,
                    col,
                ) || active_absolute_row.is_some_and(|absolute_row| {
                    terminal41::selection::is_absolute_cell_selected(
                        terminal.selection(),
                        absolute_row,
                        col,
                    )
                })
            })
            .collect(),
        matched: vec![false; cols as usize],
        active_match: vec![false; cols as usize],
        prompt_start: row.prompt_start,
        exit_status: row.exit_status,
        block_separator: false,
        sticky_prompt: true,
    };
    normalize_renderer_snapshot_row(&mut snapshot, terminal.viewport().cols, terminal);
    snapshot
}

fn normalize_renderer_snapshot_row(
    row: &mut RowSnapshot,
    cols: u32,
    terminal: &Terminal,
) {
    let cols = cols as usize;
    row.cells.resize(cols, smol_str::SmolStr::new_inline(" "));
    row.attrs.resize(cols, CellAttrs::default());
    row.fg.resize(cols, terminal.palette().fg);
    row.bg.resize(cols, terminal.palette().bg);
    row.fg_source.resize(cols, terminal41::ColorSource::Default);
    row.bg_source.resize(cols, terminal41::ColorSource::Default);
    row.underline_color.resize(cols, None);
    row.has_link.resize(cols, false);
}

fn sticky_prompt_generation(
    snapshot_generation: u64,
    rendered_row: u64,
) -> u64 {
    snapshot_generation
        .wrapping_mul(1_099_511_628_211)
        .wrapping_add(rendered_row)
        .wrapping_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_row_text(row: &RowSnapshot) -> String {
        row.cells.concat()
    }

    fn terminal_snapshot(terminal: &mut Terminal) -> TermSnapshot {
        let (mut publisher, mut output) = terminal41::terminal_snapshot_buffer(terminal);
        terminal41::publish_terminal_snapshot(terminal, &mut publisher);
        output.update();
        output.read().clone()
    }

    fn terminal_with_completed_block() -> Terminal {
        terminal_with_completed_block_rows(4)
    }

    fn terminal_with_completed_block_rows(rows: u32) -> Terminal {
        let mut terminal = Terminal::new(
            8,
            rows,
            100,
            config41::StatusLineMode::Off,
            config41::FeaturePermissions::default(),
            config41::TerminalLimits::default(),
            16,
            8,
            config41::ColorPalette::default(),
        );
        let mut processor = terminal41::TerminalProcessor::new();

        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07$ abc\x1b]133;B\x07");
        processor.process_bytes(&mut terminal, b"\r\n\x1b]133;C\x07out1");
        processor.process_bytes(&mut terminal, b"\r\nout2\r\nout3\r\nout4\r\nout5\r\nout6");
        processor.process_bytes(&mut terminal, b"\x1b]133;D;0\x07");
        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07$ ");
        terminal
    }

    #[test]
    fn sticky_prompt_is_applied_as_render_snapshot_postprocess() {
        let mut terminal = terminal_with_completed_block();
        let target_top = 4;
        let max_top = (view::rendered_rows_len(terminal.active(), terminal.viewport()) as u32)
            .saturating_sub(terminal.viewport().rows);
        terminal.set_viewport_offset(max_top.saturating_sub(target_top));

        let mut snap = terminal_snapshot(&mut terminal);
        let sticky_prompt = row_snapshot(&terminal, &snap);
        apply_to_snapshot(&mut snap, sticky_prompt);

        assert_eq!(snapshot_row_text(&snap.rows[0]), "$ abc   ");
        assert!(snap.rows[0].prompt_start);
        assert_eq!(snapshot_row_text(&snap.rows[1]), "out5    ");
        assert_eq!(snapshot_row_text(&snap.rows[2]), "out6    ");
        assert_eq!(snapshot_row_text(&snap.rows[3]).trim_end(), "");
    }

    #[test]
    fn prompt_row_does_not_stick_when_viewport_starts_on_prompt() {
        let mut terminal = terminal_with_completed_block();
        let max_top = (view::rendered_rows_len(terminal.active(), terminal.viewport()) as u32)
            .saturating_sub(terminal.viewport().rows);
        terminal.set_viewport_offset(max_top);

        let snap = terminal_snapshot(&mut terminal);

        assert!(row_snapshot(&terminal, &snap).is_none());
    }

    #[test]
    fn sticky_prompt_replaces_separator_at_live_bottom() {
        let mut terminal = terminal_with_completed_block_rows(2);
        terminal.set_viewport_offset(0);
        terminal.invalidate_snapshot_rows();

        let mut snap = terminal_snapshot(&mut terminal);
        let sticky_prompt = row_snapshot(&terminal, &snap);

        assert!(snap.rows[0].block_separator);
        assert!(sticky_prompt.is_some());

        apply_to_snapshot(&mut snap, sticky_prompt);

        assert_eq!(snapshot_row_text(&snap.rows[0]), "$ abc   ");
        assert!(snap.rows[0].prompt_start);
        assert_eq!(snapshot_row_text(&snap.rows[1]).trim_end(), "$");
    }

    #[test]
    fn sticky_prompt_does_not_push_live_cursor_offscreen() {
        let mut terminal = terminal_with_completed_block();
        terminal.set_viewport_offset(0);
        terminal.invalidate_snapshot_rows();

        let mut snap = terminal_snapshot(&mut terminal);
        let sticky_prompt = row_snapshot(&terminal, &snap);

        assert!(sticky_prompt.is_some());
        assert_eq!(snap.cursor, Some((terminal.viewport().rows - 1, 2)));
        assert_eq!(snapshot_row_text(snap.rows.last().unwrap()).trim_end(), "$");

        apply_to_snapshot(&mut snap, sticky_prompt);

        assert_eq!(snapshot_row_text(&snap.rows[0]), "$ abc   ");
        assert!(snap.rows[0].prompt_start);
        assert_eq!(snap.cursor, Some((terminal.viewport().rows - 1, 2)));
        assert_eq!(snapshot_row_text(snap.rows.last().unwrap()).trim_end(), "$");
    }

    #[test]
    fn sticky_prompt_selection_uses_absolute_active_rows_after_completed_blocks() {
        let mut terminal = terminal_with_completed_block_rows(3);
        let mut processor = terminal41::TerminalProcessor::new();
        processor.process_bytes(
            &mut terminal,
            b"hit\x1b]133;B\x07\r\n\x1b]133;C\x07out1\r\nout2\r\nout3\r\nout4\r\nout5\r\nout6",
        );

        for (query, expected) in [
            ("hit", [false, false, true, true, true, false, false, false]),
            ("out4", [false; 8]),
        ] {
            terminal.update_search(view::SearchAction::Open);
            terminal.update_search(view::SearchAction::Append(query));
            terminal.update_search(view::SearchAction::Close);
            terminal.set_viewport_offset(0);
            let mut snap = terminal_snapshot(&mut terminal);
            let sticky = row_snapshot(&terminal, &snap).expect("active prompt above output");
            assert_eq!(snapshot_row_text(&sticky), "$ hit   ");
            assert_eq!(
                sticky.selected, expected,
                "selection from search for {query}"
            );
            apply_to_snapshot(&mut snap, Some(sticky));
            assert_eq!(snap.rows[0].selected, expected);
        }
    }
}

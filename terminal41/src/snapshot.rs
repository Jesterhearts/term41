use font41::attrs::CellAttrs;
use palette::Srgb;
use smol_str::SmolStrBuilder;
use tracing::debug_span;
use unicode_segmentation::UnicodeSegmentation;
use utils41::blend_colors;

use crate::ColorPalette;
use crate::LineAttr;
use crate::StatusDisplayKind;
use crate::Terminal;
use crate::host;
use crate::selection::is_cell_active_match;
use crate::selection::is_cell_match;
use crate::selection::is_cell_selected;
use crate::selection::is_rendered_cell_selected;
use crate::selection::search_active;
use crate::selection::search_state;
use crate::view;

/// Per-row snapshot of terminal state.
#[derive(Debug, Clone)]
pub struct RowSnapshot {
    /// Row index in the rendered terminal surface. Visible terminal rows start
    /// at 0; a visible status line uses `viewport_rows`.
    pub screen_row: u32,
    /// Monotonic generation of this row. Renderers can skip rows whose
    /// generation matches the last generation they consumed.
    pub generation: u64,
    pub cells: Vec<smol_str::SmolStr>,
    pub attrs: Vec<CellAttrs>,
    pub fg: Vec<Srgb<u8>>,
    pub bg: Vec<Srgb<u8>>,
    pub underline_color: Vec<Option<Srgb<u8>>>,
    pub has_link: Vec<bool>,
    pub line_attr: LineAttr,
    pub selected: Vec<bool>,
    pub matched: Vec<bool>,
    pub active_match: Vec<bool>,
    /// Shell-integration: this row starts a prompt.
    pub prompt_start: bool,
    /// Shell-integration: exit status of the command at this prompt.
    pub exit_status: Option<i32>,
    /// Renderer-only row inserted between command blocks.
    pub block_separator: bool,
}

/// Snapshot of the search bar state for rendering.
#[derive(Debug, Clone)]
pub struct SearchSnapshot {
    pub query: String,
    pub match_count: usize,
    pub active_idx: usize,
}

/// All terminal state needed for one render frame, captured under the lock.
#[derive(Debug, Clone)]
pub struct TermSnapshot {
    /// Monotonic generation of any renderer-visible terminal change.
    pub generation: u64,
    /// Every row in the visible viewport, plus the visible status line when
    /// present.
    pub rows: Vec<RowSnapshot>,
    pub total_rows: u32,
    pub viewport_rows: u32,
    pub viewport_cols: u32,
    /// Non-zero when the viewport is scrolled back from the live bottom.
    pub viewport_offset: u32,
    pub status_line_row: Option<u32>,
    pub drcs_glyphs: font41::DrcsGlyphMap,
    pub dec_color: crate::DecColorState,
    pub palette: ColorPalette,
    pub search_active: bool,
    pub search: Option<SearchSnapshot>,
    /// Cursor position (row, col) if visible and not scrolled off.
    pub cursor: Option<(u32, u32)>,
    pub cursor_style: crate::CursorStyle,
    /// DECSCNM — screen-wide reverse video. When true, default fg/bg are
    /// swapped and per-cell REVERSE is XORed with this.
    pub screen_reverse: bool,
    pub on_alt_screen: bool,
    /// True while the command editor should not render or intercept input.
    pub command_editor_hidden: bool,
    pub synchronized_update_active: bool,
    pub current_title: Option<String>,
    /// True when the consumer should discard any cached rows before applying
    /// this snapshot.
    pub reset_cached_rows: bool,
}

pub type TermSnapshotInput = triple_buffer::Input<TermSnapshot>;
pub type TermSnapshotOutput = triple_buffer::Output<TermSnapshot>;
pub type TermSnapshotPublisher = TermSnapshotInput;

/// Row-generation state for terminal snapshots.
///
/// Keep renderer invalidation in this single sidecar vector rather than on
/// `Row` itself.
#[derive(Debug, Default)]
pub(crate) struct SnapshotState {
    row_generations: Vec<u64>,
    generation: u64,
    shape: Option<SnapshotShape>,
}

impl SnapshotState {
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }

    pub(crate) fn mark_row(
        &mut self,
        row: u32,
    ) {
        let generation = self.next_generation();
        self.mark_row_with_generation(row, generation);
    }

    fn mark_row_with_generation(
        &mut self,
        row: u32,
        generation: u64,
    ) {
        let idx = row as usize;
        if idx >= self.row_generations.len() {
            self.row_generations.resize(idx + 1, 0);
        }
        self.row_generations[idx] = generation;
    }

    pub(crate) fn mark_rows(
        &mut self,
        start: u32,
        end: u32,
    ) {
        let generation = self.next_generation();
        for row in start.min(end)..=start.max(end) {
            self.mark_row_with_generation(row, generation);
        }
    }

    pub(crate) fn mark_all(&mut self) {
        let generation = self.next_generation();
        for row_generation in &mut self.row_generations {
            *row_generation = generation;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotShape {
    total_rows: u32,
    viewport_rows: u32,
    rendered_terminal_rows: u32,
    viewport_cols: u32,
    status_line_row: Option<u32>,
}

pub fn terminal_snapshot_buffer(
    terminal: &mut Terminal
) -> (TermSnapshotPublisher, TermSnapshotOutput) {
    let (input, output) = triple_buffer::triple_buffer(&snapshot_terminal(terminal));
    (input, output)
}

pub fn publish_terminal_snapshot(
    terminal: &mut Terminal,
    publisher: &mut TermSnapshotPublisher,
) {
    publisher.write(snapshot_terminal(terminal));
}

/// Snapshot the terminal's visible state under the lock.
pub(crate) fn snapshot_terminal(terminal: &mut Terminal) -> TermSnapshot {
    let vp_rows = terminal.viewport.rows;
    let vp_cols = terminal.viewport.cols;
    let search_active = search_active(&terminal.search);
    let status_line_row = view::status_line_row(&terminal.active).map(|_| vp_rows);
    let rendered_terminal_rows = if terminal.on_alt_screen {
        vp_rows
    } else {
        (crate::screen::rendered_rows_len(&terminal.active) as u32)
            .min(vp_rows)
            .max(1)
    };
    let terminal_rows = vp_rows;
    let total_rows = terminal_rows + u32::from(status_line_row.is_some());
    let shape = SnapshotShape {
        total_rows,
        viewport_rows: terminal_rows,
        rendered_terminal_rows,
        viewport_cols: vp_cols,
        status_line_row,
    };
    let reset_cached_rows = terminal.snapshot.shape != Some(shape);
    if reset_cached_rows {
        let generation = terminal.snapshot.next_generation();
        terminal.snapshot.row_generations = vec![generation; total_rows as usize];
        terminal.snapshot.shape = Some(shape);
    } else {
        ensure_snapshot_len(&mut terminal.snapshot, total_rows as usize);
    }

    let mut rows = Vec::new();
    debug_span!("copying visible rows").in_scope(|| {
        for row in 0..rendered_terminal_rows {
            let generation = row_generation(&terminal.snapshot, row);
            rows.push(snapshot_rendered_row(
                terminal,
                row,
                rendered_terminal_rows,
                generation,
            ));
        }
    });

    if status_line_row.is_some()
        && let Some(status_row) = snapshot_status_line_row(
            terminal,
            vp_cols,
            row_generation(&terminal.snapshot, terminal_rows),
        )
    {
        rows.push(status_row);
    }

    let search = search_state(&terminal.search).map(|s| SearchSnapshot {
        query: s.query.clone(),
        match_count: s.matches.len(),
        active_idx: s.active_idx,
    });

    let cursor = if terminal.active.offset == 0 && terminal.active.cursor_visible {
        if let Some(col) = view::status_line_cursor_col(&terminal.active) {
            Some((terminal_rows, col))
        } else {
            let active_top = active_block_screen_top(terminal, terminal_rows);
            Some((
                active_top + terminal.active.cursor.row,
                terminal.active.cursor.col,
            ))
        }
    } else {
        None
    };

    TermSnapshot {
        generation: terminal.snapshot.generation,
        rows,
        total_rows,
        viewport_rows: terminal_rows,
        viewport_cols: vp_cols,
        viewport_offset: terminal.active.offset,
        status_line_row,
        drcs_glyphs: terminal.drcs_render_glyphs(),
        dec_color: terminal.dec_color_state().clone(),
        palette: terminal.palette.clone(),
        search_active,
        search,
        cursor,
        cursor_style: terminal.cursor_style,
        screen_reverse: terminal.modes.screen_reverse,
        on_alt_screen: terminal.on_alt_screen,
        command_editor_hidden: command_editor_hidden(terminal),
        synchronized_update_active: host::synchronized_update_active(
            terminal.modes.synchronized_update_since,
        ),
        current_title: terminal.metadata.current_title.clone(),
        reset_cached_rows,
    }
}

fn command_editor_hidden(terminal: &Terminal) -> bool {
    if terminal.metadata.shell_integration_phase == crate::ShellIntegrationPhase::Output {
        return true;
    }
    terminal.metadata.shell_integration_phase != crate::ShellIntegrationPhase::Command
        && (host::mouse_tracking_enabled(terminal.modes.mouse_tracking)
            || terminal.active.app_cursor_keys
            || terminal.active.app_keypad)
}

fn ensure_snapshot_len(
    snapshot: &mut SnapshotState,
    len: usize,
) {
    if len > snapshot.row_generations.len() {
        snapshot.row_generations.resize(len, snapshot.generation);
    }
}

fn row_generation(
    snapshot: &SnapshotState,
    screen_row: u32,
) -> u64 {
    let idx = screen_row as usize;
    snapshot.row_generations[idx]
}

fn snapshot_rendered_row(
    terminal: &Terminal,
    row: u32,
    terminal_rows: u32,
    generation: u64,
) -> RowSnapshot {
    if terminal.on_alt_screen {
        return snapshot_grid_row(
            terminal,
            row,
            view::visible_row(&terminal.active, &terminal.viewport, row),
            generation,
            Some(row),
            None,
        );
    }
    match rendered_row(terminal, row, terminal_rows) {
        RenderedRow::Separator => {
            separator_terminal_row(row, terminal.viewport.cols, &terminal.palette, generation)
        }
        RenderedRow::Active {
            rendered_row,
            block_row,
            grid_row,
        } => snapshot_grid_row(
            terminal,
            row,
            grid_row,
            generation,
            Some(block_row),
            Some(rendered_row),
        ),
        RenderedRow::History {
            rendered_row,
            grid_row,
        } => snapshot_grid_row(
            terminal,
            row,
            grid_row,
            generation,
            None,
            Some(rendered_row),
        ),
    }
}

fn snapshot_grid_row(
    terminal: &Terminal,
    row: u32,
    grid_row: &crate::Row,
    generation: u64,
    active_screen_row: Option<u32>,
    rendered_row: Option<u32>,
) -> RowSnapshot {
    let is_double = !matches!(grid_row.line_attr, LineAttr::Normal);
    let cols = if is_double {
        terminal.viewport.cols / 2
    } else {
        terminal.viewport.cols
    };

    let mut snapshot = RowSnapshot {
        screen_row: row,
        generation,
        cells: grid_row.cells.clone(),
        attrs: grid_row.attrs.clone(),
        fg: grid_row.fg.clone(),
        bg: grid_row.bg.clone(),
        underline_color: grid_row.underline_color.clone(),
        has_link: grid_row.links.iter().map(|l| l.is_some()).collect(),
        line_attr: grid_row.line_attr,
        selected: (0..cols)
            .map(|c| {
                rendered_row.is_some_and(|rendered_row| {
                    is_rendered_cell_selected(terminal.selection.as_ref(), rendered_row, c)
                }) || active_screen_row.is_some_and(|active_row| {
                    is_cell_selected(
                        terminal.selection.as_ref(),
                        &terminal.active,
                        &terminal.viewport,
                        active_row,
                        c,
                    )
                })
            })
            .collect(),
        matched: (0..cols)
            .map(|c| {
                active_screen_row.is_some_and(|active_row| {
                    is_cell_match(
                        &terminal.search,
                        &terminal.active,
                        &terminal.viewport,
                        active_row,
                        c,
                    )
                })
            })
            .collect(),
        active_match: (0..cols)
            .map(|c| {
                active_screen_row.is_some_and(|active_row| {
                    is_cell_active_match(
                        &terminal.search,
                        &terminal.active,
                        &terminal.viewport,
                        active_row,
                        c,
                    )
                })
            })
            .collect(),
        prompt_start: grid_row.prompt_start,
        exit_status: grid_row.exit_status,
        block_separator: false,
    };
    normalize_snapshot_row(&mut snapshot, terminal.viewport.cols, &terminal.palette);
    snapshot
}

enum RenderedRow<'a> {
    Separator,
    History {
        rendered_row: u32,
        grid_row: &'a crate::Row,
    },
    Active {
        rendered_row: u32,
        block_row: u32,
        grid_row: &'a crate::Row,
    },
}

fn rendered_row<'a>(
    terminal: &'a Terminal,
    screen_row: u32,
    terminal_rows: u32,
) -> RenderedRow<'a> {
    let rendered_len = crate::screen::rendered_rows_len(&terminal.active) as u32;
    let max_top = rendered_len.saturating_sub(terminal_rows);
    let top = max_top.saturating_sub(terminal.active.offset);
    let mut idx = top + screen_row;
    let rendered_row = idx;
    for block in &terminal.active.scrollback_blocks {
        let block_rows = crate::screen::command_block_rendered_rows_len(block) as u32;
        if idx < block_rows {
            return RenderedRow::History {
                rendered_row,
                grid_row: &block.grid.rows[idx as usize],
            };
        }
        idx -= block_rows;
        if idx == 0 {
            return RenderedRow::Separator;
        }
        idx -= 1;
    }
    let local = idx.min(terminal.active.grid.rows.len().saturating_sub(1) as u32);
    RenderedRow::Active {
        rendered_row,
        block_row: local,
        grid_row: &terminal.active.grid.rows[local as usize],
    }
}

fn active_block_screen_top(
    terminal: &Terminal,
    terminal_rows: u32,
) -> u32 {
    let rendered_len = crate::screen::rendered_rows_len(&terminal.active) as u32;
    let max_top = rendered_len.saturating_sub(terminal_rows);
    let top = max_top.saturating_sub(terminal.active.offset);
    let history_len = terminal
        .active
        .scrollback_blocks
        .iter()
        .map(|block| crate::screen::command_block_rendered_rows_len(block) as u32)
        .map(|rows| rows + 1)
        .sum::<u32>();
    history_len.saturating_sub(top)
}

fn snapshot_status_line_row(
    terminal: &Terminal,
    vp_cols: u32,
    generation: u64,
) -> Option<RowSnapshot> {
    if view::status_display_kind(&terminal.active) == StatusDisplayKind::Indicator {
        let text =
            view::indicator_status_text(&terminal.metadata, &terminal.active).unwrap_or_default();
        return Some(status_line_indicator_row(
            &text,
            UdkIndicator {
                enabled: terminal.udk_feature_enabled(),
                locked: terminal.udks_locked(),
                keys: terminal
                    .programmed_udk_selectors()
                    .into_iter()
                    .filter_map(udk_selector_label)
                    .map(str::to_string)
                    .collect(),
            },
            vp_cols,
            &terminal.palette,
            terminal.viewport.rows,
            generation,
        ));
    }
    let grid_row = view::status_line_row(&terminal.active)?;
    let mut snapshot = RowSnapshot {
        screen_row: terminal.viewport.rows,
        generation,
        cells: grid_row.cells.clone(),
        attrs: grid_row.attrs.clone(),
        fg: grid_row.fg.clone(),
        bg: grid_row.bg.clone(),
        underline_color: grid_row.underline_color.clone(),
        has_link: grid_row.links.iter().map(|l| l.is_some()).collect(),
        line_attr: grid_row.line_attr,
        selected: vec![false; vp_cols as usize],
        matched: vec![false; vp_cols as usize],
        active_match: vec![false; vp_cols as usize],
        prompt_start: false,
        exit_status: None,
        block_separator: false,
    };
    normalize_snapshot_row(&mut snapshot, vp_cols, &terminal.palette);
    Some(snapshot)
}

fn normalize_snapshot_row(
    row: &mut RowSnapshot,
    cols: u32,
    palette: &ColorPalette,
) {
    let cols = cols as usize;
    row.cells.resize(cols, blank_cell());
    row.attrs.resize(cols, CellAttrs::default());
    row.fg.resize(cols, palette.fg);
    row.bg.resize(cols, palette.bg);
    row.underline_color.resize(cols, None);
    row.has_link.resize(cols, false);
}

fn blank_terminal_row(
    screen_row: u32,
    cols: u32,
    palette: &ColorPalette,
    generation: u64,
) -> RowSnapshot {
    let mut row = blank_status_line_row(cols as usize, palette, screen_row);
    row.generation = generation;
    row.fg.fill(palette.fg);
    row.bg.fill(palette.bg);
    row
}

fn separator_terminal_row(
    screen_row: u32,
    cols: u32,
    palette: &ColorPalette,
    generation: u64,
) -> RowSnapshot {
    let mut row = blank_terminal_row(screen_row, cols, palette, generation);
    row.block_separator = true;
    row
}

fn blank_cell() -> smol_str::SmolStr {
    smol_str::SmolStr::new_inline(" ")
}

struct UdkIndicator {
    enabled: bool,
    locked: bool,
    keys: Vec<String>,
}

fn status_line_indicator_row(
    text: &str,
    udks: UdkIndicator,
    cols: u32,
    palette: &ColorPalette,
    screen_row: u32,
    generation: u64,
) -> RowSnapshot {
    let mut row = blank_status_line_row(cols as usize, palette, screen_row);
    row.generation = generation;
    let right = format_udk_indicator(udks);
    let left_graphemes: Vec<_> = text.graphemes(true).collect();
    let right_graphemes: Vec<_> = right.graphemes(true).collect();
    let left_budget = if right_graphemes.is_empty() {
        cols as usize
    } else {
        (cols as usize).saturating_sub(right_graphemes.len() + 2)
    };
    let clipped_left = clip_status_line_tail(&left_graphemes, left_budget);

    for (idx, grapheme) in clipped_left.into_iter().enumerate() {
        set_status_cell(&mut row, idx, grapheme, palette.status_line_fg);
    }

    if !right_graphemes.is_empty() {
        let start = (cols as usize).saturating_sub(right_graphemes.len());
        let warning_fg = Srgb::new(224, 116, 116);
        let dim_fg = blend_colors(palette.status_line_fg, palette.status_line_bg, 0.45);
        let mut in_badge = false;
        for (offset, grapheme) in right_graphemes.into_iter().enumerate() {
            if grapheme == "[" {
                in_badge = true;
            }
            let fg = if in_badge { warning_fg } else { dim_fg };
            set_status_cell(&mut row, start + offset, grapheme, fg);
            if grapheme == "]" {
                in_badge = false;
            }
        }
    }

    row
}

fn blank_status_line_row(
    cols: usize,
    palette: &ColorPalette,
    screen_row: u32,
) -> RowSnapshot {
    RowSnapshot {
        screen_row,
        generation: 0,
        line_attr: LineAttr::Normal,
        fg: vec![palette.status_line_fg; cols],
        bg: vec![palette.status_line_bg; cols],
        attrs: vec![CellAttrs::default(); cols],
        selected: vec![false; cols],
        matched: vec![false; cols],
        active_match: vec![false; cols],
        cells: vec![smol_str::SmolStr::new_inline(" "); cols],
        exit_status: None,
        block_separator: false,
        has_link: vec![false; cols],
        underline_color: vec![None; cols],
        prompt_start: false,
    }
}

fn set_status_cell(
    row: &mut RowSnapshot,
    idx: usize,
    grapheme: &str,
    fg: Srgb<u8>,
) {
    if idx >= row.cells.len() {
        return;
    }
    let mut builder = SmolStrBuilder::new();
    builder.push_str(grapheme);
    row.cells[idx] = builder.finish();
    row.fg[idx] = fg;
}

fn format_udk_indicator(udks: UdkIndicator) -> String {
    if !udks.enabled {
        return String::new();
    }
    if udks.keys.is_empty() {
        return "UDK enabled".to_string();
    }
    let mut out = if udks.locked {
        "UDK locked".to_string()
    } else {
        "UDK".to_string()
    };
    for key in udks.keys {
        out.push(' ');
        out.push('[');
        out.push_str(&key);
        out.push(']');
    }
    out
}

fn clip_status_line_tail<'a>(
    segments: &[&'a str],
    cols: usize,
) -> Vec<&'a str> {
    if segments.len() <= cols {
        return segments.to_vec();
    }
    if cols == 0 {
        return Vec::new();
    }
    if cols == 1 {
        return vec!["..."];
    }
    let keep = cols - 2;
    let mut clipped = Vec::with_capacity(cols);
    clipped.push("... ");
    clipped.extend_from_slice(&segments[segments.len() - keep..]);
    clipped
}

fn udk_selector_label(selector: u16) -> Option<&'static str> {
    match selector {
        17 => Some("F6"),
        18 => Some("F7"),
        19 => Some("F8"),
        20 => Some("F9"),
        21 => Some("F10"),
        23 => Some("F11"),
        24 => Some("F12"),
        25 => Some("F13"),
        26 => Some("F14"),
        28 => Some("F15"),
        29 => Some("F16"),
        31 => Some("F17"),
        32 => Some("F18"),
        33 => Some("F19"),
        34 => Some("F20"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use config41::StatusLineMode;

    use super::*;
    use crate::FeaturePermissions;
    use crate::TerminalLimits;
    use crate::TerminalProcessor;
    use crate::selection::SelectionMode;
    use crate::selection::extend_rendered_selection;
    use crate::selection::extend_selection;
    use crate::selection::start_rendered_selection;
    use crate::selection::start_selection;

    fn terminal() -> Terminal {
        Terminal::new(
            5,
            3,
            10,
            StatusLineMode::Off,
            FeaturePermissions::default(),
            TerminalLimits::default(),
            16,
            8,
            ColorPalette::default(),
        )
    }

    #[test]
    fn first_snapshot_contains_every_visible_row() {
        let mut terminal = terminal();

        let snap = snapshot_terminal(&mut terminal);

        assert!(snap.reset_cached_rows);
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(
            snap.rows
                .iter()
                .map(|row| row.screen_row)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn unchanged_snapshot_keeps_row_generations() {
        let mut terminal = terminal();
        let first = snapshot_terminal(&mut terminal);

        let snap = snapshot_terminal(&mut terminal);

        assert!(!snap.reset_cached_rows);
        assert_eq!(snap.rows.len(), first.rows.len());
        assert_eq!(
            snap.rows
                .iter()
                .map(|row| row.generation)
                .collect::<Vec<_>>(),
            first
                .rows
                .iter()
                .map(|row| row.generation)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn text_write_bumps_only_dirty_cursor_row_generation() {
        let mut terminal = terminal();
        let first = snapshot_terminal(&mut terminal);

        TerminalProcessor::new().process_bytes(&mut terminal, b"A");
        let snap = snapshot_terminal(&mut terminal);

        assert!(!snap.reset_cached_rows);
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].screen_row, 0);
        assert_eq!(snap.rows[0].cells[0].as_str(), "A");
        assert_ne!(snap.rows[0].generation, first.rows[0].generation);
        for row in 1..snap.rows.len() {
            assert_eq!(snap.rows[row].generation, first.rows[row].generation);
        }
    }

    #[test]
    fn clear_to_end_of_wrapped_row_drops_stale_continuations() {
        let mut terminal = terminal();
        let mut processor = TerminalProcessor::new();

        processor.process_bytes(&mut terminal, b"abcdefghijk");
        let first = snapshot_terminal(&mut terminal);
        assert_eq!(snapshot_row_text(&first.rows[0]), "abcde");
        assert_eq!(snapshot_row_text(&first.rows[1]), "fghij");
        assert_eq!(snapshot_row_text(&first.rows[2]), "k    ");

        processor.process_bytes(&mut terminal, b"\x1b[2A\rbcdefghijk\x1b[K");
        let snap = snapshot_terminal(&mut terminal);

        assert!(snap.reset_cached_rows);
        assert_eq!(snapshot_row_text(&snap.rows[0]), "bcdef");
        assert_eq!(snapshot_row_text(&snap.rows[1]), "ghij ");
        assert_ne!(snap.rows[1].generation, first.rows[1].generation);
    }

    #[test]
    fn snapshot_rows_are_normalized_to_viewport_width() {
        let mut terminal = terminal();
        terminal.active.grid.rows[0].truncate(4);

        let snap = snapshot_terminal(&mut terminal);
        let row = &snap.rows[0];

        assert_eq!(snap.viewport_cols, 5);
        assert_eq!(row.cells.len(), 5);
        assert_eq!(row.attrs.len(), 5);
        assert_eq!(row.fg.len(), 5);
        assert_eq!(row.bg.len(), 5);
        assert_eq!(row.underline_color.len(), 5);
        assert_eq!(row.has_link.len(), 5);
    }

    #[test]
    fn bottom_aligned_active_block_selection_uses_active_row() {
        let mut terminal = terminal();
        let mut processor = TerminalProcessor::new();

        processor.process_bytes(&mut terminal, b"old");
        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07new");
        terminal.selection = start_selection(
            &terminal.active,
            &terminal.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        terminal.selection = extend_selection(
            &terminal.selection.clone().unwrap(),
            &terminal.active,
            &terminal.viewport,
            2,
            0,
        );

        let snap = snapshot_terminal(&mut terminal);
        let active = snap.rows.last().unwrap();

        assert_eq!(snapshot_row_text(active), "new  ");
        assert_eq!(active.screen_row, 2);
        assert_eq!(&active.selected[..3], &[true, true, true]);
    }

    #[test]
    fn active_block_selection_uses_active_row_after_multiple_blocks() {
        let mut terminal = Terminal::new(
            5,
            5,
            10,
            StatusLineMode::Off,
            FeaturePermissions::default(),
            TerminalLimits::default(),
            16,
            8,
            ColorPalette::default(),
        );
        let mut processor = TerminalProcessor::new();

        processor.process_bytes(&mut terminal, b"one");
        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07two");
        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07three");
        terminal.selection = start_selection(
            &terminal.active,
            &terminal.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        terminal.selection = extend_selection(
            &terminal.selection.clone().unwrap(),
            &terminal.active,
            &terminal.viewport,
            4,
            0,
        );

        let snap = snapshot_terminal(&mut terminal);
        let active = snap.rows.last().unwrap();

        assert_eq!(snapshot_row_text(active), "three");
        assert_eq!(active.screen_row, 4);
        assert_eq!(&active.selected[..5], &[true, true, true, true, true]);
    }

    #[test]
    fn rendered_block_selection_highlights_completed_blocks() {
        let mut terminal = Terminal::new(
            5,
            5,
            10,
            StatusLineMode::Off,
            FeaturePermissions::default(),
            TerminalLimits::default(),
            16,
            8,
            ColorPalette::default(),
        );
        let mut processor = TerminalProcessor::new();

        processor.process_bytes(&mut terminal, b"one");
        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07two");
        processor.process_bytes(&mut terminal, b"\x1b]133;A\x07three");
        terminal.selection = start_rendered_selection(
            &terminal.active,
            &terminal.viewport,
            terminal.on_alt_screen,
            0,
            0,
            SelectionMode::Char,
        );
        terminal.selection = extend_rendered_selection(
            &terminal.selection.clone().unwrap(),
            &terminal.active,
            &terminal.viewport,
            terminal.on_alt_screen,
            2,
            2,
        );

        let snap = snapshot_terminal(&mut terminal);

        assert_eq!(snapshot_row_text(&snap.rows[0]), "one  ");
        assert_eq!(&snap.rows[0].selected[..3], &[true, true, true]);
        assert_eq!(snapshot_row_text(&snap.rows[2]), "two  ");
        assert_eq!(&snap.rows[2].selected[..3], &[true, true, true]);
    }

    #[test]
    fn indicator_status_snapshots_status_row() {
        let mut terminal = Terminal::new(
            20,
            3,
            10,
            StatusLineMode::Off,
            FeaturePermissions::default(),
            TerminalLimits::default(),
            16,
            8,
            ColorPalette::default(),
        );
        crate::settings::set_default_status_display(
            &mut terminal.active,
            &mut terminal.stash,
            &mut terminal.viewport,
            &terminal.palette,
            &mut terminal.default_status_display,
            StatusDisplayKind::Indicator,
        );
        let snap = snapshot_terminal(&mut terminal);

        assert_eq!(snap.rows.len(), snap.total_rows as usize);
        assert_eq!(snap.rows.last().unwrap().screen_row, terminal.viewport.rows);
    }

    fn snapshot_row_text(row: &RowSnapshot) -> String {
        row.cells.concat()
    }
}

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use palette::Srgb;
use smol_str::SmolStrBuilder;
use unicode_segmentation::UnicodeSegmentation;
use utils41::blend_colors;

use crate::ColorPalette;
use crate::LineAttr;
use crate::StatusDisplayKind;
use crate::Terminal;
use crate::selection::is_cell_active_match;
use crate::selection::is_cell_match;
use crate::selection::is_cell_selected;
use crate::selection::search_active;
use crate::selection::search_state;
use crate::view;

/// Per-row snapshot of terminal state. Dirty snapshots include only rows
/// whose visible row index is named by [`screen_row`].
#[derive(Debug, Clone)]
pub struct RowSnapshot {
    /// Row index in the rendered terminal surface. Visible terminal rows start
    /// at 0; a visible status line uses `viewport_rows`.
    pub screen_row: u32,
    pub cells: Vec<smol_str::SmolStr>,
    pub attrs: Vec<CellAttrs>,
    pub fg: Vec<Srgb<u8>>,
    pub bg: Vec<Srgb<u8>>,
    pub underline: Vec<UnderlineStyle>,
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
}

/// Snapshot of the search bar state for rendering.
#[derive(Debug, Clone)]
pub struct SearchSnapshot {
    pub query: String,
    pub match_count: usize,
    pub active_idx: usize,
}

/// All terminal state needed for one render frame, captured under the lock.
///
/// `rows` contains row updates rather than the entire viewport. Consumers keep
/// their previous row cache and replace entries by `RowSnapshot::screen_row`.
#[derive(Debug, Clone)]
pub struct TermSnapshot {
    pub rows: Vec<RowSnapshot>,
    pub total_rows: u32,
    pub viewport_rows: u32,
    pub viewport_cols: u32,
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
    /// True when the consumer should discard any cached rows before applying
    /// this snapshot.
    pub reset_cached_rows: bool,
}

/// Dirty-row state for terminal snapshots.
///
/// Keep row dirtiness in this single sidecar vector rather than on `Row`
/// itself.
#[derive(Debug, Default)]
pub(crate) struct SnapshotState {
    dirty_rows: Vec<bool>,
    all_dirty: bool,
    shape: Option<SnapshotShape>,
}

impl SnapshotState {
    pub(crate) fn mark_row(
        &mut self,
        row: u32,
    ) {
        let idx = row as usize;
        if idx >= self.dirty_rows.len() {
            self.dirty_rows.resize(idx + 1, false);
        }
        self.dirty_rows[idx] = true;
    }

    pub(crate) fn mark_rows(
        &mut self,
        start: u32,
        end: u32,
    ) {
        for row in start.min(end)..=start.max(end) {
            self.mark_row(row);
        }
    }

    pub(crate) fn mark_all(&mut self) {
        self.all_dirty = true;
        for dirty in &mut self.dirty_rows {
            *dirty = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotShape {
    total_rows: u32,
    viewport_rows: u32,
    viewport_cols: u32,
    status_line_row: Option<u32>,
}

/// Snapshot the terminal's visible state under the lock.
pub fn snapshot_terminal(terminal: &mut Terminal) -> TermSnapshot {
    let vp_rows = terminal.viewport.rows;
    let vp_cols = terminal.viewport.cols;
    let search_active = search_active(&terminal.search);
    let status_line_row = view::status_line_row(&terminal.active).map(|_| vp_rows);
    let total_rows = vp_rows + u32::from(status_line_row.is_some());
    let shape = SnapshotShape {
        total_rows,
        viewport_rows: vp_rows,
        viewport_cols: vp_cols,
        status_line_row,
    };
    let reset_cached_rows = terminal.snapshot.shape != Some(shape);
    if reset_cached_rows {
        terminal.snapshot.dirty_rows = vec![true; total_rows as usize];
        terminal.snapshot.shape = Some(shape);
    } else {
        ensure_snapshot_len(&mut terminal.snapshot, total_rows as usize);
        if terminal.snapshot.all_dirty {
            terminal.snapshot.dirty_rows.fill(true);
        }
    }
    terminal.snapshot.all_dirty = false;

    let mut rows = Vec::new();
    let dirty_visible_rows: Vec<_> = (0..vp_rows)
        .filter(|&row| take_dirty_row(&mut terminal.snapshot, row))
        .collect();
    let dirty_status_row =
        status_line_row.filter(|&row| take_dirty_row(&mut terminal.snapshot, row));
    for row in dirty_visible_rows {
        rows.push(snapshot_visible_row(terminal, row));
    }
    if dirty_status_row.is_some()
        && let Some(status_row) = snapshot_status_line_row(terminal, vp_cols)
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
            Some((vp_rows, col))
        } else {
            Some((terminal.active.cursor.row, terminal.active.cursor.col))
        }
    } else {
        None
    };

    TermSnapshot {
        rows,
        total_rows,
        viewport_rows: vp_rows,
        viewport_cols: vp_cols,
        status_line_row,
        drcs_glyphs: terminal.drcs_render_glyphs(),
        dec_color: terminal.dec_color_state().clone(),
        palette: terminal.palette.clone(),
        search_active,
        search,
        cursor,
        cursor_style: terminal.cursor_style,
        screen_reverse: terminal.modes.screen_reverse,
        reset_cached_rows,
    }
}

fn ensure_snapshot_len(
    snapshot: &mut SnapshotState,
    len: usize,
) {
    if len > snapshot.dirty_rows.len() {
        snapshot.dirty_rows.resize(len, snapshot.all_dirty);
    }
}

fn take_dirty_row(
    snapshot: &mut SnapshotState,
    screen_row: u32,
) -> bool {
    let idx = screen_row as usize;
    ensure_snapshot_len(snapshot, idx + 1);
    let dirty = snapshot.dirty_rows[idx];
    snapshot.dirty_rows[idx] = false;
    dirty
}

fn snapshot_visible_row(
    terminal: &Terminal,
    row: u32,
) -> RowSnapshot {
    let grid_row = view::visible_row(&terminal.active, &terminal.viewport, row);
    let is_double = !matches!(grid_row.line_attr, LineAttr::Normal);
    let cols = if is_double {
        terminal.viewport.cols / 2
    } else {
        terminal.viewport.cols
    };

    RowSnapshot {
        screen_row: row,
        cells: grid_row.cells.clone(),
        attrs: grid_row.attrs.clone(),
        fg: grid_row.fg.clone(),
        bg: grid_row.bg.clone(),
        underline: grid_row.underline.clone(),
        underline_color: grid_row.underline_color.clone(),
        has_link: grid_row.links.iter().map(|l| l.is_some()).collect(),
        line_attr: grid_row.line_attr,
        selected: (0..cols)
            .map(|c| {
                is_cell_selected(
                    terminal.selection.as_ref(),
                    &terminal.active,
                    &terminal.viewport,
                    row,
                    c,
                )
            })
            .collect(),
        matched: (0..cols)
            .map(|c| {
                is_cell_match(
                    &terminal.search,
                    &terminal.active,
                    &terminal.viewport,
                    row,
                    c,
                )
            })
            .collect(),
        active_match: (0..cols)
            .map(|c| {
                is_cell_active_match(
                    &terminal.search,
                    &terminal.active,
                    &terminal.viewport,
                    row,
                    c,
                )
            })
            .collect(),
        prompt_start: grid_row.prompt_start,
        exit_status: grid_row.exit_status,
    }
}

fn snapshot_status_line_row(
    terminal: &Terminal,
    vp_cols: u32,
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
        ));
    }
    let grid_row = view::status_line_row(&terminal.active)?;
    Some(RowSnapshot {
        screen_row: terminal.viewport.rows,
        cells: grid_row.cells.clone(),
        attrs: grid_row.attrs.clone(),
        fg: grid_row.fg.clone(),
        bg: grid_row.bg.clone(),
        underline: grid_row.underline.clone(),
        underline_color: grid_row.underline_color.clone(),
        has_link: grid_row.links.iter().map(|l| l.is_some()).collect(),
        line_attr: grid_row.line_attr,
        selected: vec![false; vp_cols as usize],
        matched: vec![false; vp_cols as usize],
        active_match: vec![false; vp_cols as usize],
        prompt_start: false,
        exit_status: None,
    })
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
) -> RowSnapshot {
    let mut row = blank_status_line_row(cols as usize, palette, screen_row);
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
        line_attr: LineAttr::Normal,
        fg: vec![palette.status_line_fg; cols],
        bg: vec![palette.status_line_bg; cols],
        attrs: vec![CellAttrs::default(); cols],
        selected: vec![false; cols],
        matched: vec![false; cols],
        active_match: vec![false; cols],
        cells: vec![smol_str::SmolStr::new_inline(" "); cols],
        exit_status: None,
        has_link: vec![false; cols],
        underline: vec![UnderlineStyle::None; cols],
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
    use super::*;
    use crate::FeaturePermissions;
    use crate::TerminalProcessor;

    fn terminal() -> Terminal {
        Terminal::new(
            5,
            3,
            10,
            StatusDisplayKind::None,
            FeaturePermissions::default(),
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
        assert_eq!(snap.rows.len(), snap.total_rows as usize);
        assert_eq!(
            snap.rows
                .iter()
                .map(|row| row.screen_row)
                .collect::<Vec<_>>(),
            (0..snap.total_rows).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unchanged_snapshot_contains_no_row_updates() {
        let mut terminal = terminal();
        let _ = snapshot_terminal(&mut terminal);

        let snap = snapshot_terminal(&mut terminal);

        assert!(!snap.reset_cached_rows);
        assert!(snap.rows.is_empty());
    }

    #[test]
    fn text_write_snapshots_only_dirty_cursor_row() {
        let mut terminal = terminal();
        let _ = snapshot_terminal(&mut terminal);

        TerminalProcessor::new().process_bytes(&mut terminal, b"A");
        let snap = snapshot_terminal(&mut terminal);

        assert!(!snap.reset_cached_rows);
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].screen_row, 0);
        assert_eq!(snap.rows[0].cells[0].as_str(), "A");
    }
}

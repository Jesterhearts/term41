use super::*;

pub(crate) fn active_viewport(
    screen: &Screen,
    viewport: &Viewport,
) -> Viewport {
    let mut view = screen::screen_viewport(screen, viewport);
    if screen.offset > 0 {
        view.top = view
            .top_index(screen.grid.rows.len())
            .saturating_sub(screen.offset as usize);
    }
    view
}

pub(crate) fn screen_row_to_absolute(
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> u64 {
    let base = active_viewport(screen, viewport).top_index(screen.grid.rows.len());
    (screen.grid.total_popped + base + screen_row as usize) as u64
}

pub(crate) fn absolute_row_to_local(
    screen: &Screen,
    abs: u64,
) -> Option<usize> {
    let popped = screen.grid.total_popped as u64;
    if abs < popped {
        return None;
    }
    let local = (abs - popped) as usize;
    if local >= screen.grid.rows.len() {
        return None;
    }
    Some(local)
}

pub(crate) fn start_selection(
    screen: &Screen,
    viewport: &Viewport,
    col: u32,
    screen_row: u32,
    mode: SelectionMode,
) -> Option<Selection> {
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    let local = absolute_row_to_local(screen, abs_row)?;
    let row = &screen.grid.rows[local];
    let origin = SelectionPoint { row: abs_row, col };

    let (anchor, head) = match mode {
        SelectionMode::Char => (origin, origin),
        SelectionMode::Word => {
            let (s, e) = expand_to_word(row, col);
            (
                SelectionPoint {
                    row: abs_row,
                    col: s,
                },
                SelectionPoint {
                    row: abs_row,
                    col: e,
                },
            )
        }
        SelectionMode::Line => {
            let (s, e) = expand_to_line(row);
            (
                SelectionPoint {
                    row: abs_row,
                    col: s,
                },
                SelectionPoint {
                    row: abs_row,
                    col: e,
                },
            )
        }
    };

    Some(Selection {
        anchor,
        head,
        mode,
        origin,
    })
}

pub(crate) fn extend_selection(
    selection: &Selection,
    screen: &Screen,
    viewport: &Viewport,
    col: u32,
    screen_row: u32,
) -> Option<Selection> {
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    let local = absolute_row_to_local(screen, abs_row)?;
    let origin_local = absolute_row_to_local(screen, selection.origin.row)?;

    let head_row = &screen.grid.rows[local];
    let origin_row = &screen.grid.rows[origin_local];

    let new_point = SelectionPoint { row: abs_row, col };
    let forward = (new_point.row, new_point.col) >= (selection.origin.row, selection.origin.col);

    let (anchor, head) = match selection.mode {
        SelectionMode::Char => (selection.origin, new_point),
        SelectionMode::Word => {
            let (o_start, o_end) = expand_to_word(origin_row, selection.origin.col);
            let (h_start, h_end) = expand_to_word(head_row, col);
            if forward {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_start,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_end,
                    },
                )
            } else {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_end,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_start,
                    },
                )
            }
        }
        SelectionMode::Line => {
            let (o_start, o_end) = expand_to_line(origin_row);
            let (h_start, h_end) = expand_to_line(head_row);
            if forward {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_start,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_end,
                    },
                )
            } else {
                (
                    SelectionPoint {
                        row: selection.origin.row,
                        col: o_end,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: h_start,
                    },
                )
            }
        }
    };

    Some(Selection {
        anchor,
        head,
        mode: selection.mode,
        origin: selection.origin,
    })
}

pub(crate) fn is_cell_selected(
    selection: Option<&Selection>,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    screen_col: u32,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    if selection.is_empty() {
        return false;
    }
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    selection.contains(SelectionPoint {
        row: abs_row,
        col: screen_col,
    })
}

pub(crate) fn close_search(
    search: &mut SearchState,
    selection: &mut Option<Selection>,
) {
    if let Some(&active) = search.matches.get(search.active_idx) {
        let anchor = SelectionPoint {
            row: active.row,
            col: active.start_col,
        };
        let head = SelectionPoint {
            row: active.row,
            col: active.end_col,
        };
        *selection = Some(Selection {
            anchor,
            head,
            mode: SelectionMode::Char,
            origin: anchor,
        });
    }
    search.active = false;
    search.query.clear();
    search.matches.clear();
    search.active_idx = 0;
}

pub(crate) fn search_state(search: &SearchState) -> Option<&SearchState> {
    if search.active { Some(search) } else { None }
}

pub(crate) fn search_append(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
    offset: u32,
    s: &str,
) -> u32 {
    if !search.active {
        return offset;
    }
    search.query.push_str(s);
    refresh_search(search, screen, viewport, offset)
}

pub(crate) fn search_backspace(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
    offset: u32,
) -> u32 {
    if !search.active {
        return offset;
    }
    search.query.pop();
    refresh_search(search, screen, viewport, offset)
}

pub(crate) fn search_step_next(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
    offset: u32,
) -> u32 {
    if !search.active || search.matches.is_empty() {
        return offset;
    }
    search.active_idx = (search.active_idx + 1) % search.matches.len();
    scroll_to_active_match(search, screen, viewport, offset)
}

pub(crate) fn search_step_prev(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
    offset: u32,
) -> u32 {
    if !search.active || search.matches.is_empty() {
        return offset;
    }
    let n = search.matches.len();
    search.active_idx = (search.active_idx + n - 1) % n;
    scroll_to_active_match(search, screen, viewport, offset)
}

pub(crate) fn is_cell_match(
    search: &SearchState,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    screen_col: u32,
) -> bool {
    if !search.active || search.matches.is_empty() {
        return false;
    }
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    search
        .matches
        .iter()
        .any(|m| m.contains(abs_row, screen_col))
}

pub(crate) fn is_cell_active_match(
    search: &SearchState,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    screen_col: u32,
) -> bool {
    if !search.active {
        return false;
    }
    let Some(active) = search.matches.get(search.active_idx) else {
        return false;
    };
    let abs_row = screen_row_to_absolute(screen, viewport, screen_row);
    active.contains(abs_row, screen_col)
}

fn refresh_search(
    search: &mut SearchState,
    screen: &Screen,
    viewport: &Viewport,
    offset: u32,
) -> u32 {
    recompute_matches(search, screen);
    if search.matches.is_empty() {
        search.active_idx = 0;
        return offset;
    }
    let viewport_top = screen_row_to_absolute(screen, viewport, 0);
    search.active_idx = search
        .matches
        .iter()
        .position(|m| m.row >= viewport_top)
        .unwrap_or(0);
    scroll_to_active_match(search, screen, viewport, offset)
}

fn recompute_matches(
    search: &mut SearchState,
    screen: &Screen,
) {
    search.matches.clear();
    if search.query.is_empty() {
        return;
    }
    let q = search.query.as_str();
    let popped = screen.grid.total_popped as u64;
    let mut text = String::new();
    let mut cell_byte_starts: Vec<usize> = Vec::new();
    for (local, row) in screen.grid.rows.iter().enumerate() {
        text.clear();
        cell_byte_starts.clear();
        cell_byte_starts.reserve(row.cells.len());
        for cell in &row.cells {
            cell_byte_starts.push(text.len());
            text.push_str(cell);
        }
        if text.len() < q.len() {
            continue;
        }
        let abs_row = popped + local as u64;
        for (byte, _) in text.match_indices(q) {
            let start_col = cell_byte_starts
                .partition_point(|&s| s <= byte)
                .saturating_sub(1) as u32;
            let end_byte = byte + q.len();
            let end_col = cell_byte_starts
                .partition_point(|&s| s < end_byte)
                .saturating_sub(1) as u32;
            search.matches.push(MatchSpan {
                row: abs_row,
                start_col,
                end_col,
            });
        }
    }
}

fn scroll_to_active_match(
    search: &SearchState,
    screen: &Screen,
    viewport: &Viewport,
    offset: u32,
) -> u32 {
    let Some(m) = search.matches.get(search.active_idx).copied() else {
        return offset;
    };
    let popped = screen.grid.total_popped as u64;
    let Some(local) = m.row.checked_sub(popped) else {
        return offset;
    };
    let local = local as usize;
    let grid_len = screen.grid.rows.len();
    let rows = viewport.rows as usize;
    if local >= grid_len {
        return offset;
    }
    if grid_len <= rows {
        return 0;
    }
    let ideal_top = local.saturating_sub(rows / 2);
    let max_top = grid_len - rows;
    let top = ideal_top.min(max_top);
    let next_offset = (grid_len - rows - top) as u32;
    let max_offset = screen.grid.scrollback_len(viewport);
    next_offset.min(max_offset)
}

pub(crate) fn selection_text(
    selection: Option<&Selection>,
    screen: &Screen,
) -> Option<String> {
    let selection = selection?;
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.ordered();
    let popped = screen.grid.total_popped as u64;
    let last_idx = screen.grid.rows.len().saturating_sub(1);

    let mut out = String::new();
    for abs_row in start.row..=end.row {
        let local = abs_row.checked_sub(popped)? as usize;
        if local > last_idx {
            break;
        }
        let row = &screen.grid.rows[local];
        let row_len_cols = row.cells.len() as u32;
        if row_len_cols == 0 {
            if abs_row < end.row && !row.wrapped {
                out.push('\n');
            }
            continue;
        }

        let (col_start, col_end, trim) = match selection.mode {
            SelectionMode::Line => (0, row_len_cols - 1, true),
            _ => {
                let is_first = abs_row == start.row;
                let is_last = abs_row == end.row;
                let cs = if is_first { start.col } else { 0 };
                let ce = if is_last { end.col } else { row_len_cols - 1 };
                let trim = !is_last;
                (cs, ce, trim)
            }
        };
        let col_end = col_end.min(row_len_cols - 1);
        if col_start > col_end {
            if abs_row < end.row && !row.wrapped {
                out.push('\n');
            }
            continue;
        }

        let mut segment = String::new();
        for cell in &row.cells[col_start as usize..=col_end as usize] {
            segment.push_str(cell);
        }
        if trim {
            out.push_str(segment.trim_end_matches(' '));
        } else {
            out.push_str(&segment);
        }

        if abs_row < end.row && !row.wrapped {
            out.push('\n');
        }
    }

    Some(out)
}

pub(crate) fn copy_selection(
    clipboard: &mut Clipboard,
    selection: Option<&Selection>,
    screen: &Screen,
    kind: ClipboardKind,
) {
    if let Some(text) = selection_text(selection, screen) {
        clipboard.set(kind, &text);
    }
}

pub(crate) fn paste(
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    bracketed_paste: bool,
    text: &str,
) {
    const PASTE_END: &str = "\x1b[201~";
    if bracketed_paste {
        conformance::write_csi(pending_output, c1_mode, format_args!("200~"));
        for chunk in text.split(PASTE_END) {
            pending_output.extend_from_slice(chunk.as_bytes());
        }
        conformance::write_csi(pending_output, c1_mode, format_args!("201~"));
    } else {
        for chunk in text.split(PASTE_END) {
            pending_output.extend_from_slice(chunk.as_bytes());
        }
    }
}

pub(crate) fn paste_from_clipboard(
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    bracketed_paste: bool,
    kind: ClipboardKind,
) {
    if let Some(text) = clipboard.get(kind)
        && !text.is_empty()
    {
        paste(pending_output, c1_mode, bracketed_paste, &text);
    }
}

pub(crate) fn copy_to_clipboard(
    clipboard: &mut Clipboard,
    text: &str,
) {
    clipboard.set(ClipboardKind::Clipboard, text);
}

impl Terminal {
    pub fn start_selection(
        &mut self,
        col: u32,
        screen_row: u32,
        mode: SelectionMode,
    ) {
        self.selection = start_selection(&self.active, &self.viewport, col, screen_row, mode);
    }

    pub fn extend_selection(
        &mut self,
        col: u32,
        screen_row: u32,
    ) {
        let Some(selection) = self.selection.as_ref() else {
            return;
        };
        if let Some(next) =
            extend_selection(selection, &self.active, &self.viewport, col, screen_row)
        {
            self.selection = Some(next);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn is_cell_selected(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> bool {
        is_cell_selected(
            self.selection.as_ref(),
            &self.active,
            &self.viewport,
            screen_row,
            screen_col,
        )
    }

    pub fn open_search(&mut self) {
        self.search.active = true;
        self.search.query.clear();
        self.search.matches.clear();
        self.search.active_idx = 0;
    }

    pub fn close_search(&mut self) {
        close_search(&mut self.search, &mut self.selection);
    }

    pub fn search_active(&self) -> bool {
        self.search.active
    }

    pub fn search_state(&self) -> Option<&SearchState> {
        search_state(&self.search)
    }

    pub fn search_append(
        &mut self,
        s: &str,
    ) {
        let next_offset = search_append(
            &mut self.search,
            &self.active,
            &self.viewport,
            self.active.offset,
            s,
        );
        self.active.offset = next_offset;
    }

    pub fn search_backspace(&mut self) {
        let next_offset = search_backspace(
            &mut self.search,
            &self.active,
            &self.viewport,
            self.active.offset,
        );
        self.active.offset = next_offset;
    }

    pub fn search_next(&mut self) {
        let next_offset = search_step_next(
            &mut self.search,
            &self.active,
            &self.viewport,
            self.active.offset,
        );
        self.active.offset = next_offset;
    }

    pub fn search_prev(&mut self) {
        let next_offset = search_step_prev(
            &mut self.search,
            &self.active,
            &self.viewport,
            self.active.offset,
        );
        self.active.offset = next_offset;
    }

    pub fn is_cell_match(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> bool {
        is_cell_match(
            &self.search,
            &self.active,
            &self.viewport,
            screen_row,
            screen_col,
        )
    }

    pub fn is_cell_active_match(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> bool {
        is_cell_active_match(
            &self.search,
            &self.active,
            &self.viewport,
            screen_row,
            screen_col,
        )
    }

    pub fn selection_text(&self) -> Option<String> {
        selection_text(self.selection.as_ref(), &self.active)
    }

    pub fn copy_selection(
        &mut self,
        kind: ClipboardKind,
    ) {
        copy_selection(
            &mut self.clipboard,
            self.selection.as_ref(),
            &self.active,
            kind,
        );
    }

    pub fn paste(
        &mut self,
        text: &str,
    ) {
        paste(
            &mut self.pending_output,
            self.modes.c1_mode,
            self.modes.bracketed_paste,
            text,
        );
    }

    pub fn paste_from_clipboard(
        &mut self,
        kind: ClipboardKind,
    ) {
        paste_from_clipboard(
            &mut self.clipboard,
            &mut self.pending_output,
            self.modes.c1_mode,
            self.modes.bracketed_paste,
            kind,
        );
    }

    pub fn copy_to_clipboard(
        &mut self,
        text: &str,
    ) {
        copy_to_clipboard(&mut self.clipboard, text);
    }
}

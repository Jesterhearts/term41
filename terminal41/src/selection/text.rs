use clip41::Clipboard;
use clip41::ClipboardKind;

use super::model::Selection;
use super::model::SelectionMode;
use super::model::SelectionPoint;
use super::rendered::rendered_row_ref;
use crate::Screen;

/// Extract selected text from the screen.
pub fn selection_text(
    selection: Option<&Selection>,
    screen: &Screen,
) -> Option<String> {
    let selection = selection?;
    if selection.is_empty() {
        return None;
    }
    let (start, end) = selection.ordered();
    if selection.rendered {
        return rendered_selection_text(selection, screen, start, end);
    }
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

fn rendered_selection_text(
    selection: &Selection,
    screen: &Screen,
    start: SelectionPoint,
    end: SelectionPoint,
) -> Option<String> {
    let mut out = String::new();
    for abs_row in start.row..=end.row {
        let Some(row) = rendered_row_ref(screen, abs_row) else {
            if abs_row < end.row {
                out.push('\n');
            }
            continue;
        };
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
        if col_start <= col_end {
            let mut segment = String::new();
            for cell in &row.cells[col_start as usize..=col_end as usize] {
                segment.push_str(cell);
            }
            if trim {
                out.push_str(segment.trim_end_matches(' '));
            } else {
                out.push_str(&segment);
            }
        }
        if abs_row < end.row && !row.wrapped {
            out.push('\n');
        }
    }
    Some(out)
}

/// Copy selected text into the requested clipboard selection.
pub fn copy_selection(
    clipboard: &mut Clipboard,
    selection: Option<&Selection>,
    screen: &Screen,
    kind: ClipboardKind,
) {
    if let Some(text) = selection_text(selection, screen) {
        clipboard.set(kind, &text);
    }
}

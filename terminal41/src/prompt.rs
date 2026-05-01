//! Helpers for shell-integration prompt metadata.

use std::collections::HashMap;
use std::time::Duration;

use crate::CommandMeta;
use crate::Screen;
use crate::Viewport;
use crate::selection;
use crate::selection::Selection;
use crate::selection::SelectionMode;
use crate::selection::SelectionPoint;

pub(crate) fn format_indicator_status(
    current_directory: Option<&std::path::Path>,
    current_prompt_row: Option<u64>,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> String {
    let mut segments = path_segments(current_directory);
    if let Some(command) = running_command_text(current_prompt_row, command_metas, screen) {
        segments.push(command);
    }
    segments.join(" > ")
}

fn path_segments(path: Option<&std::path::Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    path.components()
        .map(|component| match component {
            std::path::Component::RootDir => "/".to_owned(),
            std::path::Component::Prefix(prefix) => prefix.as_os_str().to_string_lossy().into(),
            std::path::Component::CurDir => ".".to_owned(),
            std::path::Component::ParentDir => "..".to_owned(),
            std::path::Component::Normal(part) => part.to_string_lossy().into_owned(),
        })
        .collect()
}

fn running_command_text(
    current_prompt_row: Option<u64>,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let prompt_abs = current_prompt_row?;
    let meta = command_metas.get(&prompt_abs)?;
    let command_running = meta.started_at.is_some() && meta.finished_at.is_none();
    if !command_running {
        return None;
    }
    display_command_text_at(prompt_abs, command_metas, screen)
}

fn display_command_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    if let Some(command) = untrusted_command_line_at(prompt_abs, command_metas) {
        return flatten_status_command_text(command);
    }
    let command = command_text_at(prompt_abs, command_metas, screen)?;
    flatten_status_command_text(&command)
}

fn flatten_status_command_text(command: &str) -> Option<String> {
    let sanitized: String = command
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let flattened = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    (!flattened.is_empty()).then_some(flattened)
}

/// Find the nearest prompt marker at or above a viewport row.
pub fn find_prompt_for_screen_row(
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> Option<u64> {
    let base = selection::active_viewport(screen, viewport).top_index(screen.grid.rows.len());
    let start = base + screen_row as usize;
    let popped = screen.grid.total_popped as u64;
    for i in (0..=start).rev() {
        if screen.grid.rows[i].prompt_start {
            return Some(popped + i as u64);
        }
    }
    None
}

fn find_next_prompt_after(
    screen: &Screen,
    after_abs: u64,
) -> Option<u64> {
    let popped = screen.grid.total_popped as u64;
    let start = after_abs.checked_sub(popped)? as usize + 1;
    for i in start..screen.grid.rows.len() {
        if screen.grid.rows[i].prompt_start {
            return Some(popped + i as u64);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextPoint {
    row: u64,
    col: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextRange {
    start: TextPoint,
    end: TextPoint,
}

/// Return the absolute row where the command block ends.
pub fn command_end_abs(
    prompt_abs: u64,
    screen: &Screen,
) -> u64 {
    if let Some(next) = find_next_prompt_after(screen, prompt_abs) {
        next.saturating_sub(1)
    } else {
        (screen.grid.total_popped + screen.grid.rows.len() - 1) as u64
    }
}

fn text_point_from_row_start(row: u64) -> TextPoint {
    TextPoint { row, col: 0 }
}

fn text_point_from_row_end(
    screen: &Screen,
    row_abs: u64,
) -> TextPoint {
    TextPoint {
        row: row_abs,
        col: selection::absolute_row_to_local(screen, row_abs)
            .map(|local| screen.grid.rows[local].cells.len() as u32)
            .unwrap_or(0),
    }
}

fn command_start_point(
    prompt_abs: u64,
    meta: Option<&CommandMeta>,
) -> TextPoint {
    TextPoint {
        row: meta.and_then(|m| m.command_row).unwrap_or(prompt_abs),
        col: meta.and_then(|m| m.command_col).unwrap_or(0),
    }
}

fn output_start_point(meta: Option<&CommandMeta>) -> Option<TextPoint> {
    let meta = meta?;
    Some(TextPoint {
        row: meta.output_row?,
        col: meta.output_col.unwrap_or(0),
    })
}

fn command_finished_point(meta: Option<&CommandMeta>) -> Option<TextPoint> {
    let meta = meta?;
    Some(TextPoint {
        row: meta.finished_row?,
        col: meta.finished_col.unwrap_or(0),
    })
}

fn command_block_end_point(
    prompt_abs: u64,
    meta: Option<&CommandMeta>,
    screen: &Screen,
) -> TextPoint {
    if let Some(finished) = command_finished_point(meta) {
        return finished;
    }
    if let Some(next_prompt) = find_next_prompt_after(screen, prompt_abs) {
        return text_point_from_row_start(next_prompt);
    }
    text_point_from_row_end(screen, command_end_abs(prompt_abs, screen))
}

fn command_text_range(
    prompt_abs: u64,
    meta: Option<&CommandMeta>,
    screen: &Screen,
) -> Option<TextRange> {
    let start = command_start_point(prompt_abs, meta);
    let end = if let Some(output) = output_start_point(meta) {
        output
    } else if let Some(finished) = command_finished_point(meta) {
        finished
    } else if let Some(next_prompt) = find_next_prompt_after(screen, prompt_abs) {
        text_point_from_row_start(next_prompt)
    } else {
        let end_row = prompt_abs.max(start.row);
        text_point_from_row_end(screen, end_row)
    };
    text_range(start, end)
}

fn command_and_output_text_range(
    prompt_abs: u64,
    meta: Option<&CommandMeta>,
    screen: &Screen,
) -> Option<TextRange> {
    let start = command_start_point(prompt_abs, meta);
    let end = command_block_end_point(prompt_abs, meta, screen);
    text_range(start, end)
}

fn output_text_range(
    prompt_abs: u64,
    meta: Option<&CommandMeta>,
    screen: &Screen,
) -> Option<TextRange> {
    let start = output_start_point(meta)?;
    let end = command_block_end_point(prompt_abs, meta, screen);
    text_range(start, end)
}

fn text_range(
    start: TextPoint,
    end: TextPoint,
) -> Option<TextRange> {
    (start.row < end.row || (start.row == end.row && start.col < end.col))
        .then_some(TextRange { start, end })
}

fn last_row_in_range(range: TextRange) -> Option<u64> {
    if range.end.col == 0 {
        range.end.row.checked_sub(1)
    } else {
        Some(range.end.row)
    }
    .filter(|&last| range.start.row <= last)
}

fn extract_range_text(
    screen: &Screen,
    range: TextRange,
) -> String {
    let Some(end_abs) = last_row_in_range(range) else {
        return String::new();
    };
    let popped = screen.grid.total_popped as u64;
    let mut out = String::new();
    for abs in range.start.row..=end_abs {
        let Some(local) = abs.checked_sub(popped).map(|l| l as usize) else {
            continue;
        };
        if local >= screen.grid.rows.len() {
            break;
        }
        let row = &screen.grid.rows[local];
        let cs = if abs == range.start.row {
            range.start.col as usize
        } else {
            0
        };
        let ce = if abs == range.end.row {
            range.end.col as usize
        } else {
            row.cells.len()
        }
        .min(row.cells.len());
        if cs >= ce {
            if abs < end_abs && !row.wrapped {
                out.push('\n');
            }
            continue;
        }
        let mut seg = String::new();
        for cell in &row.cells[cs..ce] {
            seg.push_str(cell);
        }
        out.push_str(seg.trim_end_matches(' '));
        if abs < end_abs && !row.wrapped {
            out.push('\n');
        }
    }
    out
}

/// Extract the command text associated with a prompt row.
pub fn command_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let meta = command_metas.get(&prompt_abs);
    let text = extract_range_text(screen, command_text_range(prompt_abs, meta, screen)?);
    if text.is_empty() { None } else { Some(text) }
}

/// Return the OSC 633 `E` command-line metadata associated with a prompt row.
pub fn untrusted_command_line_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
) -> Option<&str> {
    command_metas
        .get(&prompt_abs)?
        .untrusted_command_line
        .as_deref()
        .filter(|text| !text.is_empty())
}

/// Extract command output associated with a prompt row.
pub fn output_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let meta = command_metas.get(&prompt_abs);
    let text = extract_range_text(screen, output_text_range(prompt_abs, meta, screen)?);
    if text.is_empty() { None } else { Some(text) }
}

/// Extract command text plus output associated with a prompt row.
pub fn command_and_output_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let meta = command_metas.get(&prompt_abs);
    let text = extract_range_text(
        screen,
        command_and_output_text_range(prompt_abs, meta, screen)?,
    );
    if text.is_empty() { None } else { Some(text) }
}

/// Return the recorded runtime for a completed command.
pub fn command_duration_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
) -> Option<Duration> {
    let meta = command_metas.get(&prompt_abs)?;
    let start = meta.started_at?;
    let end = meta.finished_at?;
    Some(end.duration_since(start))
}

/// Replace the current selection with the command text at a prompt row.
pub fn select_command_at(
    selection: &mut Option<Selection>,
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) {
    let meta = command_metas.get(&prompt_abs);
    let Some(range) = command_text_range(prompt_abs, meta, screen) else {
        return;
    };
    let text = extract_range_text(screen, range);
    if text.trim().is_empty() {
        return;
    }
    let anchor = SelectionPoint {
        row: range.start.row,
        col: range.start.col,
    };
    let Some(head) = selection_head_for_range(screen, range) else {
        return;
    };
    let head = SelectionPoint {
        row: head.row,
        col: head.col,
    };
    *selection = Some(Selection {
        anchor,
        head,
        mode: SelectionMode::Char,
        origin: anchor,
    });
}

fn selection_head_for_range(
    screen: &Screen,
    range: TextRange,
) -> Option<TextPoint> {
    let end_abs = last_row_in_range(range)?;
    if range.end.col > 0 && range.end.row == end_abs {
        let col = selection::absolute_row_to_local(screen, end_abs)
            .map(|local| range.end.col.min(screen.grid.rows[local].len()))
            .unwrap_or(range.end.col);
        if col == 0 {
            return None;
        }
        return Some(TextPoint {
            row: end_abs,
            col: col - 1,
        });
    }
    let end_col = selection::absolute_row_to_local(screen, end_abs)
        .map(|local| screen.grid.rows[local].content_len().saturating_sub(1))
        .unwrap_or(0);
    Some(TextPoint {
        row: end_abs,
        col: end_col,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::command_and_output_text_at;
    use super::command_text_at;
    use super::output_text_at;
    use super::select_command_at;
    use crate::test_support::TestTerm;
    use crate::view;

    fn emit_prompt(
        term: &mut TestTerm,
        label: &str,
        output_lines: u32,
        exit: i32,
    ) {
        term.process(b"\x1b]133;A\x1b\\");
        term.process(label.as_bytes());
        term.process(b"\x1b]133;B\x1b\\");
        term.process(b"\n\x1b]133;C\x1b\\");
        for i in 0..output_lines {
            term.process(format!("out{i}\n").as_bytes());
        }
        term.process(format!("\x1b]133;D;{exit}\x1b\\").as_bytes());
    }

    #[test]
    fn indicator_status_formats_path_and_running_command() {
        let mut term = TestTerm::new(16, 4, 100, 16, 8);
        term.metadata.current_directory = Some(PathBuf::from("/tmp/project"));
        term.process(b"\x1b[1$~");
        term.process(b"\x1b]133;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]133;B\x07");
        term.process(b"cargo test");
        term.process(b"\x1b]133;C\x07");

        assert_eq!(
            term.indicator_status_text().as_deref(),
            Some("/ > tmp > project > cargo test")
        );
    }

    #[test]
    fn indicator_status_omits_command_when_not_running() {
        let mut term = TestTerm::new(16, 4, 100, 16, 8);
        term.metadata.current_directory = Some(PathBuf::from("/tmp/project"));
        term.process(b"\x1b[1$~");
        term.process(b"\x1b]133;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]133;B\x07");
        term.process(b"cargo test");
        term.process(b"\x1b]133;C\x07");
        term.process(b"\x1b]133;D;0\x07");

        assert_eq!(
            term.indicator_status_text().as_deref(),
            Some("/ > tmp > project")
        );
    }

    #[test]
    fn indicator_status_prefers_osc_633_command_metadata() {
        let mut term = TestTerm::new(24, 4, 100, 16, 8);
        term.metadata.current_directory = Some(PathBuf::from("/tmp/project"));
        term.process(b"\x1b[1$~");
        term.process(b"\x1b]633;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]633;B\x07");
        term.process(b"screen text");
        term.process(b"\x1b]633;E;cargo\\x20test\\x20--workspace\x07");
        term.process(b"\x1b]633;C\x07");

        assert_eq!(
            term.indicator_status_text().as_deref(),
            Some("/ > tmp > project > cargo test --workspace")
        );
    }

    #[test]
    fn command_and_output_text_use_same_line_osc_133_columns() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]133;B\x07");
        term.process(b"cargo");
        term.process(b"\x1b]133;C\x07");
        term.process(b"out");
        term.process(b"\x1b]133;D;0\x07");

        assert_eq!(
            command_text_at(0, &term.metadata.command_metas, &term.active).as_deref(),
            Some("cargo")
        );
        assert_eq!(
            output_text_at(0, &term.metadata.command_metas, &term.active).as_deref(),
            Some("out")
        );
        assert_eq!(
            command_and_output_text_at(0, &term.metadata.command_metas, &term.active).as_deref(),
            Some("cargoout")
        );
    }

    #[test]
    fn select_command_at_stops_at_same_line_output_column() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07cargo\x1b]133;C\x07out");

        let mut selection = None;
        select_command_at(
            &mut selection,
            0,
            &term.metadata.command_metas,
            &term.active,
        );

        let selection = selection.expect("command selected");
        assert_eq!(selection.anchor.row, 0);
        assert_eq!(selection.anchor.col, 2);
        assert_eq!(selection.head.row, 0);
        assert_eq!(selection.head.col, 6);
    }

    #[test]
    fn scroll_to_prev_prompt_moves_viewport() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        let before = term.active.offset;
        let viewport = term.inner.viewport;
        view::scroll_to_prev_prompt(&mut term.inner.active, &viewport);
        assert!(term.active.offset > before);
    }

    #[test]
    fn scroll_to_prev_prompt_silent_with_no_marks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"plain\noutput\nwithout\nshell integration\n");
        let before = term.active.offset;
        let viewport = term.inner.viewport;
        view::scroll_to_prev_prompt(&mut term.inner.active, &viewport);
        assert_eq!(term.active.offset, before);
    }

    #[test]
    fn scroll_to_next_prompt_walks_forward() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        term.active.offset = term.active.grid.scrollback_len(&term.viewport);
        let start = term.active.offset;
        let viewport = term.inner.viewport;
        view::scroll_to_next_prompt(&mut term.inner.active, &viewport);
        assert!(term.active.offset < start);
    }

    #[test]
    fn scroll_to_next_prompt_silent_at_last_prompt() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ only", 3, 0);
        let before = term.active.offset;
        let viewport = term.inner.viewport;
        view::scroll_to_next_prompt(&mut term.inner.active, &viewport);
        assert_eq!(term.active.offset, before);
    }
}

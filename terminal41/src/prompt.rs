//! Helpers for shell-integration prompt metadata.

use std::collections::HashMap;
use std::time::Duration;

use crate::CommandMeta;
use crate::Screen;
use crate::Viewport;
use crate::screen;
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
    if let Some(prompt) = prompt_ref_for_active_abs(screen, prompt_abs)
        && let Some(command) = command_text_for_prompt(prompt, command_metas, screen)
    {
        return flatten_status_command_text(&command);
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
    let screen_row =
        selection::active_screen_row_at_viewport_row(screen, viewport, false, screen_row)?;
    let base = selection::active_viewport(screen, viewport).top_index(screen.grid.rows.len());
    let last = screen.grid.rows.len().checked_sub(1)?;
    let start = base.saturating_add(screen_row as usize).min(last);
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

/// Prompt marker resolved from the rendered command-block stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptRef {
    /// Row in the rendered command-block document.
    pub rendered_row: u64,
    /// Absolute active-grid row when this prompt belongs to the live block.
    pub active_abs_row: Option<u64>,
}

/// Where command text in a [`CommandBlockView`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandTextSource {
    /// Text was extracted from terminal cells.
    Observed,
    /// Text came from host-provided OSC metadata and should be treated as a
    /// lower-trust fallback.
    UntrustedMetadata,
}

/// Command text associated with a prompt-backed command block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBlockCommand {
    pub text: String,
    pub source: CommandTextSource,
}

/// Coarse execution state for a prompt-backed command block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandBlockState {
    Editing,
    Running,
    Succeeded,
    Failed,
    Finished,
}

/// Read-only, derived view of a prompt-backed command block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBlockView {
    pub prompt: PromptRef,
    pub command: Option<CommandBlockCommand>,
    pub output: Option<String>,
    pub command_and_output: Option<String>,
    pub duration: Option<Duration>,
    pub exit_status: Option<i32>,
    pub state: CommandBlockState,
}

/// Read-only, derived command-block stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBlockDocument {
    pub blocks: Vec<CommandBlockView>,
}

struct RenderedRowInfo<'a> {
    row: &'a crate::Row,
    rendered_row: u64,
    block_end: u64,
    active_local: Option<usize>,
}

/// Find the nearest prompt marker at or above a viewport row in the rendered
/// command-block stream.
pub fn find_prompt_ref_for_screen_row(
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> Option<PromptRef> {
    let start =
        selection::rendered_document_row_at_viewport_row(screen, viewport, false, screen_row)?;
    for rendered_row in (0..=start).rev() {
        let Some(info) = rendered_row_info(screen, rendered_row) else {
            continue;
        };
        if info.row.prompt_start {
            return Some(prompt_ref_for_rendered_row(screen, info));
        }
    }
    None
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

fn rendered_row_info(
    screen: &Screen,
    rendered_row: u64,
) -> Option<RenderedRowInfo<'_>> {
    let mut base = 0_u64;
    for block in &screen.scrollback_blocks {
        let block_rows = screen::command_block_rendered_rows_len(block) as u64;
        if rendered_row < base + block_rows {
            let local = rendered_row - base;
            return Some(RenderedRowInfo {
                row: &block.grid.rows[local as usize],
                rendered_row,
                block_end: base + block_rows,
                active_local: None,
            });
        }
        base += block_rows;
        if rendered_row == base {
            return None;
        }
        base += 1;
    }

    let active_base = base + screen.grid.total_popped as u64;
    let local = rendered_row.checked_sub(active_base)? as usize;
    let active_rows = screen::active_block_rendered_rows_len(screen);
    if local >= active_rows {
        return None;
    }
    Some(RenderedRowInfo {
        row: &screen.grid.rows[local],
        rendered_row,
        block_end: active_base + active_rows as u64,
        active_local: Some(local),
    })
}

fn prompt_ref_for_active_abs(
    screen: &Screen,
    prompt_abs: u64,
) -> Option<PromptRef> {
    let popped = screen.grid.total_popped as u64;
    let local = prompt_abs.checked_sub(popped)? as usize;
    let row = screen.grid.rows.get(local)?;
    if !row.prompt_start {
        return None;
    }
    let rendered_row = screen
        .scrollback_blocks
        .iter()
        .map(|block| screen::command_block_rendered_rows_len(block) as u64 + 1)
        .sum::<u64>()
        + prompt_abs;
    Some(PromptRef {
        rendered_row,
        active_abs_row: Some(prompt_abs),
    })
}

fn prompt_ref_for_rendered_row(
    screen: &Screen,
    info: RenderedRowInfo<'_>,
) -> PromptRef {
    PromptRef {
        rendered_row: info.rendered_row,
        active_abs_row: info
            .active_local
            .map(|local| (screen.grid.total_popped + local) as u64),
    }
}

fn rendered_output_start_point(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<TextPoint> {
    let info = rendered_row_info(screen, prompt.rendered_row)?;
    for rendered_row in prompt.rendered_row..info.block_end {
        let row = rendered_row_info(screen, rendered_row)?.row;
        if row.output_start {
            return Some(TextPoint {
                row: rendered_row,
                col: row
                    .output_start_col
                    .unwrap_or_else(|| first_content_col(row)),
            });
        }
    }
    None
}

fn rendered_command_start_point(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<TextPoint> {
    let info = rendered_row_info(screen, prompt.rendered_row)?;
    for rendered_row in prompt.rendered_row..info.block_end {
        let row = rendered_row_info(screen, rendered_row)?.row;
        if let Some(col) = valid_command_start_col(row) {
            return Some(TextPoint {
                row: rendered_row,
                col,
            });
        }
    }
    None
}

fn first_content_col(row: &crate::Row) -> u32 {
    row.cells.iter().position(|cell| cell != " ").unwrap_or(0) as u32
}

fn valid_command_start_col(row: &crate::Row) -> Option<u32> {
    let col = row.command_start_col?;
    if col == 0 && row.prompt_start && row.content_len() > 0 {
        return None;
    }
    Some(col)
}

fn rendered_block_end_point(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<TextPoint> {
    Some(text_point_from_row_start(rendered_block_text_end(
        prompt, screen,
    )?))
}

fn rendered_block_text_end(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<u64> {
    let info = rendered_row_info(screen, prompt.rendered_row)?;
    let mut end = info.block_end;
    while end > prompt.rendered_row + 1 {
        let Some(row) = rendered_row_info(screen, end - 1).map(|info| info.row) else {
            break;
        };
        if row.content_len() > 0 {
            break;
        }
        end -= 1;
    }
    Some(end)
}

fn rendered_command_text_range(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<TextRange> {
    let start = rendered_command_start_point(prompt, screen)?;
    let end = rendered_output_start_point(prompt, screen)
        .map(|output| {
            if output.row > start.row {
                text_point_from_row_start(output.row)
            } else {
                output
            }
        })
        .or_else(|| rendered_block_end_point(prompt, screen))?;
    text_range(start, end)
}

fn rendered_output_text_range(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<TextRange> {
    let start = rendered_output_start_point(prompt, screen)?;
    let end = rendered_block_end_point(prompt, screen)?;
    text_range(start, end)
}

fn rendered_command_and_output_text_range(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<TextRange> {
    let start = rendered_command_start_point(prompt, screen)?;
    let end = rendered_block_end_point(prompt, screen)?;
    text_range(start, end)
}

fn extract_rendered_range_text(
    screen: &Screen,
    range: TextRange,
) -> String {
    let Some(end_abs) = last_row_in_range(range) else {
        return String::new();
    };
    let mut out = String::new();
    for rendered_row in range.start.row..=end_abs {
        let Some(info) = rendered_row_info(screen, rendered_row) else {
            continue;
        };
        let row = info.row;
        let cs = if rendered_row == range.start.row {
            range.start.col as usize
        } else if row.output_start {
            row.output_start_col
                .unwrap_or_else(|| first_content_col(row)) as usize
        } else {
            0
        };
        let ce = if rendered_row == range.end.row {
            range.end.col as usize
        } else {
            row.cells.len()
        }
        .min(row.cells.len());
        if cs >= ce {
            if rendered_row < end_abs && !row.wrapped {
                out.push('\n');
            }
            continue;
        }
        let mut seg = String::new();
        for cell in &row.cells[cs..ce] {
            seg.push_str(cell);
        }
        out.push_str(seg.trim_end_matches(' '));
        if rendered_row < end_abs && !row.wrapped {
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

/// Extract the command text associated with a rendered prompt.
pub fn command_text_for_prompt(
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    if let Some(text) = rendered_command_text(prompt, screen) {
        return Some(text);
    }
    if let Some(prompt_abs) = prompt.active_abs_row
        && command_metas.get(&prompt_abs).is_some_and(|meta| {
            meta.command_col.is_some_and(|col| {
                col > 0
                    || meta
                        .command_row
                        .and_then(|row| selection::absolute_row_to_local(screen, row))
                        .and_then(|local| screen.grid.rows.get(local))
                        .is_some_and(|row| !row.prompt_start || row.content_len() == 0)
            })
        })
        && let Some(text) = command_text_at(prompt_abs, command_metas, screen)
    {
        return Some(text);
    }
    None
}

fn rendered_command_text(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<String> {
    let text = extract_rendered_range_text(screen, rendered_command_text_range(prompt, screen)?);
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

/// Extract command output associated with a rendered prompt.
pub fn output_text_for_prompt(
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    if let Some(text) = rendered_output_text(prompt, screen) {
        return Some(text);
    }
    if let Some(prompt_abs) = prompt.active_abs_row
        && let Some(text) = output_text_at(prompt_abs, command_metas, screen)
    {
        return Some(text);
    }
    None
}

fn rendered_output_text(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<String> {
    let text = extract_rendered_range_text(screen, rendered_output_text_range(prompt, screen)?);
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

/// Extract command text plus output associated with a rendered prompt.
pub fn command_and_output_text_for_prompt(
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    if let Some(text) = rendered_command_and_output_text(prompt, screen) {
        return Some(text);
    }
    if let Some(prompt_abs) = prompt.active_abs_row
        && let Some(text) = command_and_output_text_at(prompt_abs, command_metas, screen)
    {
        return Some(text);
    }
    None
}

fn rendered_command_and_output_text(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<String> {
    let text = extract_rendered_range_text(
        screen,
        rendered_command_and_output_text_range(prompt, screen)?,
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

/// Return the recorded runtime for a completed rendered command.
pub fn command_duration_for_prompt(
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
) -> Option<Duration> {
    command_duration_at(prompt.active_abs_row?, command_metas)
}

/// Build a read-only derived view for the command block associated with a
/// rendered prompt.
pub fn command_block_view_for_prompt(
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> CommandBlockView {
    let meta = prompt
        .active_abs_row
        .and_then(|prompt_abs| command_metas.get(&prompt_abs));
    let command = command_block_command_for_prompt(prompt, command_metas, screen);
    let output = output_text_for_prompt(prompt, command_metas, screen);
    let command_and_output = command_and_output_text_for_prompt(prompt, command_metas, screen);
    let duration = command_duration_for_prompt(prompt, command_metas);
    let exit_status = command_block_exit_status(prompt, screen);
    let state = command_block_state(meta, exit_status);

    CommandBlockView {
        prompt,
        command,
        output,
        command_and_output,
        duration,
        exit_status,
        state,
    }
}

fn command_block_command_for_prompt(
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<CommandBlockCommand> {
    if let Some(text) = command_text_for_prompt(prompt, command_metas, screen) {
        return Some(CommandBlockCommand {
            text,
            source: CommandTextSource::Observed,
        });
    }
    let text = untrusted_command_line_at(prompt.active_abs_row?, command_metas)?;
    Some(CommandBlockCommand {
        text: text.to_owned(),
        source: CommandTextSource::UntrustedMetadata,
    })
}

fn command_block_exit_status(
    prompt: PromptRef,
    screen: &Screen,
) -> Option<i32> {
    rendered_row_info(screen, prompt.rendered_row)?
        .row
        .exit_status
}

fn command_block_state(
    meta: Option<&CommandMeta>,
    exit_status: Option<i32>,
) -> CommandBlockState {
    if meta.is_some_and(|meta| meta.started_at.is_some() && meta.finished_at.is_none()) {
        return CommandBlockState::Running;
    }
    match exit_status {
        Some(0) => CommandBlockState::Succeeded,
        Some(_) => CommandBlockState::Failed,
        None if meta.is_some_and(|meta| meta.finished_at.is_some()) => CommandBlockState::Finished,
        None => CommandBlockState::Editing,
    }
}

/// Build a read-only document of all prompt-backed command blocks in rendered
/// order.
pub fn command_block_document(
    screen: &Screen,
    command_metas: &HashMap<u64, CommandMeta>,
) -> CommandBlockDocument {
    let blocks = command_block_prompts(screen)
        .into_iter()
        .map(|prompt| command_block_view_for_prompt(prompt, command_metas, screen))
        .collect();
    CommandBlockDocument { blocks }
}

/// Find the command block for an exact prompt reference.
pub fn command_block_for_prompt(
    document: &CommandBlockDocument,
    prompt: PromptRef,
) -> Option<&CommandBlockView> {
    document.blocks.iter().find(|block| block.prompt == prompt)
}

/// Find the command block for the nearest prompt at or above a viewport row.
pub fn command_block_for_screen_row<'a>(
    document: &'a CommandBlockDocument,
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> Option<&'a CommandBlockView> {
    let prompt = find_prompt_ref_for_screen_row(screen, viewport, screen_row)?;
    command_block_for_prompt(document, prompt)
}

/// Find the nearest matching command block before a rendered document row.
pub fn previous_command_block_matching(
    document: &CommandBlockDocument,
    rendered_top: usize,
    keep: impl Fn(&CommandBlockView) -> bool,
) -> Option<&CommandBlockView> {
    document
        .blocks
        .iter()
        .filter(|block| keep(block))
        .filter(|block| (block.prompt.rendered_row as usize) < rendered_top)
        .max_by_key(|block| block.prompt.rendered_row)
}

/// Find the next command block after a rendered document row.
pub fn next_command_block_after(
    document: &CommandBlockDocument,
    rendered_top: usize,
) -> Option<&CommandBlockView> {
    document
        .blocks
        .iter()
        .find(|block| (block.prompt.rendered_row as usize) > rendered_top)
}

fn command_block_prompts(screen: &Screen) -> Vec<PromptRef> {
    (0..screen::rendered_rows_len(screen) as u64)
        .filter_map(|rendered_row| {
            let info = rendered_row_info(screen, rendered_row)?;
            info.row
                .prompt_start
                .then(|| prompt_ref_for_rendered_row(screen, info))
        })
        .collect()
}

/// Build a read-only derived command block view for the nearest prompt at or
/// above a viewport row.
pub fn command_block_view_for_screen_row(
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
    command_metas: &HashMap<u64, CommandMeta>,
) -> Option<CommandBlockView> {
    let prompt = find_prompt_ref_for_screen_row(screen, viewport, screen_row)?;
    Some(command_block_view_for_prompt(prompt, command_metas, screen))
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
        rendered: false,
        origin: anchor,
    });
}

/// Replace the current selection with the command text at a rendered prompt.
pub fn select_command_for_prompt(
    selection: &mut Option<Selection>,
    prompt: PromptRef,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) {
    if select_rendered_command_for_prompt(selection, prompt, screen) {
        return;
    }
    if let Some(prompt_abs) = prompt.active_abs_row {
        select_command_at(selection, prompt_abs, command_metas, screen);
    }
}

fn select_rendered_command_for_prompt(
    selection: &mut Option<Selection>,
    prompt: PromptRef,
    screen: &Screen,
) -> bool {
    let Some(range) = rendered_command_text_range(prompt, screen) else {
        return false;
    };
    let text = extract_rendered_range_text(screen, range);
    if text.trim().is_empty() {
        return false;
    }
    let anchor = SelectionPoint {
        row: range.start.row,
        col: range.start.col,
    };
    let Some(head) = selection_head_for_rendered_range(screen, range) else {
        return false;
    };
    *selection = Some(Selection {
        anchor,
        head,
        mode: SelectionMode::Char,
        rendered: true,
        origin: anchor,
    });
    true
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

fn selection_head_for_rendered_range(
    screen: &Screen,
    range: TextRange,
) -> Option<SelectionPoint> {
    let end_abs = last_row_in_range(range)?;
    if range.end.col > 0 && range.end.row == end_abs {
        let col = rendered_row_info(screen, end_abs)
            .map(|info| range.end.col.min(info.row.len()))
            .unwrap_or(range.end.col);
        if col == 0 {
            return None;
        }
        return Some(SelectionPoint {
            row: end_abs,
            col: col - 1,
        });
    }
    let end_col = rendered_row_info(screen, end_abs)
        .map(|info| info.row.content_len().saturating_sub(1))
        .unwrap_or(0);
    Some(SelectionPoint {
        row: end_abs,
        col: end_col,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::CommandBlockState;
    use super::CommandTextSource;
    use super::PromptRef;
    use super::command_and_output_text_at;
    use super::command_and_output_text_for_prompt;
    use super::command_block_document;
    use super::command_block_for_prompt;
    use super::command_block_for_screen_row;
    use super::command_block_view_for_prompt;
    use super::command_block_view_for_screen_row;
    use super::command_text_at;
    use super::command_text_for_prompt;
    use super::find_prompt_for_screen_row;
    use super::find_prompt_ref_for_screen_row;
    use super::next_command_block_after;
    use super::output_text_at;
    use super::output_text_for_prompt;
    use super::previous_command_block_matching;
    use super::select_command_at;
    use super::select_command_for_prompt;
    use crate::screen;
    use crate::selection;
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
    fn indicator_status_tracks_running_command_after_resize_wrap() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.metadata.current_directory = Some(PathBuf::from("/tmp/project"));
        term.process(b"\x1b[1$~");
        term.process(b"\x1b]133;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]133;B\x07");
        term.process(b"abcdefghijkl");
        term.process(b"\r\n\x1b]133;C\x07");

        term.resize(8, 4);

        assert_eq!(
            term.indicator_status_text().as_deref(),
            Some("/ > tmp > project > abcdefghijkl")
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
    fn find_prompt_for_screen_row_ignores_completed_block_output_rows() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        emit_prompt(&mut term, "$ a", 1, 0);
        term.process(b"\x1b]133;A\x07$ b");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);

        assert!(term.active.scrollback_blocks[0].grid.rows[1].output_start);
        assert_eq!(
            find_prompt_for_screen_row(&term.active, &term.inner.viewport, 1),
            None
        );
    }

    #[test]
    fn find_prompt_for_screen_row_finds_active_command_output_rows() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ a\x1b]133;B\x07\n\x1b]133;C\x07out");

        assert!(term.active.grid.rows[1].output_start);
        assert_eq!(
            find_prompt_for_screen_row(&term.active, &term.inner.viewport, 3),
            Some(0)
        );
    }

    #[test]
    fn find_prompt_ref_for_screen_row_finds_completed_block_output_rows() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07ab\n\x1b]133;C\x07out0\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ b");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);

        let prompt = find_prompt_ref_for_screen_row(&term.active, &term.inner.viewport, 1)
            .expect("completed block prompt");

        assert_eq!(prompt.rendered_row, 0);
        assert_eq!(prompt.active_abs_row, None);
        assert_eq!(
            command_text_for_prompt(prompt, &term.metadata.command_metas, &term.active).as_deref(),
            Some("ab")
        );
        assert_eq!(
            output_text_for_prompt(prompt, &term.metadata.command_metas, &term.active).as_deref(),
            Some("out0")
        );
        assert_eq!(
            command_and_output_text_for_prompt(prompt, &term.metadata.command_metas, &term.active)
                .as_deref(),
            Some("ab\nout0")
        );
    }

    #[test]
    fn select_command_for_prompt_selects_completed_block_command_row() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07ab\n\x1b]133;C\x07out0\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ b");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);
        let prompt = find_prompt_ref_for_screen_row(&term.active, &term.inner.viewport, 1)
            .expect("completed block prompt");

        let mut selected = None;
        select_command_for_prompt(
            &mut selected,
            prompt,
            &term.metadata.command_metas,
            &term.active,
        );

        let selected = selected.expect("rendered command selection");
        assert!(selected.rendered);
        assert_eq!(
            selection::selection_text(Some(&selected), &term.active).as_deref(),
            Some("ab")
        );
    }

    #[test]
    fn command_text_for_prompt_requires_command_start_marker() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ ab\n\x1b]133;C\x07out0\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ next");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);
        let prompt = find_prompt_ref_for_screen_row(&term.active, &term.inner.viewport, 1)
            .expect("completed block prompt");

        assert_eq!(
            command_text_for_prompt(prompt, &term.metadata.command_metas, &term.active),
            None
        );
    }

    #[test]
    fn command_text_for_prompt_rejects_prompt_start_column_marker() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07\x1b]133;B\x07$ ab\n\x1b]133;C\x07out0\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ next");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);
        let prompt = find_prompt_ref_for_screen_row(&term.active, &term.inner.viewport, 1)
            .expect("completed block prompt");

        assert_eq!(
            command_text_for_prompt(prompt, &term.metadata.command_metas, &term.active),
            None
        );
    }

    #[test]
    fn command_block_view_prefers_observed_command_text() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.process(b"\x1b]633;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]633;B\x07");
        term.process(b"cargo test");
        term.process(b"\x1b]633;E;cargo\\x20metadata\x07");

        let view = command_block_view_for_prompt(
            PromptRef {
                rendered_row: 0,
                active_abs_row: Some(0),
            },
            &term.metadata.command_metas,
            &term.active,
        );

        let command = view.command.expect("command text");
        assert_eq!(command.text, "cargo test");
        assert_eq!(command.source, CommandTextSource::Observed);
        assert_eq!(view.state, CommandBlockState::Editing);
    }

    #[test]
    fn command_block_view_falls_back_to_untrusted_metadata() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.process(b"\x1b]633;A\x07");
        term.process(b"\x1b]633;E;cargo\\x20test\x07");

        let view = command_block_view_for_prompt(
            PromptRef {
                rendered_row: 0,
                active_abs_row: Some(0),
            },
            &term.metadata.command_metas,
            &term.active,
        );

        let command = view.command.expect("command text");
        assert_eq!(command.text, "cargo test");
        assert_eq!(command.source, CommandTextSource::UntrustedMetadata);
    }

    #[test]
    fn command_block_view_exposes_completed_block_output_and_exit_status() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07ab\n\x1b]133;C\x07out0\n\x1b]133;D;1\x07");
        term.process(b"\x1b]133;A\x07$ next");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);
        let view = command_block_view_for_screen_row(
            &term.active,
            &term.inner.viewport,
            1,
            &term.metadata.command_metas,
        )
        .expect("completed block prompt");

        assert_eq!(
            view.command.as_ref().map(|command| command.text.as_str()),
            Some("ab")
        );
        assert_eq!(view.output.as_deref(), Some("out0"));
        assert_eq!(view.command_and_output.as_deref(), Some("ab\nout0"));
        assert_eq!(view.exit_status, Some(1));
        assert_eq!(view.state, CommandBlockState::Failed);
    }

    #[test]
    fn command_block_document_lists_completed_and_active_blocks() {
        let mut term = TestTerm::new(12, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07one\n\x1b]133;C\x07out\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07two");

        let document = command_block_document(&term.active, &term.metadata.command_metas);

        assert_eq!(document.blocks.len(), 2);
        assert_eq!(
            document.blocks[0]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("one")
        );
        assert_eq!(document.blocks[0].exit_status, Some(0));
        assert_eq!(document.blocks[0].state, CommandBlockState::Succeeded);
        assert_eq!(document.blocks[0].prompt.active_abs_row, None);

        assert_eq!(
            document.blocks[1]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("two")
        );
        assert_eq!(document.blocks[1].exit_status, None);
        assert_eq!(document.blocks[1].state, CommandBlockState::Editing);
        assert!(document.blocks[1].prompt.active_abs_row.is_some());
        assert!(document.blocks[0].prompt.rendered_row < document.blocks[1].prompt.rendered_row);
    }

    #[test]
    fn command_block_document_keeps_wrapped_running_command_in_active_block() {
        let mut term = TestTerm::new(8, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07one\n\x1b]133;C\x07out\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\r\n\x1b]133;C\x07");

        let document = command_block_document(&term.active, &term.metadata.command_metas);

        assert_eq!(document.blocks.len(), 2);
        assert_eq!(
            document.blocks[0]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("one")
        );
        assert_eq!(
            document.blocks[1]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("abcdefghijk")
        );
        assert_eq!(document.blocks[1].state, CommandBlockState::Running);
        assert_eq!(document.blocks[1].prompt.active_abs_row, Some(0));
        assert!(document.blocks[0].prompt.rendered_row < document.blocks[1].prompt.rendered_row);
    }

    #[test]
    fn command_block_document_tracks_active_command_rows_after_resize_wrap() {
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07one\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\r\n\x1b]133;C\x07running");

        term.resize(8, 6);
        let document = command_block_document(&term.active, &term.metadata.command_metas);

        assert_eq!(document.blocks.len(), 2);
        assert_eq!(
            document.blocks[1]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("abcdefghijk")
        );
        assert_eq!(document.blocks[1].output.as_deref(), Some("running"));
        assert_eq!(document.blocks[1].state, CommandBlockState::Running);
    }

    #[test]
    fn command_block_document_tracks_command_start_that_reflows_to_continuation_row() {
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x07prompt: \x1b]133;B\x07abcdef");

        term.resize(8, 6);
        let document = command_block_document(&term.active, &term.metadata.command_metas);

        assert_eq!(document.blocks.len(), 1);
        assert_eq!(
            document.blocks[0]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("abcdef")
        );
    }

    #[test]
    fn command_block_document_tracks_output_start_that_reflows_to_continuation_row() {
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07abcdef\x1b]133;C\x07output");

        term.resize(8, 6);
        let document = command_block_document(&term.active, &term.metadata.command_metas);

        assert_eq!(document.blocks.len(), 1);
        assert_eq!(
            document.blocks[0]
                .command
                .as_ref()
                .map(|command| command.text.as_str()),
            Some("abcdef")
        );
        assert_eq!(document.blocks[0].output.as_deref(), Some("output"));
    }

    #[test]
    fn select_command_for_prompt_tracks_active_command_rows_after_resize_wrap() {
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07one\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\r\n\x1b]133;C\x07running");
        term.resize(8, 6);
        let document = command_block_document(&term.active, &term.metadata.command_metas);
        let prompt = document.blocks[1].prompt;

        let mut selected = None;
        select_command_for_prompt(
            &mut selected,
            prompt,
            &term.metadata.command_metas,
            &term.active,
        );

        let selected = selected.expect("active rendered command selection");
        assert!(selected.rendered);
        assert_eq!(
            selection::selection_text(Some(&selected), &term.active).as_deref(),
            Some("abcdefghijk")
        );
    }

    #[test]
    fn command_block_queries_find_blocks_by_prompt_screen_row_and_position() {
        let mut term = TestTerm::new(12, 4, 100, 16, 8);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07one\n\x1b]133;C\x07out\n\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07two");
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.inner.viewport);

        let document = command_block_document(&term.active, &term.metadata.command_metas);
        let first = &document.blocks[0];
        let second = &document.blocks[1];

        assert_eq!(
            command_block_for_prompt(&document, first.prompt).map(|block| block.prompt),
            Some(first.prompt)
        );
        assert_eq!(
            command_block_for_screen_row(&document, &term.active, &term.inner.viewport, 0)
                .map(|block| block.prompt),
            Some(first.prompt)
        );
        assert_eq!(
            previous_command_block_matching(
                &document,
                second.prompt.rendered_row as usize,
                |block| block.state == CommandBlockState::Succeeded,
            )
            .map(|block| block.prompt),
            Some(first.prompt)
        );
        assert_eq!(
            next_command_block_after(&document, first.prompt.rendered_row as usize)
                .map(|block| block.prompt),
            Some(second.prompt)
        );
    }

    #[test]
    fn scroll_to_prev_prompt_moves_viewport() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        let before = term.active.offset;
        let viewport = term.inner.viewport;
        let document = command_block_document(&term.inner.active, &term.metadata.command_metas);
        view::scroll_to_prev_prompt(&mut term.inner.active, &viewport, &document);
        assert!(term.active.offset > before);
    }

    #[test]
    fn scroll_to_prev_prompt_silent_with_no_marks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"plain\noutput\nwithout\nshell integration\n");
        let before = term.active.offset;
        let viewport = term.inner.viewport;
        let document = command_block_document(&term.inner.active, &term.metadata.command_metas);
        view::scroll_to_prev_prompt(&mut term.inner.active, &viewport, &document);
        assert_eq!(term.active.offset, before);
    }

    #[test]
    fn scroll_to_next_prompt_walks_forward() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        term.active.offset = screen::rendered_scrollback_len(&term.active, &term.viewport);
        let start = term.active.offset;
        let viewport = term.inner.viewport;
        let document = command_block_document(&term.inner.active, &term.metadata.command_metas);
        view::scroll_to_next_prompt(&mut term.inner.active, &viewport, &document);
        assert!(term.active.offset < start);
    }

    #[test]
    fn scroll_to_next_prompt_silent_at_last_prompt() {
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
        emit_prompt(&mut term, "$ only", 3, 0);
        let before = term.active.offset;
        let viewport = term.inner.viewport;
        let document = command_block_document(&term.inner.active, &term.metadata.command_metas);
        view::scroll_to_next_prompt(&mut term.inner.active, &viewport, &document);
        assert_eq!(term.active.offset, before);
    }
}

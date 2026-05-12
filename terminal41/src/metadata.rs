use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::selection::Selection;
use crate::selection::search::SearchState;

/// Current OSC 133 shell-integration phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellIntegrationPhase {
    /// No active shell-integration phase is known.
    #[default]
    None,
    /// Prompt text is being printed after `OSC 133;A`.
    Prompt,
    /// User command editing is active after `OSC 133;B`.
    Command,
    /// Command output is active after `OSC 133;C`.
    Output,
    /// The previous command has finished after `OSC 133;D`.
    Finished,
}

/// Per-prompt metadata recorded from OSC 133 / OSC 633 shell-integration
/// sequences. Keyed by the absolute row of the prompt (`A` mark) in
/// [`TerminalMetadata::command_metas`]. Enables command selection, rerun, text
/// extraction, and duration display in the gutter popup.
#[derive(Debug)]
pub struct CommandMeta {
    /// Column where the user's command text begins (from OSC 133 `B`).
    /// `None` when the shell doesn't emit `B`.
    pub command_col: Option<u32>,
    /// Absolute row where OSC 133 `B` fired. Usually the same as the
    /// prompt row, but multi-line prompts can differ.
    pub command_row: Option<u64>,
    /// Absolute row where OSC 133 `C` fired (command output starts).
    pub output_row: Option<u64>,
    /// Column where command output begins (from OSC 133 `C`).
    /// `None` when the shell doesn't emit `C`.
    pub output_col: Option<u32>,
    /// When execution started (timestamped at `C`).
    pub started_at: Option<Instant>,
    /// When the command finished (timestamped at `D`).
    pub finished_at: Option<Instant>,
    /// Absolute row where OSC 133 `D` fired (command output ends).
    pub finished_row: Option<u64>,
    /// Column where OSC 133 `D` fired (command output ends).
    pub finished_col: Option<u32>,
    /// Command line reported by OSC 633 `E`. This is host-provided metadata,
    /// not terminal-observed text. Screen-extracted command text remains the
    /// preferred source; UI code may only display this as an annotation or
    /// use it as a lower-trust fallback when no observed command text exists.
    pub untrusted_command_line: Option<String>,
}

impl CommandMeta {
    pub(crate) fn new() -> Self {
        Self {
            command_col: None,
            command_row: None,
            output_row: None,
            output_col: None,
            started_at: None,
            finished_at: None,
            finished_row: None,
            finished_col: None,
            untrusted_command_line: None,
        }
    }
}

/// Shell/app metadata derived from OSC and window-title sequences.
#[derive(Debug, Default)]
pub struct TerminalMetadata {
    /// Last directory reported by the foreground shell via OSC 7.
    pub current_directory: Option<PathBuf>,
    /// Title last reported by the foreground app via OSC 0 / OSC 2.
    pub current_title: Option<String>,
    /// xterm title stack. CSI 22;0 t pushes, CSI 23;0 t pops.
    pub title_stack: Vec<Option<String>>,
    /// Absolute row index of the most recent OSC 133 `A` (prompt-start) mark.
    pub current_prompt_row: Option<u64>,
    /// Per-prompt metadata (command column, output row, timing) keyed by the
    /// absolute row of the prompt's `A` mark.
    pub command_metas: HashMap<u64, CommandMeta>,
    /// Most recent OSC 133 / OSC 633 phase. Used only as a compatibility hint;
    /// terminal semantics still come from explicit VT input.
    pub shell_integration_phase: ShellIntegrationPhase,
}

pub(crate) fn shift_visible_absolute_rows(
    selection: &mut Option<Selection>,
    search: &mut SearchState,
    delta: u64,
) {
    if delta == 0 {
        return;
    }
    if let Some(selection) = selection {
        selection.anchor.row = selection.anchor.row.saturating_add(delta);
        selection.head.row = selection.head.row.saturating_add(delta);
        selection.origin.row = selection.origin.row.saturating_add(delta);
    }
    for span in &mut search.matches {
        span.row = span.row.saturating_add(delta);
    }
}

pub(crate) fn shift_terminal_metadata_rows(
    metadata: &mut TerminalMetadata,
    delta: u64,
) {
    if delta == 0 {
        return;
    }
    metadata.current_prompt_row = metadata
        .current_prompt_row
        .map(|row| row.saturating_add(delta));

    metadata.command_metas = metadata
        .command_metas
        .drain()
        .map(|(row, mut meta)| {
            shift_command_meta_rows(&mut meta, delta);
            (row.saturating_add(delta), meta)
        })
        .collect();
}

fn shift_command_meta_rows(
    meta: &mut CommandMeta,
    delta: u64,
) {
    meta.command_row = meta.command_row.map(|row| row.saturating_add(delta));
    meta.output_row = meta.output_row.map(|row| row.saturating_add(delta));
    meta.finished_row = meta.finished_row.map(|row| row.saturating_add(delta));
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use super::directory;
use super::directory::DirectoryAction;
use super::split_key_value;
use super::split_osc;
use crate::CommandMeta;
use crate::Row;
use crate::ShellIntegrationPhase;
use crate::screen::Screen;
use crate::screen::grid::Viewport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShellIntegrationAction {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandFinished { exit: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VscodeShellIntegrationAction {
    Lifecycle(ShellIntegrationAction),
    SetDirectory(DirectoryAction),
    SetCommandLine(String),
}

pub(super) fn parse_osc_133(rest: &[u8]) -> Option<ShellIntegrationAction> {
    let (kind, args) = split_osc(rest);
    parse_lifecycle(kind, args)
}

pub(super) fn parse_osc_633(rest: &[u8]) -> Option<VscodeShellIntegrationAction> {
    let (kind, args) = split_osc(rest);
    if let Some(action) = parse_lifecycle(kind, args) {
        return Some(VscodeShellIntegrationAction::Lifecycle(action));
    }

    match kind {
        b"P" => parse_osc_633_property(args),
        b"E" => parse_osc_633_command_line(args).map(VscodeShellIntegrationAction::SetCommandLine),
        _ => None,
    }
}

pub(super) fn apply_vscode(
    action: VscodeShellIntegrationAction,
    current_directory: &mut Option<PathBuf>,
    screen: &mut Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    match action {
        VscodeShellIntegrationAction::Lifecycle(action) => apply(
            action,
            screen,
            viewport,
            on_alt_screen,
            current_prompt_row,
            shell_integration_phase,
            command_metas,
        ),
        VscodeShellIntegrationAction::SetDirectory(action) => {
            directory::apply(action, current_directory);
        }
        VscodeShellIntegrationAction::SetCommandLine(command_line) => {
            apply_command_line_metadata(command_line, *current_prompt_row, command_metas);
        }
    }
}

pub(super) fn apply(
    action: ShellIntegrationAction,
    screen: &mut Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    match action {
        ShellIntegrationAction::PromptStart => {
            if !on_alt_screen {
                crate::screen::start_command_block(screen, viewport);
            }
            let abs = mark_current_row(screen, viewport, |row| {
                row.prompt_start = true;
                row.command_start_col = None;
                row.output_start_col = None;
                // A fresh prompt invalidates any lingering exit_status from
                // a prior occupant of this row (e.g. a recycled scrollback
                // slot). The shell hasn't even shown the prompt yet.
                row.exit_status = None;
            });
            *current_prompt_row = Some(abs);
            *shell_integration_phase = ShellIntegrationPhase::Prompt;
            command_metas.insert(abs, CommandMeta::new());
        }
        ShellIntegrationAction::CommandStart => {
            *shell_integration_phase = ShellIntegrationPhase::Command;
            // Prompt end / command start. Record the cursor column so
            // "select command" can skip the prompt decoration.
            if let Some(prompt_abs) = *current_prompt_row {
                let command_col = screen.cursor.col;
                let abs = mark_current_row(screen, viewport, |row| {
                    row.command_start_col = Some(command_col);
                });
                if let Some(meta) = command_metas.get_mut(&prompt_abs) {
                    meta.command_col = Some(command_col);
                    meta.command_row = Some(abs);
                }
            }
        }
        ShellIntegrationAction::OutputStart => {
            *shell_integration_phase = ShellIntegrationPhase::Output;
            let output_col = screen.cursor.col;
            let abs = mark_current_row(screen, viewport, |row| {
                row.output_start = true;
                row.output_start_col = Some(output_col);
            });
            if let Some(prompt_abs) = *current_prompt_row
                && let Some(meta) = command_metas.get_mut(&prompt_abs)
            {
                meta.output_row = Some(abs);
                meta.output_col = Some(output_col);
                meta.started_at = Some(Instant::now());
            }
        }
        ShellIntegrationAction::CommandFinished { exit } => {
            *shell_integration_phase = ShellIntegrationPhase::Finished;
            if let Some(abs) = *current_prompt_row
                && let Some(local) = absolute_to_local(screen, abs)
            {
                screen.grid.rows[local].exit_status = Some(exit);
            }
            if let Some(prompt_abs) = *current_prompt_row
                && let Some(meta) = command_metas.get_mut(&prompt_abs)
            {
                meta.finished_row = Some(current_absolute_row(screen, viewport));
                meta.finished_col = Some(screen.cursor.col);
                meta.finished_at = Some(Instant::now());
            }
        }
    }
}

fn parse_lifecycle(
    kind: &[u8],
    args: &[u8],
) -> Option<ShellIntegrationAction> {
    match kind {
        b"A" => Some(ShellIntegrationAction::PromptStart),
        b"B" => Some(ShellIntegrationAction::CommandStart),
        b"C" => Some(ShellIntegrationAction::OutputStart),
        b"D" => Some(ShellIntegrationAction::CommandFinished {
            exit: parse_shell_integration_exit(args),
        }),
        _ => None,
    }
}

fn parse_osc_633_property(args: &[u8]) -> Option<VscodeShellIntegrationAction> {
    let (key, value) = split_key_value(args)?;
    if key != b"Cwd" {
        return None;
    }
    directory::parse_absolute_or_file(value).map(VscodeShellIntegrationAction::SetDirectory)
}

fn parse_osc_633_command_line(args: &[u8]) -> Option<String> {
    let (command_line, _) = split_osc(args);
    decode_osc_633_command_line(command_line)
}

fn apply_command_line_metadata(
    command_line: String,
    current_prompt_row: Option<u64>,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    let Some(prompt_abs) = current_prompt_row else {
        return;
    };
    let Some(meta) = command_metas.get_mut(&prompt_abs) else {
        return;
    };
    meta.untrusted_command_line = Some(command_line);
}

/// Run `apply` on the row the cursor currently occupies and return that
/// row's absolute index (stable under scrollback trimming). Factored out
/// because every OSC 133 kind that stores a mark does the same lookup.
fn mark_current_row(
    screen: &mut Screen,
    viewport: &Viewport,
    apply: impl FnOnce(&mut Row),
) -> u64 {
    crate::screen::ensure_cursor_row_exists(screen, viewport);
    let local = crate::screen::active_row_index(screen, viewport);
    apply(&mut screen.grid.rows[local]);
    (screen.grid.total_popped + local) as u64
}

/// Return the absolute row index the cursor currently sits on, without
/// mutating the row. Used by OSC 133 B to record the command start row.
fn current_absolute_row(
    screen: &Screen,
    viewport: &Viewport,
) -> u64 {
    let local = crate::screen::active_row_index(screen, viewport);
    (screen.grid.total_popped + local) as u64
}

/// Translate an absolute row index into a live grid offset, or `None` if
/// the row has already fallen off the front of scrollback.
fn absolute_to_local(
    screen: &Screen,
    abs: u64,
) -> Option<usize> {
    let popped = screen.grid.total_popped as u64;
    let local = abs.checked_sub(popped)? as usize;
    (local < screen.grid.rows.len()).then_some(local)
}

/// Parse the exit code from an OSC 133 `D` payload. Per the spec the first
/// argument is the exit status; non-numeric or missing values are treated
/// as success (`0`) so a shell that merely reports "command finished"
/// without the numeric status doesn't accidentally paint every prompt red.
fn parse_shell_integration_exit(args: &[u8]) -> i32 {
    let (first, _) = split_osc(args);
    std::str::from_utf8(first)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0)
}

fn decode_osc_633_command_line(command_line: &[u8]) -> Option<String> {
    let mut decoded = Vec::with_capacity(command_line.len());
    let mut i = 0;
    while i < command_line.len() {
        if command_line[i] != b'\\' {
            decoded.push(command_line[i]);
            i += 1;
            continue;
        }

        let escaped = *command_line.get(i + 1)?;
        match escaped {
            b'\\' => {
                decoded.push(b'\\');
                i += 2;
            }
            b'x' | b'X' => {
                let hi = *command_line.get(i + 2)?;
                let lo = *command_line.get(i + 3)?;
                decoded.push((hex_nibble(hi)? << 4) | hex_nibble(lo)?);
                i += 4;
            }
            _ => return None,
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

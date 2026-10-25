use std::time::Instant;

use crate::HyperlinkRegistry;
use crate::Row;
use crate::Screen;
use crate::StatusDisplayKind;
use crate::TerminalMetadata;
use crate::Viewport;
use crate::VisibleImage;
use crate::lifecycle_ops;
use crate::prompt;
use crate::screen;
use crate::selection;

/// A command-block boundary relative to the current viewport.
pub enum ScrollTarget {
    PreviousPrompt,
    NextPrompt,
    PreviousCommand,
    PreviousFailedCommand,
    PreviousSuccessfulCommand,
}

/// An edit or navigation step in the terminal's search interface.
pub enum SearchAction<'a> {
    Open,
    Close,
    Append(&'a str),
    Backspace,
    Previous,
    Next,
}

impl crate::Terminal {
    /// Scroll into history and invalidate the rows whose presentation changed.
    pub fn scroll_viewport_up(
        &mut self,
        lines: u32,
    ) -> u32 {
        let delta = scroll_viewport_up(&mut self.active, &self.viewport, lines);
        if delta != 0 {
            self.invalidate_snapshot_rows();
        }
        delta
    }

    /// Scroll toward the live bottom and invalidate changed rows.
    pub fn scroll_viewport_down(
        &mut self,
        lines: u32,
    ) -> u32 {
        let delta = scroll_viewport_down(&mut self.active, lines);
        if delta != 0 {
            self.invalidate_snapshot_rows();
        }
        delta
    }

    /// Move to a bounded scrollback offset and invalidate changed rows.
    pub fn set_viewport_offset(
        &mut self,
        offset: u32,
    ) {
        if screen::page_memory_active(&self.active) {
            return;
        }
        let max = screen::rendered_scrollback_len(&self.active, &self.viewport);
        let offset = offset.min(max);
        if self.active.offset != offset {
            self.active.offset = offset;
            self.invalidate_snapshot_rows();
        }
    }

    pub fn scroll_to(
        &mut self,
        target: ScrollTarget,
    ) {
        let document = prompt::command_block_document(&self.active, &self.metadata.command_metas);
        match target {
            ScrollTarget::PreviousPrompt => {
                scroll_to_prev_prompt(&mut self.active, &self.viewport, &document)
            }
            ScrollTarget::NextPrompt => {
                scroll_to_next_prompt(&mut self.active, &self.viewport, &document)
            }
            ScrollTarget::PreviousCommand => {
                scroll_to_prev_command(&mut self.active, &self.viewport, &document)
            }
            ScrollTarget::PreviousFailedCommand => {
                scroll_to_prev_failed_command(&mut self.active, &self.viewport, &document)
            }
            ScrollTarget::PreviousSuccessfulCommand => {
                scroll_to_prev_successful_command(&mut self.active, &self.viewport, &document)
            }
        }
        self.invalidate_snapshot_rows();
    }

    /// Update search, selection, and viewport as one presentation change.
    pub fn update_search(
        &mut self,
        action: SearchAction<'_>,
    ) {
        let offset = match action {
            SearchAction::Open => {
                selection::open_search(&mut self.search);
                self.active.offset
            }
            SearchAction::Close => {
                selection::close_search(&mut self.search, &mut self.selection);
                self.active.offset
            }
            SearchAction::Append(text) => {
                selection::search_append(&mut self.search, &self.active, &self.viewport, text)
            }
            SearchAction::Backspace => {
                selection::search_backspace(&mut self.search, &self.active, &self.viewport)
            }
            SearchAction::Previous => {
                selection::search_step_prev(&mut self.search, &self.active, &self.viewport)
            }
            SearchAction::Next => {
                selection::search_step_next(&mut self.search, &self.active, &self.viewport)
            }
        };
        self.set_viewport_offset(offset);
        self.invalidate_snapshot_rows();
    }

    /// Copy the current selection without exposing mutable screen state.
    pub fn copy_selection(
        &mut self,
        kind: clip41::ClipboardKind,
    ) {
        selection::copy_selection(
            &mut self.clipboard,
            self.selection.as_ref(),
            &self.active,
            kind,
        );
    }

    pub fn select_command_for_prompt(
        &mut self,
        prompt: prompt::PromptRef,
    ) {
        prompt::select_command_for_prompt(
            &mut self.selection,
            prompt,
            &self.metadata.command_metas,
            &self.active,
        );
        self.invalidate_snapshot_rows();
    }
}

/// Return the number of rows currently presented to the host, including any
/// visible status line row that consumes part of the window height.
pub fn total_rows(
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    lifecycle_ops::total_rows(screen, viewport)
}

/// Return whether the given screen currently shows a status line row.
pub fn status_line_visible(screen: &Screen) -> bool {
    lifecycle_ops::status_line_visible(screen)
}

/// Return the active status-display mode for the given screen.
pub fn status_display_kind(screen: &Screen) -> StatusDisplayKind {
    screen.status_display
}

/// Return the visible status line row, if any.
pub fn status_line_row(screen: &Screen) -> Option<&Row> {
    lifecycle_ops::status_line_row(screen)
}

/// Format the indicator-status text for the current prompt / cwd state.
pub fn indicator_status_text(
    metadata: &TerminalMetadata,
    screen: &Screen,
) -> Option<String> {
    (screen.status_display == StatusDisplayKind::Indicator)
        .then(|| {
            prompt::format_indicator_status(
                metadata.current_directory.as_deref(),
                metadata.current_prompt_row,
                &metadata.command_metas,
                screen,
            )
        })
        .filter(|text| !text.is_empty())
}

/// Return the visible status-line cursor column when the status line owns the
/// active cursor; otherwise `None`.
pub fn status_line_cursor_col(screen: &Screen) -> Option<u32> {
    lifecycle_ops::status_line_cursor_col(screen)
}

/// Return the cursor row as a physical viewport row (`0` = top).
///
/// Primary-screen command blocks may be stored as compact active blocks and
/// bottom-aligned at render time. This helper returns the row the user sees,
/// which is the coordinate UI placement code needs.
pub fn cursor_viewport_row(
    screen: &Screen,
    viewport: &Viewport,
    on_alt_screen: bool,
) -> u32 {
    screen::cursor_viewport_row(screen, viewport, on_alt_screen)
}

/// Return the visible row at the given viewport row index (`0` = top).
pub fn visible_row<'a>(
    screen: &'a Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> &'a Row {
    let base = selection::active_viewport(screen, viewport).top_index(screen.grid.rows.len());
    &screen.grid.rows[base + screen_row as usize]
}

/// Resolve the hyperlink target at the given viewport cell.
pub fn hyperlink_at<'a>(
    screen: &Screen,
    viewport: &Viewport,
    hyperlinks: &'a HyperlinkRegistry,
    screen_row: u32,
    screen_col: u32,
) -> Option<&'a str> {
    if screen_row >= viewport.rows || screen_col >= viewport.cols {
        return None;
    }
    let row = visible_row(screen, viewport, screen_row);
    let id = row.links.get(screen_col as usize).copied().flatten()?;
    hyperlinks.get(id)
}

/// Scroll the viewport upward into scrollback. Returns the actual delta.
pub fn scroll_viewport_up(
    screen: &mut Screen,
    viewport: &Viewport,
    lines: u32,
) -> u32 {
    lifecycle_ops::scroll_viewport_up(screen, viewport, lines)
}

/// Move the viewport to the previous prompt above the current viewport top.
pub fn scroll_to_prev_prompt(
    screen: &mut Screen,
    viewport: &Viewport,
    document: &prompt::CommandBlockDocument,
) {
    lifecycle_ops::scroll_to_prev_prompt(screen, viewport, document)
}

/// Move the viewport to the next prompt below the current viewport top.
pub fn scroll_to_next_prompt(
    screen: &mut Screen,
    viewport: &Viewport,
    document: &prompt::CommandBlockDocument,
) {
    lifecycle_ops::scroll_to_next_prompt(screen, viewport, document)
}

/// Move the viewport to the previous failed command above the current
/// viewport top.
pub fn scroll_to_prev_failed_command(
    screen: &mut Screen,
    viewport: &Viewport,
    document: &prompt::CommandBlockDocument,
) {
    lifecycle_ops::scroll_to_prev_failed_command(screen, viewport, document)
}

/// Move the viewport to the previous command above the current viewport top.
pub fn scroll_to_prev_command(
    screen: &mut Screen,
    viewport: &Viewport,
    document: &prompt::CommandBlockDocument,
) {
    lifecycle_ops::scroll_to_prev_command(screen, viewport, document)
}

/// Move the viewport to the previous successful command above the current
/// viewport top.
pub fn scroll_to_prev_successful_command(
    screen: &mut Screen,
    viewport: &Viewport,
    document: &prompt::CommandBlockDocument,
) {
    lifecycle_ops::scroll_to_prev_successful_command(screen, viewport, document)
}

/// Scroll the viewport downward toward the live bottom. Returns the actual
/// delta.
pub fn scroll_viewport_down(
    screen: &mut Screen,
    lines: u32,
) -> u32 {
    lifecycle_ops::scroll_viewport_down(screen, lines)
}

/// Reset the viewport to the live bottom.
pub fn reset_viewport(screen: &mut Screen) {
    lifecycle_ops::reset_viewport(screen)
}

/// Current scrollback offset. `0` means the live bottom is showing.
pub fn viewport_offset(screen: &Screen) -> u32 {
    screen.offset
}

/// Total rows in the rendered document: every completed command block plus
/// its separator row, then the active block.
pub fn rendered_rows_len(
    screen: &Screen,
    viewport: &Viewport,
) -> usize {
    screen::rendered_rows_len_for_viewport(screen, viewport)
}

/// Whether DECCKM application cursor-key mode is active.
pub fn app_cursor_keys(screen: &Screen) -> bool {
    screen.app_cursor_keys
}

/// Whether DECKPAM application keypad mode is active.
pub fn app_keypad(screen: &Screen) -> bool {
    screen.app_keypad
}

/// A prompt row pinned to the top of the view. See
/// [`sticky_prompt_above_view`].
pub struct StickyPrompt<'a> {
    /// The prompt row itself.
    pub row: &'a Row,
    /// Its row number in the rendered document.
    pub rendered_row: u64,
    /// Its absolute active-grid row, or `None` for a completed block.
    /// This includes trimmed rows but excludes preceding completed blocks.
    pub active_absolute_row: Option<u64>,
}

/// Find the closest prompt row at or above the top of the view, so the
/// renderer can pin it while its output scrolls underneath.
///
/// `viewport_rows` comes from the published snapshot the renderer is drawing,
/// which can briefly disagree with the live viewport during a resize.
pub fn sticky_prompt_above_view<'a>(
    screen: &'a Screen,
    viewport: &Viewport,
    viewport_rows: u32,
) -> Option<StickyPrompt<'a>> {
    let found = lifecycle_ops::sticky_prompt_above_view(screen, viewport, viewport_rows)?;
    Some(StickyPrompt {
        row: found.row,
        rendered_row: found.rendered_row,
        active_absolute_row: found.active_absolute_row,
    })
}

/// Iterate the images whose row range overlaps the current viewport.
pub fn visible_images(
    screen: &Screen,
    viewport: &Viewport,
    cell_height: u32,
    cell_width: u32,
    kitty_images: &image41::kitty::KittyImageStore,
    palette: &crate::ColorPalette,
    now: Instant,
) -> impl Iterator<Item = VisibleImage> {
    lifecycle_ops::visible_images(
        screen,
        viewport,
        cell_height,
        cell_width,
        kitty_images,
        palette,
        now,
    )
}

/// A conservative acquisition point for terminal-owned shell editing. A
/// displayed command is not a reliable copy of the shell's actual buffer.
pub fn shell_prompt_is_empty(terminal: &crate::Terminal) -> bool {
    let Some(prompt) = terminal.metadata.current_prompt_row else {
        return false;
    };
    let Some(meta) = terminal.metadata.command_metas.get(&prompt) else {
        return false;
    };
    let Some(col) = meta.command_col else {
        return false;
    };
    let local = crate::screen::active_row_index(&terminal.active, &terminal.viewport);
    let absolute = (terminal.active.grid.total_popped + local) as u64;
    meta.command_row == Some(absolute)
        && terminal.active.cursor.col == col
        && terminal
            .active
            .grid
            .rows
            .get(local)
            .is_some_and(|row| row.content_len() <= col)
}

/// Identify the prompt across archived command blocks, whose active grids
/// can reuse the same local row numbers.
pub fn shell_prompt_document_row(terminal: &crate::Terminal) -> Option<u64> {
    let prompt = terminal.metadata.current_prompt_row?;
    let local = prompt.checked_sub(terminal.active.grid.total_popped as u64)?;
    Some(crate::screen::active_block_document_base(&terminal.active) + local)
}

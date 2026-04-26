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
use crate::selection;

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
) {
    lifecycle_ops::scroll_to_prev_prompt(screen, viewport)
}

/// Move the viewport to the next prompt below the current viewport top.
pub fn scroll_to_next_prompt(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    lifecycle_ops::scroll_to_next_prompt(screen, viewport)
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

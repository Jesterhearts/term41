use super::write::put_status_char;
use super::write::status_delete_chars;
use super::write::status_erase_chars;
use super::write::status_insert_chars;
use super::write::status_line_mut;
use crate::Screen;
use crate::charset;
use crate::color;
use crate::color::apply_sgr_groups;
use crate::parser::AsciiControlBytes;
use crate::parser::StatusLineCsiAction;
use crate::parser::next_tab_stop;

pub(crate) fn execute_status(
    screen: &mut Screen,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    let Ok(byte) = AsciiControlBytes::try_from(byte) else {
        return;
    };

    match byte {
        AsciiControlBytes::Nul => {}
        AsciiControlBytes::Bell => *bell_pending = true,
        AsciiControlBytes::Backspace => {
            if let Some(status) = status_line_mut(screen) {
                status.cursor.col = status.cursor.col.saturating_sub(1);
            }
        }
        AsciiControlBytes::HorizontalTab => {
            let tab_stops = screen.tab_stops.clone();
            if let Some(status) = status_line_mut(screen) {
                let cols = status.row.len().max(1);
                status.cursor.col = next_tab_stop(&tab_stops, status.cursor.col, cols);
            }
        }
        AsciiControlBytes::LineFeed
        | AsciiControlBytes::VerticalTab
        | AsciiControlBytes::FormFeed => {
            if newline_mode && let Some(status) = status_line_mut(screen) {
                status.cursor.col = 0;
            }
        }
        AsciiControlBytes::CarriageReturn => {
            if let Some(status) = status_line_mut(screen) {
                status.cursor.col = 0;
            }
        }
        AsciiControlBytes::ShiftOut => screen.charset.set_gl(charset::GraphicSetSlot::G1),
        AsciiControlBytes::ShiftIn => screen.charset.set_gl(charset::GraphicSetSlot::G0),
    }
}

pub(crate) fn apply_status_line_csi(
    screen: &mut Screen,
    palette: &mut color::ColorPalette,
    insert_mode: bool,
    action: StatusLineCsiAction,
) {
    let Some(status) = status_line_mut(screen) else {
        return;
    };
    let cols = status.row.len().max(1);
    let cursor = &mut status.cursor;

    match action {
        StatusLineCsiAction::SetGraphicsRendition { params } => {
            let mut palette = palette.clone();
            palette.fg = palette.status_line_fg;
            palette.bg = palette.status_line_bg;
            apply_sgr_groups(
                &mut status.fg,
                &mut status.bg,
                &mut status.attrs,
                &mut status.underline,
                &mut status.underline_color,
                params.as_groups(),
                &palette,
            );
        }
        StatusLineCsiAction::InsertChars { count } => {
            status_insert_chars(status, count as usize);
        }
        StatusLineCsiAction::HomeRow => {
            cursor.row = 0;
        }
        StatusLineCsiAction::CursorForward { count } => {
            cursor.col = (cursor.col + count as u32).min(cols - 1);
        }
        StatusLineCsiAction::CursorBackward { count } => {
            cursor.col = cursor.col.saturating_sub(count as u32);
        }
        StatusLineCsiAction::CursorHorizontalAbsolute { col } => {
            cursor.col = (col.max(1) as u32 - 1).min(cols - 1);
        }
        StatusLineCsiAction::CursorPosition { col } => {
            cursor.row = 0;
            cursor.col = (col.max(1) as u32 - 1).min(cols - 1);
        }
        StatusLineCsiAction::EraseDisplay => {
            status.row.clear(status.fg, status.bg);
        }
        StatusLineCsiAction::EraseInLine { mode } => {
            let col = cursor.col as usize;
            let len = cols as usize;
            match mode {
                0 => status.row.clear_range(col..len, status.fg, status.bg),
                1 => status.row.clear_range(0..(col + 1), status.fg, status.bg),
                2 => status.row.clear(status.fg, status.bg),
                _ => {}
            }
        }
        StatusLineCsiAction::DeleteChars { count } => {
            status_delete_chars(status, count as usize);
        }
        StatusLineCsiAction::EraseChars { count } => {
            status_erase_chars(status, count as usize);
        }
        StatusLineCsiAction::RepeatLastChar { count } => {
            if let Some(last) = status.last_char.clone() {
                for _ in 0..count {
                    put_status_char(screen, last.clone(), insert_mode);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusDisplayKind;
    use crate::TerminalModes;
    use crate::parser::ParsedCsiAction;
    use crate::parser::test_support::*;
    use crate::screen;
    use crate::screen::ActiveDisplay;

    #[test]
    fn csi_parse_uses_status_line_context_for_plain_actions() {
        let (mut screen, _) = setup();
        screen::set_status_display(
            &mut screen,
            TEST_COLS,
            StatusDisplayKind::HostWritable,
            color::default_fg(),
            color::default_bg(),
        );
        screen.active_display = ActiveDisplay::Status;
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_csi_action_with(b"\x1b[31m", &screen, &modes),
            ParsedCsiAction::StatusLine(StatusLineCsiAction::SetGraphicsRendition { .. })
        ));
    }
}

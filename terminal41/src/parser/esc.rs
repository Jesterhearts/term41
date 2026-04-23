use smol_str::SmolStr;
use vte_mode41::C1Mode;
use vte_mode41::ConformanceLevel;
use vte_mode41::TextMode;

use crate::CursorStyle;
use crate::DecColorState;
use crate::KittyKeyboardState;
use crate::LineAttr;
use crate::Screen;
use crate::ShellIntegrationPhase;
use crate::StatusDisplayKind;
use crate::TerminalModes;
use crate::Viewport;
use crate::charset;
use crate::charset::CharacterSet;
use crate::charset::GraphicSetSlot;
use crate::color;
use crate::dec::r#macro::MacroStore;
use crate::dec::udk::UdkState;
use crate::drcs::DrcsStore;
use crate::mode;
use crate::parser::ParsedEscAction;
use crate::parser::Vt52EscAction;
use crate::parser::apply_hard_reset_state;
use crate::parser::can_negotiate_c1;
use crate::parser::clamp_cursor_to_row_width;
use crate::parser::row_display_cols;
use crate::screen;
use crate::screen::grid;

fn parse_vt52_esc(byte: u8) -> Option<ParsedEscAction> {
    let action = match byte {
        b'A' => Vt52EscAction::CursorUp,
        b'B' => Vt52EscAction::CursorDown,
        b'C' => Vt52EscAction::CursorRight,
        b'D' => Vt52EscAction::CursorLeft,
        b'F' => Vt52EscAction::EnterDecSpecialGraphics,
        b'G' => Vt52EscAction::ExitDecSpecialGraphics,
        b'H' => Vt52EscAction::CursorHome,
        b'I' => Vt52EscAction::ReverseIndex,
        b'J' => Vt52EscAction::EraseToEndOfScreen,
        b'K' => Vt52EscAction::EraseToEndOfLine,
        b'Y' => Vt52EscAction::DirectCursorAddressStart,
        b'Z' => Vt52EscAction::Identify,
        b'<' => Vt52EscAction::ExitVt52Mode,
        _ => return None,
    };
    Some(ParsedEscAction::Vt52(action))
}

fn parse_space_esc(
    modes: &TerminalModes,
    byte: u8,
) -> ParsedEscAction {
    match byte {
        b'F' if can_negotiate_c1(modes) => ParsedEscAction::UseSevenBitC1Controls,
        b'G' if can_negotiate_c1(modes) => ParsedEscAction::UseEightBitC1Controls,
        _ => ParsedEscAction::Unsupported,
    }
}

fn parse_percent_esc(byte: u8) -> ParsedEscAction {
    match TextMode::from_docs_final(byte) {
        Some(TextMode::EightBit) => ParsedEscAction::UseEightBitText,
        Some(TextMode::Utf8) => ParsedEscAction::UseUtf8Text,
        None => ParsedEscAction::Unsupported,
    }
}

fn parse_hash_esc(byte: u8) -> ParsedEscAction {
    match byte {
        b'8' => ParsedEscAction::ScreenAlignmentTest,
        b'3' => ParsedEscAction::SetDoubleHeightTopLine,
        b'4' => ParsedEscAction::SetDoubleHeightBottomLine,
        b'5' => ParsedEscAction::SetSingleWidthLine,
        b'6' => ParsedEscAction::SetDoubleWidthLine,
        _ => ParsedEscAction::Unsupported,
    }
}

fn parse_plain_esc(byte: u8) -> ParsedEscAction {
    match byte {
        b'c' => ParsedEscAction::HardReset,
        b'7' => ParsedEscAction::SaveCursor,
        b'8' => ParsedEscAction::RestoreCursor,
        b'D' => ParsedEscAction::Index,
        b'E' => ParsedEscAction::NextLine,
        b'H' => ParsedEscAction::SetTabStop,
        b'M' => ParsedEscAction::ReverseIndex,
        b'=' => ParsedEscAction::EnableApplicationKeypad,
        b'>' => ParsedEscAction::DisableApplicationKeypad,
        b'N' => ParsedEscAction::SingleShiftG2,
        b'O' => ParsedEscAction::SingleShiftG3,
        b'n' => ParsedEscAction::LockingShiftG2ToGl,
        b'o' => ParsedEscAction::LockingShiftG3ToGl,
        b'~' => ParsedEscAction::LockingShiftG1ToGr,
        b'}' => ParsedEscAction::LockingShiftG2ToGr,
        b'|' => ParsedEscAction::LockingShiftG3ToGr,
        b'6' => ParsedEscAction::BackIndex,
        b'9' => ParsedEscAction::ForwardIndex,
        _ => ParsedEscAction::Unsupported,
    }
}

fn parse_charset_esc(
    drcs: &DrcsStore,
    intermediates: &[u8],
    byte: u8,
) -> ParsedEscAction {
    if let Some((slot, charset)) = charset::parse_designation(intermediates, byte) {
        return ParsedEscAction::DesignateCharset { slot, charset };
    }

    let Some(slot) = charset::slot_for_intermediates(intermediates) else {
        return ParsedEscAction::Unsupported;
    };
    let Some(charset) = drcs.lookup_designation(intermediates, byte) else {
        return ParsedEscAction::Unsupported;
    };
    ParsedEscAction::DesignateCharset { slot, charset }
}

pub(crate) fn esc_parse(
    modes: &TerminalModes,
    drcs: &DrcsStore,
    intermediates: &[u8],
    byte: u8,
) -> ParsedEscAction {
    if intermediates.is_empty()
        && modes.vt52_mode
        && let Some(action) = parse_vt52_esc(byte)
    {
        return action;
    }

    match intermediates {
        b" " => parse_space_esc(modes, byte),
        b"%" => parse_percent_esc(byte),
        b"#" => parse_hash_esc(byte),
        b"" => parse_plain_esc(byte),
        _ => parse_charset_esc(drcs, intermediates, byte),
    }
}

fn apply_vt52_esc(
    action: Vt52EscAction,
    screen: &mut Screen,
    viewport: &Viewport,
    modes: &mut TerminalModes,
    pending_output: &mut Vec<u8>,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
) {
    clamp_cursor_to_row_width(screen, viewport);
    match action {
        Vt52EscAction::CursorUp => {
            screen.cursor.row = screen.cursor.row.saturating_sub(1);
            clamp_cursor_to_row_width(screen, viewport);
        }
        Vt52EscAction::CursorDown if screen.cursor.row + 1 < viewport.rows => {
            screen.cursor.row += 1;
            clamp_cursor_to_row_width(screen, viewport);
        }
        Vt52EscAction::CursorDown => {}
        Vt52EscAction::CursorRight
            if screen.cursor.col + 1 < row_display_cols(screen, viewport, screen.cursor.row) =>
        {
            screen.cursor.col += 1;
        }
        Vt52EscAction::CursorRight => {}
        Vt52EscAction::CursorLeft => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        Vt52EscAction::EnterDecSpecialGraphics => {
            screen
                .charset
                .designate(GraphicSetSlot::G0, CharacterSet::DecSpecialGraphics);
        }
        Vt52EscAction::ExitDecSpecialGraphics => {
            screen
                .charset
                .designate(GraphicSetSlot::G0, CharacterSet::Ascii);
        }
        Vt52EscAction::CursorHome => {
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        Vt52EscAction::ReverseIndex => {
            if screen.cursor.row == screen.scroll_top {
                grid::scroll_down_in_region(
                    &mut screen.grid,
                    viewport,
                    &mut screen.images,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    1,
                );
            } else if screen.cursor.row > 0 {
                screen.cursor.row -= 1;
            }
        }
        Vt52EscAction::EraseToEndOfScreen => {
            grid::erase_in_display(
                &mut screen.grid,
                &screen.cursor,
                viewport,
                &mut screen.images,
                0,
            );
        }
        Vt52EscAction::EraseToEndOfLine => {
            grid::erase_in_line(&mut screen.grid, &screen.cursor, viewport, 0);
        }
        Vt52EscAction::DirectCursorAddressStart => {
            *vt52_cursor_addr = crate::Vt52CursorAddr::AwaitingRow;
        }
        Vt52EscAction::Identify => {
            pending_output.extend_from_slice(b"\x1b/Z");
        }
        Vt52EscAction::ExitVt52Mode => {
            modes.vt52_mode = false;
        }
    }
}

fn apply_screen_alignment_test(
    screen: &mut Screen,
    viewport: &Viewport,
    palette: &color::ColorPalette,
) {
    let first_visible = screen
        .grid
        .rows
        .len()
        .saturating_sub(viewport.rows as usize);
    let e_cell = SmolStr::new_inline("E");
    let fg = palette.fg;
    let bg = palette.bg;
    for r in first_visible..screen.grid.rows.len() {
        let row = &mut screen.grid.rows[r];
        row.clear(fg, bg);
        row.wrapped = false;
        row.line_attr = LineAttr::Normal;
        for cell in row.cells.iter_mut() {
            *cell = e_cell.clone();
        }
        row.fg.fill(fg);
        row.bg.fill(bg);
    }
    screen.scroll_top = 0;
    screen.scroll_bottom = viewport.rows.saturating_sub(1);
    screen.left_margin = 0;
    screen.right_margin = viewport.cols.saturating_sub(1);
    screen.origin_mode = false;
    screen.cursor.row = 0;
    screen.cursor.col = 0;
}

fn apply_esc_line_attr(
    screen: &mut Screen,
    viewport: &Viewport,
    line_attr: LineAttr,
) {
    let visible_start = screen
        .grid
        .rows
        .len()
        .saturating_sub(viewport.rows as usize);
    let abs_row = visible_start + screen.cursor.row as usize;
    if let Some(row) = screen.grid.rows.get_mut(abs_row) {
        row.line_attr = line_attr;
        if screen.cursor.row as usize + visible_start == abs_row {
            clamp_cursor_to_row_width(screen, viewport);
        }
    }
}

fn apply_esc_index(
    screen: &mut Screen,
    screen_view: &Viewport,
    preserve_top_origin_scrollback: bool,
) {
    if screen.cursor.row == screen.scroll_bottom {
        if screen.scroll_top == 0 && screen.scroll_bottom == screen_view.rows - 1 {
            if screen::page_can_scroll_down(screen, screen_view) {
                screen::scroll_page_down(screen, screen_view, 1);
            } else {
                screen.grid.push_visible_row(screen_view);
            }
        } else {
            grid::scroll_up_in_region_with_scrollback_policy(
                &mut screen.grid,
                screen_view,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
                preserve_top_origin_scrollback,
            );
        }
    } else if screen.cursor.row < screen_view.rows - 1 {
        screen.cursor.row += 1;
        clamp_cursor_to_row_width(screen, screen_view);
    }
}

fn apply_esc_next_line(
    screen: &mut Screen,
    screen_view: &Viewport,
    preserve_top_origin_scrollback: bool,
) {
    screen.cursor.col = 0;
    if screen.cursor.row == screen.scroll_bottom {
        if screen.scroll_top == 0 && screen.scroll_bottom == screen_view.rows - 1 {
            if screen::page_can_scroll_down(screen, screen_view) {
                screen::scroll_page_down(screen, screen_view, 1);
            } else {
                screen.grid.push_visible_row(screen_view);
            }
        } else {
            grid::scroll_up_in_region_with_scrollback_policy(
                &mut screen.grid,
                screen_view,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
                preserve_top_origin_scrollback,
            );
        }
    } else if screen.cursor.row < screen_view.rows - 1 {
        screen.cursor.row += 1;
    }
    clamp_cursor_to_row_width(screen, screen_view);
}

fn apply_esc_reverse_index(
    screen: &mut Screen,
    screen_view: &Viewport,
) {
    if screen.cursor.row == screen.scroll_top {
        grid::scroll_down_in_region(
            &mut screen.grid,
            screen_view,
            &mut screen.images,
            screen.scroll_top,
            screen.scroll_bottom,
            1,
        );
    } else if screen.cursor.row > 0 {
        screen.cursor.row -= 1;
    }
}

fn apply_esc_back_index(
    screen: &mut Screen,
    screen_view: &Viewport,
) {
    if screen.cursor.col == 0 {
        grid::scroll_right(
            &mut screen.grid,
            screen_view,
            screen.scroll_top,
            screen.scroll_bottom,
            1,
        );
    } else {
        screen.cursor.col = screen.cursor.col.saturating_sub(1);
    }
}

fn apply_esc_forward_index(
    screen: &mut Screen,
    screen_view: &Viewport,
) {
    if screen.cursor.col >= row_display_cols(screen, screen_view, screen.cursor.row) - 1 {
        grid::scroll_left(
            &mut screen.grid,
            screen_view,
            screen.scroll_top,
            screen.scroll_bottom,
            1,
        );
    } else {
        screen.cursor.col += 1;
    }
}

#[bon::builder]
pub(crate) fn esc_apply(
    action: ParsedEscAction,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    cursor_style: &mut CursorStyle,
    current_title: &mut Option<String>,
    title_stack: &mut Vec<Option<String>>,
    saved_modes: &mut std::collections::HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    bell_pending: &mut bool,
    palette: &mut color::ColorPalette,
    base_palette: &color::ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    pending_output: &mut Vec<u8>,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    udks: &mut UdkState,
    drcs: &mut DrcsStore,
) {
    match action {
        ParsedEscAction::Unsupported => {}
        ParsedEscAction::Vt52(action) => {
            apply_vt52_esc(
                action,
                screen,
                viewport,
                modes,
                pending_output,
                vt52_cursor_addr,
            );
        }
        ParsedEscAction::UseSevenBitC1Controls => {
            modes.c1_mode = C1Mode::SevenBit;
        }
        ParsedEscAction::UseEightBitC1Controls => {
            modes.c1_mode = C1Mode::EightBit;
        }
        ParsedEscAction::UseEightBitText => {
            modes.text_mode = TextMode::EightBit;
        }
        ParsedEscAction::UseUtf8Text => {
            modes.text_mode = TextMode::Utf8;
        }
        ParsedEscAction::DesignateCharset { slot, charset } => {
            screen.charset.designate(slot, charset);
        }
        ParsedEscAction::ScreenAlignmentTest => {
            apply_screen_alignment_test(screen, viewport, palette);
        }
        ParsedEscAction::SetDoubleHeightTopLine => {
            apply_esc_line_attr(screen, viewport, LineAttr::DoubleHeightTop);
        }
        ParsedEscAction::SetDoubleHeightBottomLine => {
            apply_esc_line_attr(screen, viewport, LineAttr::DoubleHeightBottom);
        }
        ParsedEscAction::SetSingleWidthLine => {
            apply_esc_line_attr(screen, viewport, LineAttr::Normal);
        }
        ParsedEscAction::SetDoubleWidthLine => {
            apply_esc_line_attr(screen, viewport, LineAttr::DoubleWidth);
        }
        ParsedEscAction::HardReset => {
            let conformance_level = ConformanceLevel::Level4;
            let c1_mode = C1Mode::SevenBit;
            apply_hard_reset_state()
                .screen(screen)
                .stash(stash)
                .on_alt_screen(on_alt_screen)
                .modes(modes)
                .viewport(viewport)
                .kitty_keyboard(kitty_keyboard)
                .cursor_style(cursor_style)
                .current_title(current_title)
                .title_stack(title_stack)
                .saved_modes(saved_modes)
                .current_prompt_row(current_prompt_row)
                .shell_integration_phase(shell_integration_phase)
                .bell_pending(bell_pending)
                .vt52_cursor_addr(vt52_cursor_addr)
                .palette(palette)
                .base_palette(base_palette)
                .dec_color(dec_color)
                .default_status_display(default_status_display)
                .macros(macros)
                .udks(udks)
                .drcs(drcs)
                .conformance_level(conformance_level)
                .c1_mode(c1_mode)
                .call();
        }
        ParsedEscAction::SaveCursor => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            screen::save_cursor_slot(screen);
            clamp_cursor_to_row_width(screen, &screen_view);
        }
        ParsedEscAction::RestoreCursor => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            screen::restore_cursor_slot(screen, &screen_view);
        }
        ParsedEscAction::Index => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            apply_esc_index(
                screen,
                &screen_view,
                !*on_alt_screen && !screen::page_memory_active(screen),
            );
        }
        ParsedEscAction::NextLine => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            apply_esc_next_line(
                screen,
                &screen_view,
                !*on_alt_screen && !screen::page_memory_active(screen),
            );
        }
        ParsedEscAction::SetTabStop => {
            clamp_cursor_to_row_width(screen, viewport);
            let col = screen.cursor.col as usize;
            if col < screen.tab_stops.len() {
                screen.tab_stops[col] = true;
            }
        }
        ParsedEscAction::ReverseIndex => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            apply_esc_reverse_index(screen, &screen_view);
        }
        ParsedEscAction::EnableApplicationKeypad => {
            screen.app_keypad = true;
        }
        ParsedEscAction::DisableApplicationKeypad => {
            screen.app_keypad = false;
        }
        ParsedEscAction::SingleShiftG2 => {
            screen.charset.single_shift = Some(GraphicSetSlot::G2);
        }
        ParsedEscAction::SingleShiftG3 => {
            screen.charset.single_shift = Some(GraphicSetSlot::G3);
        }
        ParsedEscAction::LockingShiftG2ToGl => {
            screen.charset.set_gl(GraphicSetSlot::G2);
        }
        ParsedEscAction::LockingShiftG3ToGl => {
            screen.charset.set_gl(GraphicSetSlot::G3);
        }
        ParsedEscAction::LockingShiftG1ToGr => {
            screen.charset.set_gr(GraphicSetSlot::G1);
        }
        ParsedEscAction::LockingShiftG2ToGr => {
            screen.charset.set_gr(GraphicSetSlot::G2);
        }
        ParsedEscAction::LockingShiftG3ToGr => {
            screen.charset.set_gr(GraphicSetSlot::G3);
        }
        ParsedEscAction::BackIndex => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            apply_esc_back_index(screen, &screen_view);
        }
        ParsedEscAction::ForwardIndex => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            apply_esc_forward_index(screen, &screen_view);
        }
    }
}

#[cfg(test)]
#[bon::builder]
pub(crate) fn esc_dispatch(
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    cursor_style: &mut CursorStyle,
    current_title: &mut Option<String>,
    title_stack: &mut Vec<Option<String>>,
    saved_modes: &mut std::collections::HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    bell_pending: &mut bool,
    palette: &mut color::ColorPalette,
    base_palette: &color::ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    pending_output: &mut Vec<u8>,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    udks: &mut UdkState,
    drcs: &mut DrcsStore,
    intermediates: &[u8],
    byte: u8,
) {
    let action = esc_parse(modes, drcs, intermediates, byte);
    esc_apply()
        .action(action)
        .screen(screen)
        .stash(stash)
        .viewport(viewport)
        .on_alt_screen(on_alt_screen)
        .modes(modes)
        .kitty_keyboard(kitty_keyboard)
        .cursor_style(cursor_style)
        .current_title(current_title)
        .title_stack(title_stack)
        .saved_modes(saved_modes)
        .current_prompt_row(current_prompt_row)
        .shell_integration_phase(shell_integration_phase)
        .bell_pending(bell_pending)
        .palette(palette)
        .base_palette(base_palette)
        .dec_color(dec_color)
        .default_status_display(default_status_display)
        .pending_output(pending_output)
        .vt52_cursor_addr(vt52_cursor_addr)
        .macros(macros)
        .udks(udks)
        .drcs(drcs)
        .call();
}

#[cfg(test)]
mod tests {
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::FeaturePermissions;
    use crate::dec::color::effective_palette;
    use crate::dec_color_state_from_palette;
    use crate::parser::csi_dispatch;
    use crate::parser::execute;
    use crate::parser::put_8bit_byte;
    use crate::parser::put_ascii_run;
    use crate::parser::put_printable;
    use crate::parser::put_text_run;
    use crate::parser::test_support::*;

    fn set_cursor_col(
        screen: &mut Screen,
        col: u32,
    ) {
        screen.cursor.col = col;
    }

    #[test]
    fn esc_parse_maps_hard_reset_semantically() {
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_esc_action(b"\x1bc", &modes),
            ParsedEscAction::HardReset
        ));
    }

    #[test]
    fn esc_parse_maps_hash_intermediate_semantically() {
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_esc_action(b"\x1b#8", &modes),
            ParsedEscAction::ScreenAlignmentTest
        ));
    }

    #[test]
    fn esc_parse_maps_charset_designation_semantically() {
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_esc_action(b"\x1b(0", &modes),
            ParsedEscAction::DesignateCharset {
                slot: GraphicSetSlot::G0,
                charset: CharacterSet::DecSpecialGraphics
            }
        ));
    }

    #[test]
    fn esc_parse_resolves_soft_charset_designations_semantically() {
        let modes = TerminalModes::new();
        let mut drcs = DrcsStore::default();
        drcs.define(&[0, 0, 0, 0, 0, 0, 0, 0], b"@?");
        assert!(matches!(
            parse_esc_action_with(b"\x1b(@", &modes, &drcs),
            ParsedEscAction::DesignateCharset {
                slot: GraphicSetSlot::G0,
                charset: CharacterSet::Drcs(0, crate::drcs::CharsetSize::Cs94)
            }
        ));
    }

    #[test]
    fn esc_parse_maps_vt52_sequences_using_mode_state() {
        let mut modes = TerminalModes::new();
        modes.vt52_mode = true;
        assert!(matches!(
            parse_esc_action(b"\x1bH", &modes),
            ParsedEscAction::Vt52(Vt52EscAction::CursorHome)
        ));
    }

    // -- esc_dispatch ------------------------------------------------------

    #[test]
    fn esc_m_at_scroll_top_scrolls_down() {
        let (mut screen, mut viewport) = setup();
        feed(b"top\nmid\nbot", &mut screen, &mut viewport);
        // Cursor is at scroll_top (row 0) after moving back there.
        feed(b"\x1b[H", &mut screen, &mut viewport);
        feed(b"\x1bM", &mut screen, &mut viewport);
        // After scroll-down, the old top row shifts down one and row 0 blanks.
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim_end(), "top");
    }

    #[test]
    fn esc_m_above_scroll_top_moves_cursor_up() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1bM", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn esc_m_at_row_zero_outside_region_is_noop() {
        // scroll_top defaults to 0, so row 0 triggers scroll_down_in_region
        // above. Force a non-zero scroll_top to exercise the cursor.row > 0
        // branch at exactly row 0 of the viewport.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;4r", &mut screen, &mut viewport); // scroll_top = 1
        screen.cursor.row = 0;
        feed(b"\x1bM", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn esc_scs_designator_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        set_cursor_col(&mut screen, 3);
        // ESC ( B designates US-ASCII as G0. Parser should no-op without
        // dropping state or panicking on the `B` byte (which would otherwise
        // land in the unknown-byte arm).
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn esc_keypad_modes_set_app_keypad() {
        let (mut screen, mut viewport) = setup();
        assert!(!screen.app_keypad);
        feed(b"\x1b=", &mut screen, &mut viewport);
        assert!(screen.app_keypad);
        feed(b"\x1b>", &mut screen, &mut viewport);
        assert!(!screen.app_keypad);
        // Cursor must not be affected.
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- DEC Special Graphics (SCS) ------------------------------------------

    #[test]
    fn scs_g0_drawing_translates_box_chars() {
        let (mut screen, mut viewport) = setup();
        // ESC ( 0 designates DEC drawing into G0, then print box-drawing bytes.
        // 0x6C = ┌, 0x71 = ─, 0x6B = ┐
        feed(b"\x1b(0\x6c\x71\x6b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{2500}"); // ─
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "\u{2510}"); // ┐
    }

    #[test]
    fn scs_g0_ascii_restores_normal() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing, write a box char, then switch back to ASCII.
        feed(b"\x1b(0\x6c\x1b(B\x6c", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "l"); // plain ASCII
    }

    #[test]
    fn scs_drawing_does_not_translate_below_0x60() {
        let (mut screen, mut viewport) = setup();
        // In drawing mode, bytes below 0x60 should pass through as ASCII.
        feed(b"\x1b(0ABC", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "B");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "C");
    }

    #[test]
    fn scs_so_si_switch_between_g0_g1() {
        let (mut screen, mut viewport) = setup();
        // G0 = ASCII (default), G1 = drawing.
        // SO (0x0E) invokes G1, SI (0x0F) invokes G0.
        feed(b"\x1b)0", &mut screen, &mut viewport); // G1 = drawing
        feed(b"\x0E", &mut screen, &mut viewport); // SO → GL = G1
        feed(b"\x6c", &mut screen, &mut viewport); // should translate
        feed(b"\x0F", &mut screen, &mut viewport); // SI → GL = G0
        feed(b"\x6c", &mut screen, &mut viewport); // should be plain ASCII
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "l"); // plain
    }

    #[test]
    fn scs_decstr_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
        // DECSTR should reset charset state.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::Ascii
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G0);
    }

    #[test]
    fn scs_ris_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0\x1b)0\x0E", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::DecSpecialGraphics
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G1);
        // RIS should reset everything.
        feed(b"\x1bc", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::Ascii
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G0);
    }

    #[test]
    fn scs_save_restore_cursor_preserves_charset() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing in G0, save cursor.
        feed(b"\x1b(0\x1b7", &mut screen, &mut viewport);
        // Switch back to ASCII.
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        // Restore cursor — should bring back DEC drawing.
        feed(b"\x1b8", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
    }

    #[test]
    fn scs_technical_charset_translates_math_symbols() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b)>", &mut screen, &mut viewport); // G1 = DEC Technical
        feed(b"\x0Eabc", &mut screen, &mut viewport); // SO -> GL = G1
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{03B2}");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "\u{03C7}");
    }

    #[test]
    fn scs_ls2_maps_g2_into_gl() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.A", &mut screen, &mut viewport); // G2 = ISO Latin-1 supplemental
        feed(b"\x1bn!!", &mut screen, &mut viewport); // LS2 -> GL = G2
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{00A1}");
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G2);
    }

    #[test]
    fn scs_single_shift_uses_g2_for_one_character() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.A\x1bN!!", &mut screen, &mut viewport); // G2 = ISO Latin-1 supplemental
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "!");
    }

    #[test]
    fn scs_ls1r_maps_g1_into_gr_for_utf8_text() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b)>\x1b~", &mut screen, &mut viewport); // G1 = DEC Technical, GR = G1
        feed("á".as_bytes(), &mut screen, &mut viewport); // U+00E1 -> 0x61 in GR
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.charset.gr_slot(), GraphicSetSlot::G1);
    }

    #[test]
    fn scs_ls2r_maps_g2_into_gr_for_utf8_text() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.%5\x1b}", &mut screen, &mut viewport); // G2 = DEC Supplemental, GR = G2
        feed("¨".as_bytes(), &mut screen, &mut viewport); // U+00A8 -> DEC MCS currency sign
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A4}");
        assert_eq!(screen.charset.gr_slot(), GraphicSetSlot::G2);
    }

    #[test]
    fn docs_8bit_mode_routes_raw_high_bytes_through_gr() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b%@\x1b)>\x1b~\xe1A", &mut screen, &mut viewport); // raw 0xE1 -> 0x61 in GR
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "A");
    }

    #[test]
    fn scs_gr_translation_applies_to_split_utf8_codepoint() {
        let (mut screen, mut viewport) = setup();
        let base_pal = color::ColorPalette::default();
        let mut dec_color = dec_color_state_from_palette(&base_pal);
        let mut pal = effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut shell_integration_phase = ShellIntegrationPhase::None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for chunk in [b"\x1b)>\x1b~\xc3".as_slice(), b"\xa1".as_slice()] {
            for action in parser.parse(chunk) {
                match action {
                    Action::PrintAscii(run) => {
                        put_ascii_run(&mut screen, &viewport, run, modes.insert_mode)
                    }
                    Action::PrintText(run) => {
                        put_text_run(&mut screen, &viewport, run, modes.insert_mode)
                    }
                    Action::Print(s) => put_printable(&mut screen, &viewport, s, modes.insert_mode),
                    Action::Print8Bit(byte) => {
                        put_8bit_byte(&mut screen, &viewport, byte, modes.insert_mode)
                    }
                    Action::Execute(b) => execute(
                        &mut screen,
                        &viewport,
                        b,
                        &mut bell_pending,
                        modes.newline_mode,
                    ),
                    Action::CsiDispatch {
                        params,
                        intermediates,
                        action,
                    } => {
                        csi_dispatch()
                            .screen(&mut screen)
                            .stash(&mut stash)
                            .viewport(&mut viewport)
                            .on_alt_screen(&mut on_alt_screen)
                            .modes(&mut modes)
                            .kitty_keyboard(&mut kitty_keyboard)
                            .pending_output(&mut pending_output)
                            .pending_resize(&mut pending_resize)
                            .cursor_style(&mut cursor_style)
                            .cell_width(8)
                            .cell_height(16)
                            .palette(&mut pal)
                            .base_palette(&base_pal)
                            .dec_color(&mut dec_color)
                            .default_status_display(&mut default_status_display)
                            .title_stack(&mut title_stack)
                            .current_title(&mut current_title)
                            .saved_modes(&mut saved_modes)
                            .current_prompt_row(&mut current_prompt_row)
                            .bell_pending(&mut bell_pending)
                            .vt52_cursor_addr(&mut vt52_cursor_addr)
                            .macros(&mut macros)
                            .drcs(&mut drcs)
                            .params(&params)
                            .intermediates(intermediates.as_slice())
                            .action(action)
                            .feature_permissions(&feature_permissions)
                            .call();
                    }
                    Action::EscDispatch {
                        intermediates,
                        byte,
                    } => {
                        esc_dispatch()
                            .screen(&mut screen)
                            .stash(&mut stash)
                            .viewport(&mut viewport)
                            .on_alt_screen(&mut on_alt_screen)
                            .modes(&mut modes)
                            .kitty_keyboard(&mut kitty_keyboard)
                            .cursor_style(&mut cursor_style)
                            .current_title(&mut current_title)
                            .title_stack(&mut title_stack)
                            .saved_modes(&mut saved_modes)
                            .current_prompt_row(&mut current_prompt_row)
                            .shell_integration_phase(&mut shell_integration_phase)
                            .bell_pending(&mut bell_pending)
                            .palette(&mut pal)
                            .base_palette(&base_pal)
                            .dec_color(&mut dec_color)
                            .default_status_display(&mut default_status_display)
                            .pending_output(&mut pending_output)
                            .vt52_cursor_addr(&mut vt52_cursor_addr)
                            .macros(&mut macros)
                            .drcs(&mut drcs)
                            .intermediates(intermediates.as_slice())
                            .byte(byte)
                            .call();
                    }
                    _ => {}
                }
            }
        }

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
    }

    #[test]
    fn scs_decnrcm_gates_nrc_translation() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(A#", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "#");

        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?42h\x1b(A#", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A3}");
    }

    #[test]
    fn decrqupss_reports_default_upss() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[&u", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1bP0!u%5\x1b\\");
    }

    #[test]
    fn scs_full_box_top_bottom() {
        // Simulate a typical box-drawing sequence: ┌──┐ on top, └──┘ on bottom.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0", &mut screen, &mut viewport);
        feed(b"\x6c\x71\x71\x6b", &mut screen, &mut viewport); // ┌──┐
        feed(b"\r\n", &mut screen, &mut viewport);
        feed(b"\x6d\x71\x71\x6a", &mut screen, &mut viewport); // └──┘
        let top = row_text(&screen, &viewport, 0);
        assert!(top.starts_with("\u{250C}\u{2500}\u{2500}\u{2510}"));
        let bot = row_text(&screen, &viewport, 1);
        assert!(bot.starts_with("\u{2514}\u{2500}\u{2500}\u{2518}"));
    }

    // -- DECALN (ESC # 8) ---------------------------------------------------

    #[test]
    fn decaln_fills_screen_with_e() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello", &mut screen, &mut viewport);
        feed(b"\x1b#8", &mut screen, &mut viewport);
        let text = row_text(&screen, &viewport, 0);
        assert!(text.chars().all(|c| c == 'E'));
        let text2 = row_text(&screen, &viewport, TEST_ROWS - 1);
        assert!(text2.chars().all(|c| c == 'E'));
    }

    // -- IND (ESC D) and NEL (ESC E) ----------------------------------------

    #[test]
    fn ind_moves_cursor_down() {
        let (mut screen, mut viewport) = setup();
        set_cursor_col(&mut screen, 5);
        feed(b"\x1bD", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 5); // col preserved
    }

    #[test]
    fn ind_at_scroll_bottom_scrolls_up() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();
        feed(b"\x1bD", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert!(screen.grid.rows.len() > rows_before);
    }

    #[test]
    fn nel_moves_to_col_0_of_next_line() {
        let (mut screen, mut viewport) = setup();
        set_cursor_col(&mut screen, 5);
        feed(b"\x1bE", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
    }

    /// Enter VT52 then exit via `ESC <`; DECRQM should see ANSI mode restored.
    #[test]
    fn vt52_enter_and_exit_via_esc_lt() {
        let (mut screen, mut viewport) = setup();
        // `CSI ? 2 l` → VT52; `ESC <` → back to ANSI; DECRQM → set.
        let out = feed_with_output(b"\x1b[?2l\x1b<\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// VT52 ESC A/B/C/D cursor movement.
    #[test]
    fn vt52_cursor_up() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 2, col 3 (1-based: 3;4), then VT52 ESC A.
        feed(b"\x1b[3;4H\x1b[?2l\x1bA", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (1, 3));
    }

    #[test]
    fn vt52_cursor_down() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 1, col 0, then VT52 ESC B.
        feed(b"\x1b[2;1H\x1b[?2l\x1bB", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 0));
    }

    #[test]
    fn vt52_cursor_right() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;3H\x1b[?2l\x1bC", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    }

    #[test]
    fn vt52_cursor_left() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;5H\x1b[?2l\x1bD", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    }

    /// VT52 cursor up at row 0 does not underflow.
    #[test]
    fn vt52_cursor_up_clamps_at_top() {
        let (mut screen, mut viewport) = setup();
        // Already at row 0 (home). VT52 mode, ESC A.
        feed(b"\x1b[?2l\x1bA", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    /// VT52 ESC H homes the cursor.
    #[test]
    fn vt52_cursor_home() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 3, col 6 (1-based), then VT52 ESC H.
        feed(b"\x1b[3;6H\x1b[?2l\x1bH", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 0));
    }

    /// VT52 ESC Y <row+0x20> <col+0x20> direct cursor address — bytes split.
    #[test]
    fn vt52_direct_cursor_address() {
        let (mut screen, mut viewport) = setup();
        // Enter VT52 then ESC Y: row 2 ('"'=0x22), col 4 ('$'=0x24).
        feed(b"\x1b[?2l\x1bY\"$", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 4));
    }

    /// VT52 ESC Y where both position bytes arrive in the same PrintAscii run.
    #[test]
    fn vt52_direct_cursor_address_batched() {
        let (mut screen, mut viewport) = setup();
        // Row 1 ('!'=0x21), col 3 ('#'=0x23).
        feed(b"\x1b[?2l\x1bY!#", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (1, 3));
    }

    /// Text after ESC Y position bytes is printed normally.
    #[test]
    fn vt52_direct_cursor_address_then_text() {
        let (mut screen, mut viewport) = setup();
        // Row 0, col 0 (both 0x20 = space), then 'A'.
        feed(b"\x1b[?2l\x1bY  A", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 1));
        assert_eq!(&row_text(&screen, &viewport, 0)[..1], "A");
    }

    /// VT52 ESC J erases from cursor to end of screen (same as ED 0).
    #[test]
    fn vt52_erase_to_end_of_screen() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0 with 'a', row 1 with 'b', then enter VT52 at row 0
        // col 5 (via CUP before VT52 entry) and erase.
        feed(
            b"aaaaaaaaaa\r\nbbbbbbbbbb\x1b[1;6H\x1b[?2l\x1bJ",
            &mut screen,
            &mut viewport,
        );
        let r0 = row_text(&screen, &viewport, 0);
        let r1 = row_text(&screen, &viewport, 1);
        assert_eq!(&r0[..5], "aaaaa", "text before cursor preserved");
        assert_eq!(r0[5..].trim(), "", "text from cursor erased");
        assert_eq!(r1.trim(), "", "row 1 cleared");
    }

    /// VT52 ESC K erases from cursor to end of line (same as EL 0).
    #[test]
    fn vt52_erase_to_end_of_line() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0, position at col 3, enter VT52, erase to EOL.
        feed(
            b"aaaaaaaaaa\x1b[1;4H\x1b[?2l\x1bK",
            &mut screen,
            &mut viewport,
        );
        let r0 = row_text(&screen, &viewport, 0);
        assert_eq!(&r0[..3], "aaa");
        assert_eq!(r0[3..].trim(), "");
    }

    /// VT52 ESC F/G toggle DEC Special Graphics on G0 within one parse pass.
    #[test]
    fn vt52_graphics_mode_on() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?2l\x1bF", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
    }

    #[test]
    fn vt52_graphics_mode_off() {
        let (mut screen, mut viewport) = setup();
        // Enable then disable in the same parse pass.
        feed(b"\x1b[?2l\x1bF\x1bG", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
    }

    /// VT52 ESC Z identify returns ESC / Z.
    #[test]
    fn vt52_identify() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?2l\x1bZ", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b/Z");
    }

    /// CSI sequences are silently dropped in VT52 mode.
    #[test]
    fn vt52_csi_suppressed() {
        let (mut screen, mut viewport) = setup();
        // Position cursor at col 5 (1-based col 6), enter VT52, send CSI CUB.
        feed(b"\x1b[1;6H\x1b[?2l\x1b[3D", &mut screen, &mut viewport);
        // CSI cursor-back should have been dropped.
        assert_eq!(screen.cursor.col, 5, "cursor should not move in VT52 mode");
    }

    /// VT52 reverse index (ESC I) scrolls down at the top of the scroll region.
    #[test]
    fn vt52_reverse_index_scrolls() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0 with text, CUP to row 0, enter VT52, reverse index.
        feed(
            b"line0\r\nline1\r\nline2\x1b[1;1H\x1b[?2l\x1bI",
            &mut screen,
            &mut viewport,
        );
        // Row 0 should now be blank (scrolled down).
        let r0 = row_text(&screen, &viewport, 0);
        assert_eq!(r0.trim(), "");
    }
}

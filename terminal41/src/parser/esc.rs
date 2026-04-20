use super::*;

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
            if screen.cursor.col + 1 < current_row_display_cols(screen, viewport) =>
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
                screen.grid.scroll_down_in_region(
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
            screen
                .grid
                .erase_in_display(&screen.cursor, viewport, &mut screen.images, 0);
        }
        Vt52EscAction::EraseToEndOfLine => {
            screen.grid.erase_in_line(&screen.cursor, viewport, 0);
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
) {
    if screen.cursor.row == screen.scroll_bottom {
        if screen.scroll_top == 0 && screen.scroll_bottom == screen_view.rows - 1 {
            if screen::page_can_scroll_down(screen, screen_view) {
                screen::scroll_page_down(screen, screen_view, 1);
            } else {
                screen.grid.push_visible_row(screen_view);
            }
        } else {
            screen.grid.scroll_up_in_region(
                screen_view,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
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
            screen.grid.scroll_up_in_region(
                screen_view,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
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
        screen.grid.scroll_down_in_region(
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
        screen
            .grid
            .scroll_right(screen_view, screen.scroll_top, screen.scroll_bottom, 1);
    } else {
        screen.cursor.col -= 1;
    }
}

fn apply_esc_forward_index(
    screen: &mut Screen,
    screen_view: &Viewport,
) {
    if screen.cursor.col >= current_row_display_cols(screen, screen_view) - 1 {
        screen
            .grid
            .scroll_left(screen_view, screen.scroll_top, screen.scroll_bottom, 1);
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
    saved_modes: &mut std::collections::HashMap<u16, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    palette: &mut color::ColorPalette,
    base_palette: &color::ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    pending_output: &mut Vec<u8>,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
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
                .bell_pending(bell_pending)
                .vt52_cursor_addr(vt52_cursor_addr)
                .palette(palette)
                .base_palette(base_palette)
                .dec_color(dec_color)
                .default_status_display(default_status_display)
                .macros(macros)
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
            apply_esc_index(screen, &screen_view);
        }
        ParsedEscAction::NextLine => {
            clamp_cursor_to_row_width(screen, viewport);
            let screen_view = screen::screen_viewport(screen, viewport);
            apply_esc_next_line(screen, &screen_view);
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
    saved_modes: &mut std::collections::HashMap<u16, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    palette: &mut color::ColorPalette,
    base_palette: &color::ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    pending_output: &mut Vec<u8>,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
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
        .bell_pending(bell_pending)
        .palette(palette)
        .base_palette(base_palette)
        .dec_color(dec_color)
        .default_status_display(default_status_display)
        .pending_output(pending_output)
        .vt52_cursor_addr(vt52_cursor_addr)
        .macros(macros)
        .drcs(drcs)
        .call();
}

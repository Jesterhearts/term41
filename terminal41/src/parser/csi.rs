use super::*;

fn first_group_param(
    params: &OwnedParams,
    default: u16,
) -> u16 {
    params
        .get(0)
        .and_then(|group| group.first().copied())
        .unwrap_or(default)
}

fn nth_group_param(
    params: &OwnedParams,
    idx: usize,
    default: u16,
) -> u16 {
    params
        .get(idx)
        .and_then(|group| group.first().copied())
        .unwrap_or(default)
}

fn parse_status_line_plain_csi(
    params: OwnedParams,
    action: char,
) -> ParsedCsiAction {
    match action {
        'm' => ParsedCsiAction::StatusLine(StatusLineCsiAction::SetGraphicsRendition { params }),
        '@' => ParsedCsiAction::StatusLine(StatusLineCsiAction::InsertChars {
            count: first_group_param(&params, 1),
        }),
        'A' | 'B' | 'd' => ParsedCsiAction::StatusLine(StatusLineCsiAction::HomeRow),
        'C' => ParsedCsiAction::StatusLine(StatusLineCsiAction::CursorForward {
            count: first_group_param(&params, 1),
        }),
        'D' => ParsedCsiAction::StatusLine(StatusLineCsiAction::CursorBackward {
            count: first_group_param(&params, 1),
        }),
        'G' | '`' => ParsedCsiAction::StatusLine(StatusLineCsiAction::CursorHorizontalAbsolute {
            col: first_group_param(&params, 1),
        }),
        'H' | 'f' => ParsedCsiAction::StatusLine(StatusLineCsiAction::CursorPosition {
            col: nth_group_param(&params, 1, 1),
        }),
        'J' => ParsedCsiAction::StatusLine(StatusLineCsiAction::EraseDisplay),
        'K' => ParsedCsiAction::StatusLine(StatusLineCsiAction::EraseInLine {
            mode: first_group_param(&params, 0),
        }),
        'P' => ParsedCsiAction::StatusLine(StatusLineCsiAction::DeleteChars {
            count: first_group_param(&params, 1),
        }),
        'X' => ParsedCsiAction::StatusLine(StatusLineCsiAction::EraseChars {
            count: first_group_param(&params, 1),
        }),
        'b' => ParsedCsiAction::StatusLine(StatusLineCsiAction::RepeatLastChar {
            count: first_group_param(&params, 1),
        }),
        _ => ParsedCsiAction::Unsupported,
    }
}

fn parse_main_plain_csi(
    modes: &TerminalModes,
    params: OwnedParams,
    action: char,
) -> ParsedCsiAction {
    match action {
        'y' => {
            let mut groups = params.iter();
            let selector = groups.next().and_then(|g| g.first().copied()).unwrap_or(0);
            if selector != 4 {
                return ParsedCsiAction::Unsupported;
            }
            ParsedCsiAction::Main(MainCsiAction::SelfTest {
                requested_tests: groups.flat_map(|g| g.iter().copied()).collect(),
            })
        }
        'c' => ParsedCsiAction::Main(MainCsiAction::ReportPrimaryDeviceAttrs),
        'n' => ParsedCsiAction::Main(MainCsiAction::DeviceStatusReport {
            selector: first_group_param(&params, 0),
        }),
        't' => {
            let ps = first_group_param(&params, 0);
            if params.iter().count() <= 1 && valid_page_lines(ps).is_some() {
                ParsedCsiAction::Main(MainCsiAction::SetPageLines { lines: ps })
            } else {
                match ps {
                    WINOP_TITLE_PUSH => ParsedCsiAction::Main(MainCsiAction::PushTitle),
                    WINOP_TITLE_POP => ParsedCsiAction::Main(MainCsiAction::PopTitle),
                    WINOP_REPORT_PIXELS => ParsedCsiAction::Main(MainCsiAction::ReportPixelSize),
                    WINOP_REPORT_CELL_SIZE => ParsedCsiAction::Main(MainCsiAction::ReportCellSize),
                    WINOP_REPORT_TEXT_SIZE => ParsedCsiAction::Main(MainCsiAction::ReportTextSize),
                    _ => ParsedCsiAction::Unsupported,
                }
            }
        }
        'b' => ParsedCsiAction::Main(MainCsiAction::RepeatLastChar {
            count: first_group_param(&params, 1),
        }),
        'A' => ParsedCsiAction::Main(MainCsiAction::CursorUp {
            count: first_group_param(&params, 1),
        }),
        'B' => ParsedCsiAction::Main(MainCsiAction::CursorDown {
            count: first_group_param(&params, 1),
        }),
        'C' => ParsedCsiAction::Main(MainCsiAction::CursorForward {
            count: first_group_param(&params, 1),
        }),
        'D' => ParsedCsiAction::Main(MainCsiAction::CursorBackward {
            count: first_group_param(&params, 1),
        }),
        'E' => ParsedCsiAction::Main(MainCsiAction::CursorNextLine {
            count: first_group_param(&params, 1),
        }),
        'F' => ParsedCsiAction::Main(MainCsiAction::CursorPreviousLine {
            count: first_group_param(&params, 1),
        }),
        'H' | 'f' => ParsedCsiAction::Main(MainCsiAction::CursorPosition {
            row: first_group_param(&params, 1),
            col: nth_group_param(&params, 1, 1),
        }),
        'J' => ParsedCsiAction::Main(MainCsiAction::EraseInDisplay {
            mode: first_group_param(&params, 0),
        }),
        'K' => ParsedCsiAction::Main(MainCsiAction::EraseInLine {
            mode: first_group_param(&params, 0),
        }),
        'm' => ParsedCsiAction::Main(MainCsiAction::SetGraphicsRendition { params }),
        'd' => ParsedCsiAction::Main(MainCsiAction::LinePositionAbsolute {
            row: first_group_param(&params, 1),
        }),
        'G' | '`' => ParsedCsiAction::Main(MainCsiAction::CursorHorizontalAbsolute {
            col: first_group_param(&params, 1),
        }),
        'a' => ParsedCsiAction::Main(MainCsiAction::CursorForwardRelative {
            count: first_group_param(&params, 1),
        }),
        'e' => ParsedCsiAction::Main(MainCsiAction::CursorVerticalRelative {
            count: first_group_param(&params, 1),
        }),
        'L' => ParsedCsiAction::Main(MainCsiAction::InsertLines {
            count: first_group_param(&params, 1),
        }),
        'M' => ParsedCsiAction::Main(MainCsiAction::DeleteLines {
            count: first_group_param(&params, 1),
        }),
        'P' => ParsedCsiAction::Main(MainCsiAction::DeleteChars {
            count: first_group_param(&params, 1),
        }),
        '@' => ParsedCsiAction::Main(MainCsiAction::InsertChars {
            count: first_group_param(&params, 1),
        }),
        'X' => ParsedCsiAction::Main(MainCsiAction::EraseChars {
            count: first_group_param(&params, 1),
        }),
        'S' => ParsedCsiAction::Main(MainCsiAction::ScrollUp {
            count: first_group_param(&params, 1),
        }),
        'T' => ParsedCsiAction::Main(MainCsiAction::ScrollDown {
            count: first_group_param(&params, 1),
        }),
        'r' => ParsedCsiAction::Main(MainCsiAction::SetScrollRegion {
            top: first_group_param(&params, 1),
            bottom: params.get(1).and_then(|group| group.first().copied()),
        }),
        's' if modes.declrmm && params.get(0).is_some() => {
            ParsedCsiAction::Main(MainCsiAction::SetLeftRightMargins {
                left: first_group_param(&params, 1),
                right: params.get(1).and_then(|group| group.first().copied()),
            })
        }
        's' => ParsedCsiAction::Main(MainCsiAction::SaveCursor),
        'u' => ParsedCsiAction::Main(MainCsiAction::RestoreCursor),
        'U' => ParsedCsiAction::Main(MainCsiAction::NextPage {
            count: first_group_param(&params, 1),
        }),
        'V' => ParsedCsiAction::Main(MainCsiAction::PrevPage {
            count: first_group_param(&params, 1),
        }),
        'I' => ParsedCsiAction::Main(MainCsiAction::CursorForwardTabulation {
            count: first_group_param(&params, 1),
        }),
        'Z' => ParsedCsiAction::Main(MainCsiAction::CursorBackwardTabulation {
            count: first_group_param(&params, 1),
        }),
        'g' => ParsedCsiAction::Main(MainCsiAction::TabClear {
            mode: first_group_param(&params, 0),
        }),
        'h' => ParsedCsiAction::Main(MainCsiAction::SetAnsiModes {
            enable: true,
            modes: params
                .iter()
                .filter_map(|group| group.first().copied())
                .filter_map(|mode| mode::AnsiMode::try_from(mode).ok())
                .collect(),
        }),
        'l' => ParsedCsiAction::Main(MainCsiAction::SetAnsiModes {
            enable: false,
            modes: params
                .iter()
                .filter_map(|group| group.first().copied())
                .filter_map(|mode| mode::AnsiMode::try_from(mode).ok())
                .collect(),
        }),
        _ => ParsedCsiAction::Unsupported,
    }
}

fn apply_private_mode(
    modes: &mut TerminalModes,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    cursor_style: &mut CursorStyle,
    dec_color: &mut DecColorState,
    mode: mode::PrivateMode,
    enable: bool,
) {
    if mode == mode::PrivateMode::Decanm {
        modes.vt52_mode = !enable;
    } else if mode == mode::PrivateMode::Decscnm {
        modes.screen_reverse = enable;
    } else if mode == mode::PrivateMode::Decarm {
        modes.decarm = enable;
    } else if mode == mode::PrivateMode::Att610Blink {
        cursor_style.blink = enable;
    } else if mode == mode::PrivateMode::Decncsm {
        modes.decncsm = enable;
    } else if mode == mode::PrivateMode::Declrmm {
        modes.declrmm = enable;
        if !enable {
            screen.left_margin = 0;
            screen.right_margin = viewport.cols.saturating_sub(1);
        }
    } else if mode == mode::PrivateMode::Decnrcm {
        modes.decnrcm = enable;
        for screen in [&mut *screen, &mut *stash] {
            screen.nrc_mode = enable;
            screen.charset = charset::CharsetState::new();
        }
    } else if mode == mode::PrivateMode::BracketedPaste {
        modes.bracketed_paste = enable;
    } else if mode == mode::PrivateMode::FocusReporting {
        modes.focus_reporting = enable;
    } else if mode == mode::PrivateMode::SynchronizedUpdate {
        modes.synchronized_update_since = enable.then(Instant::now);
    } else if mode == mode::PrivateMode::AllowDeccolm {
        modes.allow_deccolm = enable;
    } else if mode == mode::PrivateMode::Decatcum {
        dec_color.alternate_underline_text = enable;
    } else if mode == mode::PrivateMode::Decatcbm {
        dec_color.alternate_blink_text = enable;
    } else if mode == mode::PrivateMode::Decbbsm {
        dec_color.bold_blink_affects_background = enable;
    } else if mode == mode::PrivateMode::Dececm {
        dec_color.erase_to_screen = enable;
        for screen in [&mut *screen, &mut *stash] {
            sync_screen_erase_defaults(screen, dec_color);
        }
    } else if mode == mode::PrivateMode::Deccolm {
    } else if !apply_mouse_mode(
        mode,
        enable,
        &mut modes.mouse_tracking,
        &mut modes.mouse_encoding,
    ) {
        screen::set_private_mode(mode, enable, screen, stash, viewport, on_alt_screen);
    }
}

fn query_private_mode(
    modes: &TerminalModes,
    screen: &Screen,
    on_alt_screen: bool,
    dec_color: &DecColorState,
    cursor_style: &CursorStyle,
    mode: mode::PrivateMode,
) -> u8 {
    match mode {
        mode::PrivateMode::Decanm => {
            if !modes.vt52_mode {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decscnm => {
            if modes.screen_reverse {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decarm => {
            if modes.decarm {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Att610Blink => {
            if cursor_style.blink {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Declrmm => {
            if modes.declrmm {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decnrcm => {
            if modes.decnrcm {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decncsm => {
            if modes.decncsm {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decckm => {
            if screen.app_cursor_keys {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decom => {
            if screen.origin_mode {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decawm => {
            if screen.autowrap {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::AllowDeccolm => {
            if modes.allow_deccolm {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decatcum => {
            if dec_color.alternate_underline_text {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decatcbm => {
            if dec_color.alternate_blink_text {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decbbsm => {
            if dec_color.bold_blink_affects_background {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Dececm => {
            if dec_color.erase_to_screen {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Dectcem => {
            if screen.cursor_visible {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::Decnkm => {
            if screen.app_keypad {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::AltScreen
        | mode::PrivateMode::AltScreenClear
        | mode::PrivateMode::AltScreenSave => {
            if on_alt_screen {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::X10Mouse => {
            if modes.mouse_tracking == MouseTracking::X10 {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::NormalMouse => {
            if modes.mouse_tracking == MouseTracking::Normal {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::ButtonEventMouse => {
            if modes.mouse_tracking == MouseTracking::ButtonEvent {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::AnyEventMouse => {
            if modes.mouse_tracking == MouseTracking::AnyEvent {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::FocusReporting => {
            if modes.focus_reporting {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::SaveCursor => {
            if screen.saved_cursor.is_some() {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::BracketedPaste => {
            if modes.bracketed_paste {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::SynchronizedUpdate => {
            if modes.synchronized_update_since.is_some() {
                1
            } else {
                2
            }
        }
        _ => 0,
    }
}

fn query_private_mode_by_id(
    modes: &TerminalModes,
    screen: &Screen,
    on_alt_screen: bool,
    dec_color: &DecColorState,
    cursor_style: &CursorStyle,
    ps: u16,
) -> u8 {
    if ps == 60 {
        return 4;
    }
    let Ok(mode) = mode::PrivateMode::try_from(ps) else {
        return 0;
    };
    query_private_mode(modes, screen, on_alt_screen, dec_color, cursor_style, mode)
}

fn query_ansi_mode(
    modes: &TerminalModes,
    mode: mode::AnsiMode,
) -> u8 {
    match mode {
        mode::AnsiMode::Mode4 => 4,
        mode::AnsiMode::Irm => {
            if modes.insert_mode {
                1
            } else {
                2
            }
        }
        mode::AnsiMode::Lnm => {
            if modes.newline_mode {
                1
            } else {
                2
            }
        }
    }
}

fn query_ansi_mode_by_id(
    modes: &TerminalModes,
    ps: u16,
) -> u8 {
    let Ok(mode) = mode::AnsiMode::try_from(ps) else {
        return 0;
    };
    query_ansi_mode(modes, mode)
}

pub(crate) fn csi_parse(
    screen: &Screen,
    modes: &TerminalModes,
    params: Params,
    intermediates: &[u8],
    action: char,
) -> ParsedCsiAction {
    let params = OwnedParams::from_vte(params);
    match intermediates {
        b"?" => match action {
            'h' => ParsedCsiAction::SetPrivateModes {
                enable: true,
                modes: params,
            },
            'l' => ParsedCsiAction::SetPrivateModes {
                enable: false,
                modes: params,
            },
            's' => ParsedCsiAction::SavePrivateModes { modes: params },
            'r' => ParsedCsiAction::RestorePrivateModes { modes: params },
            'J' => ParsedCsiAction::SelectiveEraseDisplay {
                mode: first_group_param(&params, 0),
            },
            'K' => ParsedCsiAction::SelectiveEraseLine {
                mode: first_group_param(&params, 0),
            },
            'u' => ParsedCsiAction::KittyKeyboard {
                intermediate: b'?',
                params,
            },
            'n' => ParsedCsiAction::PrivateDeviceStatusReport {
                selector: first_group_param(&params, 0),
            },
            _ => ParsedCsiAction::Unsupported,
        },
        b"?$" if action == 'p' => ParsedCsiAction::QueryPrivateMode {
            mode: first_group_param(&params, 0),
        },
        b"$" => match action {
            '}' => ParsedCsiAction::SelectActiveDisplay {
                mode: first_group_param(&params, 0),
            },
            '~' => ParsedCsiAction::SetStatusDisplay {
                mode: first_group_param(&params, 0),
            },
            'w' => ParsedCsiAction::ReportStatus {
                selector: first_group_param(&params, 0),
            },
            'p' => ParsedCsiAction::QueryAnsiMode {
                mode: first_group_param(&params, 0),
            },
            '|' => ParsedCsiAction::ResizeColumns {
                cols: first_group_param(&params, 80),
            },
            'z' => ParsedCsiAction::EraseRect { params },
            '{' => ParsedCsiAction::SelectiveEraseRect { params },
            'x' => ParsedCsiAction::FillRect { params },
            'v' => ParsedCsiAction::CopyRect { params },
            'r' => ParsedCsiAction::ChangeRectAttrs { params },
            't' => ParsedCsiAction::ReverseRectAttrs { params },
            _ => ParsedCsiAction::Unsupported,
        },
        b"*" => match action {
            '|' => ParsedCsiAction::SetScreenLines {
                lines: first_group_param(&params, 24),
            },
            'x' => match first_group_param(&params, 0) {
                2 => ParsedCsiAction::SetAttrChangeExtent {
                    extent: grid::AttrChangeExtent::Rectangle,
                },
                0 | 1 => ParsedCsiAction::SetAttrChangeExtent {
                    extent: grid::AttrChangeExtent::Stream,
                },
                _ => ParsedCsiAction::Unsupported,
            },
            _ => ParsedCsiAction::Unsupported,
        },
        b" " => match action {
            'q' => ParsedCsiAction::SetCursorStyle {
                style: first_group_param(&params, 0),
            },
            '@' => ParsedCsiAction::ScrollLeft {
                count: first_group_param(&params, 1),
            },
            'A' => ParsedCsiAction::ScrollRight {
                count: first_group_param(&params, 1),
            },
            'P' => ParsedCsiAction::SelectPage {
                page: first_group_param(&params, 1),
            },
            'Q' => ParsedCsiAction::NextPage {
                count: first_group_param(&params, 1),
            },
            'R' => ParsedCsiAction::PrevPage {
                count: first_group_param(&params, 1),
            },
            _ => ParsedCsiAction::Unsupported,
        },
        b"\"" => match action {
            'p' => {
                let ps1 = first_group_param(&params, 0);
                let Some(level) = ConformanceLevel::from_decscl(ps1) else {
                    return ParsedCsiAction::Unsupported;
                };
                let ps2 = nth_group_param(&params, 1, 0);
                let c1_mode = if level.supports_c1_negotiation() {
                    C1Mode::from_decscl(Some(ps2))
                } else {
                    C1Mode::SevenBit
                };
                ParsedCsiAction::SetConformanceLevel { level, c1_mode }
            }
            'q' => ParsedCsiAction::SetCharacterProtection {
                mode: first_group_param(&params, 0),
            },
            _ => ParsedCsiAction::Unsupported,
        },
        b"'" => match action {
            '}' => ParsedCsiAction::InsertColumns {
                count: first_group_param(&params, 1),
            },
            '~' => ParsedCsiAction::DeleteColumns {
                count: first_group_param(&params, 1),
            },
            _ => ParsedCsiAction::Unsupported,
        },
        b"!" if action == 'p' => ParsedCsiAction::SoftReset,
        b"&" if action == 'u' => ParsedCsiAction::ReportUserPreferredSupplementalSet,
        b"+" if action == 'p' => ParsedCsiAction::ResetWithConfirmation {
            confirmation_param: params.get(0).and_then(|group| group.first().copied()),
        },
        [b'>' | b'<' | b'='] => match (intermediates[0], action) {
            (b'>' | b'<' | b'=', 'u') => ParsedCsiAction::KittyKeyboard {
                intermediate: intermediates[0],
                params,
            },
            (b'>', 'q') => ParsedCsiAction::ReportXtVersion,
            (b'>', 'c') => ParsedCsiAction::ReportSecondaryDeviceAttrs,
            (b'=', 'c') => ParsedCsiAction::ReportTertiaryDeviceAttrs,
            _ => ParsedCsiAction::Unsupported,
        },
        b"" if screen.active_display == ActiveDisplay::Status
            && screen::status_line_writable(screen) =>
        {
            parse_status_line_plain_csi(params, action)
        }
        b"" => parse_main_plain_csi(modes, params, action),
        _ => ParsedCsiAction::Unsupported,
    }
}

#[bon::builder]
fn apply_main_csi(
    action: MainCsiAction,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    pending_output: &mut Vec<u8>,
    pending_resize: &mut Option<(u32, u32)>,
    cursor_style: &mut CursorStyle,
    cell_width: u32,
    cell_height: u32,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    title_stack: &mut Vec<Option<String>>,
    current_title: &mut Option<String>,
    saved_modes: &mut HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    feature_permissions: &FeaturePermissions,
    drcs: &mut DrcsStore,
) {
    let pending_output = &mut *pending_output;
    let screen = &mut *screen;
    let mut viewport = screen::screen_viewport(screen, viewport);

    match action {
        MainCsiAction::SelfTest { requested_tests } => {
            let power_up_self_test = requested_tests.is_empty()
                || requested_tests.contains(&0)
                || requested_tests.contains(&1);
            if power_up_self_test {
                apply_hard_reset_state()
                    .screen(screen)
                    .stash(stash)
                    .on_alt_screen(on_alt_screen)
                    .modes(modes)
                    .viewport(&mut viewport)
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
                    .conformance_level(ConformanceLevel::Level4)
                    .c1_mode(C1Mode::SevenBit)
                    .call();
            }
        }
        MainCsiAction::ReportPrimaryDeviceAttrs => {
            let macro_allowed = feature_permissions.macros.allow();
            let level = if macro_allowed {
                modes.conformance_level.da1_code()
            } else {
                modes.conformance_level.da1_code().min(63)
            };
            let macro_feature = if macro_allowed { ";32" } else { "" };
            conformance::write_csi(
                pending_output,
                modes.c1_mode,
                format_args!("?{level};7;21;22;28;29{macro_feature}c"),
            );
        }
        MainCsiAction::DeviceStatusReport { selector } => match selector {
            DSR_OK => {
                conformance::write_csi(pending_output, modes.c1_mode, format_args!("0n"));
            }
            DSR_CPR => {
                let row = screen.cursor.row + 1;
                let col = screen.cursor.col + 1;
                conformance::write_csi(pending_output, modes.c1_mode, format_args!("{row};{col}R"));
            }
            _ => {}
        },
        MainCsiAction::SetPageLines { lines } => {
            let Some(lines_per_page) = valid_page_lines(lines) else {
                return;
            };
            let rows = viewport.rows.min(lines_per_page);
            for screen in [&mut *screen, &mut *stash] {
                screen::activate_page_memory(
                    screen,
                    &Viewport {
                        rows,
                        cols: viewport.cols,
                        top: 0,
                    },
                    lines_per_page,
                );
            }
            if rows != viewport.rows {
                let old_cols = viewport.cols;
                let old_total_rows = viewport.rows + screen::status_line_rows(screen);
                let new_total_rows = rows + screen::status_line_rows(screen);
                for screen in [&mut *screen, &mut *stash] {
                    let old_rows = old_total_rows.saturating_sub(screen::status_line_rows(screen));
                    let new_rows = new_total_rows.saturating_sub(screen::status_line_rows(screen));
                    screen::resize_screen(screen, old_cols, old_rows, old_cols, new_rows);
                }
                viewport.rows = rows;
                *pending_resize = Some((viewport.cols, rows + screen::status_line_rows(screen)));
            }
        }
        MainCsiAction::PushTitle if title_stack.len() < 16 => {
            title_stack.push(current_title.clone());
        }
        MainCsiAction::PushTitle => {}
        MainCsiAction::PopTitle => {
            if let Some(title) = title_stack.pop() {
                *current_title = title;
            }
        }
        MainCsiAction::ReportPixelSize => {
            let h = viewport.rows * cell_height;
            let w = viewport.cols * cell_width;
            conformance::write_csi(pending_output, modes.c1_mode, format_args!("4;{h};{w}t"));
        }
        MainCsiAction::ReportCellSize => {
            conformance::write_csi(
                pending_output,
                modes.c1_mode,
                format_args!("6;{};{}t", cell_height, cell_width),
            );
        }
        MainCsiAction::ReportTextSize => {
            conformance::write_csi(
                pending_output,
                modes.c1_mode,
                format_args!("8;{};{}t", viewport.rows, viewport.cols),
            );
        }
        MainCsiAction::RepeatLastChar { count } => {
            if let Some(ch) = screen.last_char.clone() {
                let insert = modes.insert_mode;
                let view = screen::screen_viewport(screen, &viewport);
                for _ in 0..count.max(1) {
                    put_char(screen, &view, ch.clone(), insert);
                }
            }
        }
        MainCsiAction::CursorUp { count } => {
            let n = count.max(1) as u32;
            let top = if screen.origin_mode {
                screen.scroll_top
            } else {
                0
            };
            screen.cursor.row = screen.cursor.row.saturating_sub(n).max(top);
            clamp_cursor_to_row_width(screen, &viewport);
        }
        MainCsiAction::CursorDown { count } => {
            let n = count.max(1) as u32;
            let bottom = if screen.origin_mode {
                screen.scroll_bottom
            } else {
                viewport.rows - 1
            };
            screen.cursor.row = (screen.cursor.row + n).min(bottom);
            clamp_cursor_to_row_width(screen, &viewport);
        }
        MainCsiAction::CursorForward { count } => {
            let n = count.max(1) as u32;
            let cols = current_row_display_cols(screen, &viewport);
            screen.cursor.col = (screen.cursor.col + n).min(cols - 1);
        }
        MainCsiAction::CursorBackward { count } => {
            screen.cursor.col = screen.cursor.col.saturating_sub(count.max(1) as u32);
        }
        MainCsiAction::CursorNextLine { count } => {
            let n = count.max(1) as u32;
            screen.cursor.row = (screen.cursor.row + n).min(viewport.rows - 1);
            screen.cursor.col = 0;
        }
        MainCsiAction::CursorPreviousLine { count } => {
            let n = count.max(1) as u32;
            screen.cursor.row = screen.cursor.row.saturating_sub(n);
            screen.cursor.col = 0;
        }
        MainCsiAction::CursorPosition { row, col } => {
            let row = row.max(1) as u32 - 1;
            let col = col.max(1) as u32 - 1;
            let target_row = if screen.origin_mode {
                (screen.scroll_top + row).min(screen.scroll_bottom)
            } else {
                row.min(viewport.rows - 1)
            };
            let cols = row_display_cols(screen, &viewport, target_row);
            screen.cursor.row = target_row;
            screen.cursor.col = col.min(cols - 1);
        }
        MainCsiAction::EraseInDisplay { mode } => {
            grid::erase_in_display_op(
                &mut screen.grid,
                &screen.cursor,
                &viewport,
                &mut screen.images,
                mode,
            );
        }
        MainCsiAction::EraseInLine { mode } => {
            grid::erase_in_line_op(&mut screen.grid, &screen.cursor, &viewport, mode);
        }
        MainCsiAction::SetGraphicsRendition { params } => {
            apply_sgr_groups(
                &mut screen.fg,
                &mut screen.bg,
                &mut screen.attrs,
                &mut screen.underline,
                &mut screen.underline_color,
                params.as_groups(),
                palette,
            );
            sync_screen_erase_defaults(screen, dec_color);
        }
        MainCsiAction::LinePositionAbsolute { row } => {
            let row = row.max(1) as u32 - 1;
            if screen.origin_mode {
                screen.cursor.row = (screen.scroll_top + row).min(screen.scroll_bottom);
            } else {
                screen.cursor.row = row.min(viewport.rows - 1);
            }
            clamp_cursor_to_row_width(screen, &viewport);
        }
        MainCsiAction::CursorHorizontalAbsolute { col } => {
            let col = col.max(1) as u32 - 1;
            let cols = current_row_display_cols(screen, &viewport);
            screen.cursor.col = col.min(cols - 1);
        }
        MainCsiAction::CursorForwardRelative { count } => {
            let n = count.max(1) as u32;
            let cols = current_row_display_cols(screen, &viewport);
            screen.cursor.col = (screen.cursor.col + n).min(cols - 1);
        }
        MainCsiAction::CursorVerticalRelative { count } => {
            let n = count.max(1) as u32;
            let bottom = if screen.origin_mode {
                screen.scroll_bottom
            } else {
                viewport.rows - 1
            };
            screen.cursor.row = (screen.cursor.row + n).min(bottom);
            clamp_cursor_to_row_width(screen, &viewport);
        }
        MainCsiAction::InsertLines { count } => {
            let n = count.max(1) as u32;
            if screen.cursor.row >= screen.scroll_top && screen.cursor.row <= screen.scroll_bottom {
                let top = screen.cursor.row;
                if modes.declrmm {
                    grid::scroll_down_in_rect_op(
                        &mut screen.grid,
                        &viewport,
                        top,
                        screen.scroll_bottom,
                        screen.left_margin,
                        screen.right_margin,
                        n,
                    );
                } else {
                    grid::scroll_down_in_region_op(
                        &mut screen.grid,
                        &viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        n,
                    );
                }
            }
        }
        MainCsiAction::DeleteLines { count } => {
            let n = count.max(1) as u32;
            if screen.cursor.row >= screen.scroll_top && screen.cursor.row <= screen.scroll_bottom {
                let top = screen.cursor.row;
                if modes.declrmm {
                    grid::scroll_up_in_rect_op(
                        &mut screen.grid,
                        &viewport,
                        top,
                        screen.scroll_bottom,
                        screen.left_margin,
                        screen.right_margin,
                        n,
                    );
                } else {
                    grid::scroll_up_in_region_op(
                        &mut screen.grid,
                        &viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        n,
                    );
                }
            }
        }
        MainCsiAction::DeleteChars { count } => {
            grid::delete_chars_op(&mut screen.grid, &screen.cursor, &viewport, count.max(1));
        }
        MainCsiAction::InsertChars { count } => {
            grid::insert_chars_op(&mut screen.grid, &screen.cursor, &viewport, count.max(1));
        }
        MainCsiAction::EraseChars { count } => {
            grid::erase_chars_op(&mut screen.grid, &screen.cursor, &viewport, count.max(1));
        }
        MainCsiAction::ScrollUp { count } => {
            let n = count.max(1) as u32;
            if screen::page_can_scroll_down(screen, &viewport) {
                screen::scroll_page_down(screen, &viewport, n);
            } else if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                for _ in 0..n {
                    screen.grid.push_visible_row(&viewport);
                }
            } else {
                grid::scroll_up_in_region_op(
                    &mut screen.grid,
                    &viewport,
                    &mut screen.images,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        MainCsiAction::ScrollDown { count } => {
            grid::scroll_down_in_region_op(
                &mut screen.grid,
                &viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                count.max(1) as u32,
            );
        }
        MainCsiAction::SetScrollRegion { top, bottom } => {
            let top = top.max(1) as u32 - 1;
            let bottom = bottom.unwrap_or(viewport.rows as u16).max(1) as u32 - 1;
            screen.scroll_top = top.min(viewport.rows - 1);
            screen.scroll_bottom = bottom.min(viewport.rows - 1).max(screen.scroll_top);
            screen.cursor.row = if screen.origin_mode {
                screen.scroll_top
            } else {
                0
            };
            screen.cursor.col = 0;
        }
        MainCsiAction::SetLeftRightMargins { left, right } => {
            let left = left.max(1) as u32 - 1;
            let right = right.unwrap_or(viewport.cols as u16).max(1) as u32 - 1;
            screen.left_margin = left.min(viewport.cols.saturating_sub(1));
            screen.right_margin = right
                .min(viewport.cols.saturating_sub(1))
                .max(screen.left_margin);
        }
        MainCsiAction::SaveCursor => {
            screen::save_cursor_slot(screen);
        }
        MainCsiAction::RestoreCursor => {
            screen::restore_cursor_slot(screen, &viewport);
        }
        MainCsiAction::NextPage { count } => {
            let n = count.max(1) as u32;
            screen::activate_page_memory(screen, &viewport, viewport.rows);
            if let Some(page) = screen.page_memory.as_mut() {
                page.active_page = (page.active_page + n).min(page.page_count().saturating_sub(1));
                page.display_top = 0;
            }
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        MainCsiAction::PrevPage { count } => {
            let n = count.max(1) as u32;
            screen::activate_page_memory(screen, &viewport, viewport.rows);
            if let Some(page) = screen.page_memory.as_mut() {
                page.active_page = page.active_page.saturating_sub(n);
                page.display_top = 0;
            }
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        MainCsiAction::CursorForwardTabulation { count } => {
            let cols = current_row_display_cols(screen, &viewport);
            for _ in 0..count.max(1) {
                screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
            }
        }
        MainCsiAction::CursorBackwardTabulation { count } => {
            for _ in 0..count.max(1) {
                screen.cursor.col = prev_tab_stop(&screen.tab_stops, screen.cursor.col);
            }
        }
        MainCsiAction::TabClear { mode } => match mode {
            TBC_CURRENT => {
                let col = screen.cursor.col as usize;
                if col < screen.tab_stops.len() {
                    screen.tab_stops[col] = false;
                }
            }
            TBC_ALL => screen.tab_stops.fill(false),
            _ => {}
        },
        MainCsiAction::SetAnsiModes {
            enable,
            modes: mode_ids,
        } => {
            for m in mode_ids {
                match m {
                    mode::AnsiMode::Irm => modes.insert_mode = enable,
                    mode::AnsiMode::Lnm => modes.newline_mode = enable,
                    mode::AnsiMode::Mode4 => {}
                }
            }
        }
    }
}

#[bon::builder]
pub(crate) fn csi_apply(
    action: ParsedCsiAction,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    pending_output: &mut Vec<u8>,
    pending_resize: &mut Option<(u32, u32)>,
    cursor_style: &mut CursorStyle,
    cell_width: u32,
    cell_height: u32,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    title_stack: &mut Vec<Option<String>>,
    current_title: &mut Option<String>,
    saved_modes: &mut HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    feature_permissions: &FeaturePermissions,
    drcs: &mut DrcsStore,
) {
    clamp_cursor_to_row_width(screen, viewport);

    match action {
        ParsedCsiAction::Unsupported => (),
        ParsedCsiAction::StatusLine(action) => {
            apply_status_line_csi(screen, palette, modes.insert_mode, action);
        }
        ParsedCsiAction::Main(action) => {
            apply_main_csi()
                .action(action)
                .screen(screen)
                .stash(stash)
                .viewport(viewport)
                .on_alt_screen(on_alt_screen)
                .modes(modes)
                .kitty_keyboard(kitty_keyboard)
                .pending_output(pending_output)
                .pending_resize(pending_resize)
                .cursor_style(cursor_style)
                .cell_width(cell_width)
                .cell_height(cell_height)
                .palette(palette)
                .base_palette(base_palette)
                .dec_color(dec_color)
                .default_status_display(default_status_display)
                .title_stack(title_stack)
                .current_title(current_title)
                .saved_modes(saved_modes)
                .current_prompt_row(current_prompt_row)
                .bell_pending(bell_pending)
                .vt52_cursor_addr(vt52_cursor_addr)
                .macros(macros)
                .feature_permissions(feature_permissions)
                .drcs(drcs)
                .call();
        }
        ParsedCsiAction::SetPrivateModes {
            enable,
            modes: params,
        } => {
            for p in params.iter() {
                let Ok(mode) = mode::PrivateMode::try_from(p[0]) else {
                    continue;
                };
                match mode {
                    mode::PrivateMode::Deccolm => {
                        if !modes.allow_deccolm {
                            continue;
                        }
                        let new_cols = if enable {
                            modes.deccolm_saved_cols = Some(viewport.cols);
                            132
                        } else {
                            modes.deccolm_saved_cols.take().unwrap_or(viewport.cols)
                        };
                        let old_cols = viewport.cols;
                        let rows = viewport.rows;
                        for s in [&mut *screen, &mut *stash] {
                            screen::resize_screen(s, old_cols, rows, new_cols, rows);
                        }
                        viewport.cols = new_cols;
                        if !modes.decncsm {
                            let view = screen::screen_viewport(screen, viewport);
                            screen::clear_visible(screen, &view);
                        }
                        screen.scroll_top = 0;
                        screen.scroll_bottom = rows.saturating_sub(1);
                        screen.left_margin = 0;
                        screen.right_margin = viewport.cols.saturating_sub(1);
                        screen.cursor = grid::Cursor::default();
                    }
                    mode => apply_private_mode(
                        modes,
                        screen,
                        stash,
                        viewport,
                        on_alt_screen,
                        cursor_style,
                        dec_color,
                        mode,
                        enable,
                    ),
                }
            }
        }
        ParsedCsiAction::SavePrivateModes { modes: params } => {
            for p in params.iter() {
                let Ok(mode) = mode::PrivateMode::try_from(p[0]) else {
                    continue;
                };
                let state = query_private_mode(
                    modes,
                    screen,
                    *on_alt_screen,
                    dec_color,
                    cursor_style,
                    mode,
                );
                saved_modes.insert(mode, state == 1);
            }
        }
        ParsedCsiAction::RestorePrivateModes { modes: params } => {
            for p in params.iter() {
                let Ok(mode) = mode::PrivateMode::try_from(p[0]) else {
                    continue;
                };
                if let Some(&saved) = saved_modes.get(&mode) {
                    apply_private_mode(
                        modes,
                        screen,
                        stash,
                        viewport,
                        on_alt_screen,
                        cursor_style,
                        dec_color,
                        mode,
                        saved,
                    );
                }
            }
        }
        ParsedCsiAction::SelectiveEraseDisplay { mode } => {
            let view = screen::screen_viewport(screen, viewport);
            grid::erase_in_display_selective_op(
                &mut screen.grid,
                &screen.cursor,
                &view,
                &mut screen.images,
                mode,
            );
        }
        ParsedCsiAction::SelectiveEraseLine { mode } => {
            let view = screen::screen_viewport(screen, viewport);
            grid::erase_in_line_selective_op(&mut screen.grid, &screen.cursor, &view, mode);
        }
        ParsedCsiAction::KittyKeyboard {
            intermediate,
            params,
        } => {
            let groups: Vec<&[u16]> = params.iter().collect();
            handle_kitty_keyboard_groups(
                intermediate,
                &groups,
                kitty_keyboard,
                modes.c1_mode,
                pending_output,
            );
        }
        ParsedCsiAction::PrivateDeviceStatusReport { selector } => {
            if selector == DSR_CPR {
                let row = screen.cursor.row + 1;
                let col = screen.cursor.col + 1;
                let page = screen
                    .page_memory
                    .as_ref()
                    .map(|page| page.active_page + 1)
                    .unwrap_or(1);
                conformance::write_csi(
                    pending_output,
                    modes.c1_mode,
                    format_args!("?{row};{col};{page}R"),
                );
            }
        }
        ParsedCsiAction::QueryPrivateMode { mode: ps } => {
            let pm = query_private_mode_by_id(
                modes,
                screen,
                *on_alt_screen,
                dec_color,
                cursor_style,
                ps,
            );
            conformance::write_csi(pending_output, modes.c1_mode, format_args!("?{ps};{pm}$y"));
        }
        ParsedCsiAction::SelectActiveDisplay { mode } => {
            screen.active_display = match mode {
                1 if screen::status_line_writable(screen) => ActiveDisplay::Status,
                _ => ActiveDisplay::Main,
            };
        }
        ParsedCsiAction::SetStatusDisplay { mode } => {
            let total_rows = viewport.rows + screen::status_line_rows(screen);
            let old_rows = viewport.rows;
            let status_display = match mode {
                1 => StatusDisplayKind::Indicator,
                2 => StatusDisplayKind::HostWritable,
                _ => StatusDisplayKind::None,
            };
            screen::set_status_display(
                screen,
                viewport.cols,
                status_display,
                palette.status_line_fg,
                palette.status_line_bg,
            );
            let new_rows = total_rows.saturating_sub(screen::status_line_rows(screen));
            if new_rows != old_rows {
                let old_cols = viewport.cols;
                screen::resize_screen(screen, old_cols, old_rows, old_cols, new_rows);
                if screen::page_memory_active(screen)
                    && let Some(page_rows) = screen::page_rows(screen)
                {
                    screen::resize_page_memory(
                        screen,
                        &Viewport {
                            rows: new_rows,
                            cols: old_cols,
                            top: 0,
                        },
                        page_rows,
                    );
                }
                viewport.rows = new_rows;
            }
        }
        ParsedCsiAction::ReportStatus { selector } => match selector {
            1 => {
                if let Some(report) = crate::deccir_report(screen, viewport, modes, drcs) {
                    conformance::write_dcs(
                        pending_output,
                        modes.c1_mode,
                        format_args!("1$u{report}"),
                    );
                }
            }
            2 => {
                let stops = crate::dectabsr_report(screen);
                conformance::write_dcs(pending_output, modes.c1_mode, format_args!("2$u{stops}"));
            }
            _ => {}
        },
        ParsedCsiAction::QueryAnsiMode { mode: ps } => {
            let pm = query_ansi_mode_by_id(modes, ps);
            conformance::write_csi(pending_output, modes.c1_mode, format_args!("{ps};{pm}$y"));
        }
        ParsedCsiAction::ResizeColumns { cols } => {
            let Some(cols) = matches!(cols, 80 | 132).then_some(cols as u32) else {
                return;
            };
            let old_cols = viewport.cols;
            let total_rows = viewport.rows + screen::status_line_rows(screen);
            for screen in [&mut *screen, &mut *stash] {
                let rows = total_rows.saturating_sub(screen::status_line_rows(screen));
                screen::resize_screen(screen, old_cols, rows, cols, rows);
                if screen::page_memory_active(screen) {
                    let page_rows = screen::page_rows(screen).unwrap_or(rows);
                    screen::resize_page_memory(screen, &Viewport { rows, cols, top: 0 }, page_rows);
                }
            }
            viewport.cols = cols;
            *pending_resize = Some((cols, viewport.rows + screen::status_line_rows(screen)));
            screen.right_margin = cols.saturating_sub(1);
            screen.cursor.col = screen.cursor.col.min(cols.saturating_sub(1));
        }
        ParsedCsiAction::EraseRect { ref params }
        | ParsedCsiAction::SelectiveEraseRect { ref params }
        | ParsedCsiAction::FillRect { ref params }
        | ParsedCsiAction::CopyRect { ref params }
        | ParsedCsiAction::ChangeRectAttrs { ref params }
        | ParsedCsiAction::ReverseRectAttrs { ref params } => {
            let view = screen::screen_viewport(screen, viewport);
            let rows = view.rows;
            let cols = view.cols;
            let p: Vec<u16> = params.iter().map(|group| group[0]).collect();

            if matches!(
                action,
                ParsedCsiAction::ChangeRectAttrs { .. } | ParsedCsiAction::ReverseRectAttrs { .. }
            ) && screen.attr_change_extent == grid::AttrChangeExtent::Stream
            {
                let start_row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
                let start_col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
                let end_row = (p.get(2).copied().unwrap_or(rows as u16).max(1) as u32 - 1)
                    .min(rows.saturating_sub(1));
                let end_col = (p.get(3).copied().unwrap_or(cols as u16).max(1) as u32 - 1)
                    .min(cols.saturating_sub(1));
                if start_row > end_row || (start_row == end_row && start_col > end_col) {
                    return;
                }
                let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                match action {
                    ParsedCsiAction::ChangeRectAttrs { .. } => grid::change_attrs_rect_op(
                        &mut screen.grid,
                        &view,
                        start_row,
                        start_col,
                        end_row,
                        end_col,
                        &sgr,
                        screen.attr_change_extent,
                    ),
                    ParsedCsiAction::ReverseRectAttrs { .. } => grid::reverse_attrs_rect_op(
                        &mut screen.grid,
                        &view,
                        start_row,
                        start_col,
                        end_row,
                        end_col,
                        &sgr,
                        screen.attr_change_extent,
                    ),
                    _ => {}
                }
                return;
            }

            let rect_top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let rect_left = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
            let rect_bottom = (p.get(2).copied().unwrap_or(rows as u16).max(1) as u32 - 1)
                .min(rows.saturating_sub(1));
            let rect_right = (p.get(3).copied().unwrap_or(cols as u16).max(1) as u32 - 1)
                .min(cols.saturating_sub(1));

            if rect_top > rect_bottom || rect_left > rect_right {
                return;
            }

            match action {
                ParsedCsiAction::EraseRect { .. } => {
                    grid::erase_rect_op(
                        &mut screen.grid,
                        &view,
                        rect_top,
                        rect_left,
                        rect_bottom,
                        rect_right,
                    );
                }
                ParsedCsiAction::SelectiveEraseRect { .. } => {
                    grid::erase_rect_selective_op(
                        &mut screen.grid,
                        &view,
                        rect_top,
                        rect_left,
                        rect_bottom,
                        rect_right,
                    );
                }
                ParsedCsiAction::FillRect { .. } => {
                    let ch_code = p.get(4).copied().unwrap_or(0x20) as u32;
                    let valid = (32..=126).contains(&ch_code) || (160..=255).contains(&ch_code);
                    if valid && let Some(ch) = char::from_u32(ch_code) {
                        let mut buf = [0u8; 4];
                        let s = SmolStr::new(ch.encode_utf8(&mut buf) as &str);
                        grid::fill_rect_op(
                            &mut screen.grid,
                            &view,
                            rect_top,
                            rect_left,
                            rect_bottom,
                            rect_right,
                            s,
                            screen.fg,
                            screen.bg,
                            screen.attrs,
                            screen.underline,
                            screen.underline_color,
                        );
                    }
                }
                ParsedCsiAction::CopyRect { .. } => {
                    let src_page = p.get(4).copied().unwrap_or(1);
                    let dst_top = p.get(5).copied().unwrap_or(1).max(1) as u32 - 1;
                    let dst_left = p.get(6).copied().unwrap_or(1).max(1) as u32 - 1;
                    let dst_page = p.get(7).copied().unwrap_or(1);
                    if src_page > 1 || dst_page > 1 {
                        screen::ensure_page_memory(screen, viewport);
                    }
                    let Some(src_view) = screen::page_viewport(screen, viewport, src_page) else {
                        return;
                    };
                    let Some(dst_view) = screen::page_viewport(screen, viewport, dst_page) else {
                        return;
                    };
                    grid::copy_rect_op(
                        &mut screen.grid,
                        &src_view,
                        rect_top,
                        rect_left,
                        rect_bottom,
                        rect_right,
                        dst_top,
                        dst_left,
                        &dst_view,
                    );
                }
                ParsedCsiAction::ChangeRectAttrs { .. } => {
                    let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                    grid::change_attrs_rect_op(
                        &mut screen.grid,
                        &view,
                        rect_top,
                        rect_left,
                        rect_bottom,
                        rect_right,
                        &sgr,
                        screen.attr_change_extent,
                    );
                }
                ParsedCsiAction::ReverseRectAttrs { .. } => {
                    let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                    grid::reverse_attrs_rect_op(
                        &mut screen.grid,
                        &view,
                        rect_top,
                        rect_left,
                        rect_bottom,
                        rect_right,
                        &sgr,
                        screen.attr_change_extent,
                    );
                }
                _ => {}
            }
        }
        ParsedCsiAction::SetScreenLines { lines } => {
            if let Some(rows) = valid_screen_lines(lines) {
                let page_rows = screen::page_rows(screen).unwrap_or(rows.max(viewport.rows));
                for screen in [&mut *screen, &mut *stash] {
                    screen::activate_page_memory(
                        screen,
                        &Viewport {
                            rows,
                            cols: viewport.cols,
                            top: 0,
                        },
                        page_rows,
                    );
                }
                let old_cols = viewport.cols;
                let old_total_rows = viewport.rows + screen::status_line_rows(screen);
                let new_total_rows = rows + screen::status_line_rows(screen);
                for screen in [&mut *screen, &mut *stash] {
                    let old_rows = old_total_rows.saturating_sub(screen::status_line_rows(screen));
                    let new_rows = new_total_rows.saturating_sub(screen::status_line_rows(screen));
                    screen::resize_screen(screen, old_cols, old_rows, old_cols, new_rows);
                }
                viewport.rows = rows;
                *pending_resize = Some((viewport.cols, rows + screen::status_line_rows(screen)));
                screen.scroll_top = 0;
                screen.scroll_bottom = rows.saturating_sub(1);
                screen.cursor.row = screen.cursor.row.min(rows.saturating_sub(1));
            }
        }
        ParsedCsiAction::SetAttrChangeExtent { extent } => {
            screen.attr_change_extent = extent;
        }
        ParsedCsiAction::SetCursorStyle { style } => {
            cursor_style.apply_decscusr(style);
        }
        ParsedCsiAction::ScrollLeft { count } => {
            let view = screen::screen_viewport(screen, viewport);
            let n = count.max(1) as u32;
            grid::scroll_left_op(
                &mut screen.grid,
                &view,
                screen.scroll_top,
                screen.scroll_bottom,
                n,
            );
        }
        ParsedCsiAction::ScrollRight { count } => {
            let view = screen::screen_viewport(screen, viewport);
            let n = count.max(1) as u32;
            grid::scroll_right_op(
                &mut screen.grid,
                &view,
                screen.scroll_top,
                screen.scroll_bottom,
                n,
            );
        }
        ParsedCsiAction::SelectPage { page } => {
            let view = screen::screen_viewport(screen, viewport);
            screen::activate_page_memory(screen, &view, view.rows);
            if let Some(page_memory) = screen.page_memory.as_mut() {
                page_memory.active_page = u32::from(page.saturating_sub(1))
                    .min(page_memory.page_count().saturating_sub(1));
            }
        }
        ParsedCsiAction::NextPage { count } => {
            let n = count.max(1) as u32;
            let view = screen::screen_viewport(screen, viewport);
            screen::activate_page_memory(screen, &view, view.rows);
            if let Some(page_memory) = screen.page_memory.as_mut() {
                page_memory.active_page =
                    (page_memory.active_page + n).min(page_memory.page_count().saturating_sub(1));
            }
        }
        ParsedCsiAction::PrevPage { count } => {
            let n = count.max(1) as u32;
            let view = screen::screen_viewport(screen, viewport);
            screen::activate_page_memory(screen, &view, view.rows);
            if let Some(page_memory) = screen.page_memory.as_mut() {
                page_memory.active_page = page_memory.active_page.saturating_sub(n);
            }
        }
        ParsedCsiAction::SetConformanceLevel { level, c1_mode } => {
            modes.conformance_level = level;
            modes.c1_mode = c1_mode;
            modes.vt52_mode = false;
        }
        ParsedCsiAction::SetCharacterProtection { mode } => match mode {
            1 => screen.attrs.insert(CellAttrs::PROTECTED),
            0 | 2 => screen.attrs.remove(CellAttrs::PROTECTED),
            _ => {}
        },
        ParsedCsiAction::InsertColumns { count } => {
            let view = screen::screen_viewport(screen, viewport);
            grid::insert_cols_op(
                &mut screen.grid,
                &view,
                screen.cursor.col,
                screen.scroll_top,
                screen.scroll_bottom,
                count.max(1) as u32,
            );
        }
        ParsedCsiAction::DeleteColumns { count } => {
            let view = screen::screen_viewport(screen, viewport);
            grid::delete_cols_op(
                &mut screen.grid,
                &view,
                screen.cursor.col,
                screen.scroll_top,
                screen.scroll_bottom,
                count.max(1) as u32,
            );
        }
        ParsedCsiAction::SoftReset => {
            if modes.vt52_mode || !modes.conformance_level.supports_c1_negotiation() {
                return;
            }
            screen.fg = palette.fg;
            screen.bg = palette.bg;
            screen.attrs = CellAttrs::default();
            screen.underline = UnderlineStyle::None;
            screen.underline_color = None;
            screen.scroll_top = 0;
            screen.scroll_bottom = viewport.rows.saturating_sub(1);
            screen.left_margin = 0;
            screen.right_margin = viewport.cols.saturating_sub(1);
            screen.saved_cursor = None;
            screen.current_hyperlink = None;
            screen.cursor_visible = true;
            screen.last_char = None;
            screen.tab_stops = screen::init_tab_stops(viewport.cols);
            screen.origin_mode = false;
            screen.nrc_mode = false;
            screen.upss = charset::UserPreferredSupplementalSet::DecSupplemental;
            screen.autowrap = true;
            screen.app_cursor_keys = false;
            screen.attr_change_extent = grid::AttrChangeExtent::Stream;
            screen.app_keypad = false;
            screen.charset = charset::CharsetState::new();
            let conformance_level = modes.conformance_level;
            let c1_mode = modes.c1_mode;
            *modes = TerminalModes::new();
            modes.conformance_level = conformance_level;
            modes.c1_mode = c1_mode;
            *kitty_keyboard = KittyKeyboardState::new();
            *cursor_style = CursorStyle::default();
        }
        ParsedCsiAction::ReportUserPreferredSupplementalSet => {
            conformance::write_dcs(
                pending_output,
                modes.c1_mode,
                format_args!("{}", charset::decaupss_report(screen.upss)),
            );
        }
        ParsedCsiAction::ResetWithConfirmation { confirmation_param } => {
            apply_hard_reset_state()
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
                .vt52_cursor_addr(vt52_cursor_addr)
                .dec_color(dec_color)
                .default_status_display(default_status_display)
                .macros(macros)
                .drcs(drcs)
                .palette(palette)
                .base_palette(base_palette)
                .conformance_level(ConformanceLevel::Level4)
                .c1_mode(C1Mode::SevenBit)
                .call();

            if let Some(pr) = confirmation_param {
                conformance::write_csi(pending_output, modes.c1_mode, format_args!("{pr}*q"));
            }
        }
        ParsedCsiAction::ReportXtVersion => {
            conformance::write_dcs(
                pending_output,
                modes.c1_mode,
                format_args!(">|term41 {}", env!("CARGO_PKG_VERSION")),
            );
        }
        ParsedCsiAction::ReportSecondaryDeviceAttrs => {
            conformance::write_csi(pending_output, modes.c1_mode, format_args!(">41;0;0c"));
        }
        ParsedCsiAction::ReportTertiaryDeviceAttrs => {
            if modes.vt52_mode || !modes.conformance_level.supports_c1_negotiation() {
                return;
            }
            conformance::write_dcs(pending_output, modes.c1_mode, format_args!("!|000000000"));
        }
    }
}

#[cfg(test)]
#[bon::builder]
pub(crate) fn csi_dispatch(
    params: &Params,
    intermediates: &[u8],
    action: char,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    pending_output: &mut Vec<u8>,
    pending_resize: &mut Option<(u32, u32)>,
    cursor_style: &mut CursorStyle,
    cell_width: u32,
    cell_height: u32,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    title_stack: &mut Vec<Option<String>>,
    current_title: &mut Option<String>,
    saved_modes: &mut HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    feature_permissions: &FeaturePermissions,
    drcs: &mut DrcsStore,
) {
    let action = csi_parse(screen, modes, *params, intermediates, action);
    csi_apply()
        .action(action)
        .screen(screen)
        .stash(stash)
        .viewport(viewport)
        .on_alt_screen(on_alt_screen)
        .modes(modes)
        .kitty_keyboard(kitty_keyboard)
        .pending_output(pending_output)
        .pending_resize(pending_resize)
        .cursor_style(cursor_style)
        .cell_width(cell_width)
        .cell_height(cell_height)
        .palette(palette)
        .base_palette(base_palette)
        .dec_color(dec_color)
        .default_status_display(default_status_display)
        .title_stack(title_stack)
        .current_title(current_title)
        .saved_modes(saved_modes)
        .current_prompt_row(current_prompt_row)
        .bell_pending(bell_pending)
        .vt52_cursor_addr(vt52_cursor_addr)
        .macros(macros)
        .feature_permissions(feature_permissions)
        .drcs(drcs)
        .call();
}

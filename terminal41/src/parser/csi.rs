use std::collections::HashMap;

use font41::attrs::CellAttrs;
use smol_str::SmolStr;
use vte_mode41::C1Mode;
use vte_mode41::ConformanceLevel;
use vtepp::Params;

use crate::ColorPalette;
use crate::CursorStyle;
use crate::DecColorState;
use crate::FeaturePermissions;
use crate::KittyKeyboardState;
use crate::Screen;
use crate::ShellIntegrationPhase;
use crate::StatusDisplayKind;
use crate::TerminalModes;
use crate::Viewport;
use crate::charset;
use crate::color::apply_sgr_groups;
use crate::conformance;
use crate::cursor::DecCusr;
use crate::dec::r#macro::MacroStore;
use crate::dec::udk::UdkState;
use crate::drcs::DrcsStore;
use crate::io::keyboard::handle_kitty_keyboard_groups;
use crate::mode;
use crate::parser::BorrowedParams;
use crate::parser::DsrParameters;
use crate::parser::MainCsiAction;
use crate::parser::ParsedCsiAction;
use crate::parser::StatusLineCsiAction;
use crate::parser::TabClearMode;
use crate::parser::WinManipulationAction;
use crate::parser::apply_hard_reset_state;
use crate::parser::apply_status_line_csi;
use crate::parser::clamp_cursor_to_row_width;
use crate::parser::current_row_display_cols;
use crate::parser::next_tab_stop;
use crate::parser::prev_tab_stop;
use crate::parser::put_char_with_scrollback_policy;
use crate::parser::row_display_cols;
use crate::parser::sync_screen_erase_defaults;
use crate::parser::valid_page_lines;
use crate::parser::valid_screen_lines;
use crate::screen;
use crate::screen::ActiveDisplay;
use crate::screen::grid;

mod private_modes;

use private_modes::apply_private_mode;
use private_modes::query_ansi_mode_by_id;
use private_modes::query_private_mode;
use private_modes::query_private_mode_by_id;

fn first_group_param(
    params: &BorrowedParams<'_>,
    default: u16,
) -> u16 {
    params
        .get(0)
        .and_then(|group| group.first().copied())
        .unwrap_or(default)
}

fn nth_group_param(
    params: &BorrowedParams<'_>,
    idx: usize,
    default: u16,
) -> u16 {
    params
        .get(idx)
        .and_then(|group| group.first().copied())
        .unwrap_or(default)
}

fn parse_status_line_plain_csi<'a>(
    params: BorrowedParams<'a>,
    action: char,
) -> ParsedCsiAction<'a> {
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

fn parse_main_plain_csi<'a>(
    modes: &TerminalModes,
    params: BorrowedParams<'a>,
    action: char,
) -> ParsedCsiAction<'a> {
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
                let Ok(ps) = WinManipulationAction::try_from(ps) else {
                    return ParsedCsiAction::Unsupported;
                };

                match ps {
                    WinManipulationAction::TitlePush => {
                        ParsedCsiAction::Main(MainCsiAction::PushTitle)
                    }
                    WinManipulationAction::TitlePop => {
                        ParsedCsiAction::Main(MainCsiAction::PopTitle)
                    }
                    WinManipulationAction::ReportPixels => {
                        ParsedCsiAction::Main(MainCsiAction::ReportPixelSize)
                    }
                    WinManipulationAction::ReportCellSize => {
                        ParsedCsiAction::Main(MainCsiAction::ReportCellSize)
                    }
                    WinManipulationAction::ReportTextSize => {
                        ParsedCsiAction::Main(MainCsiAction::ReportTextSize)
                    }
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

pub(crate) fn csi_parse<'a>(
    screen: &Screen,
    modes: &TerminalModes,
    params: &'a Params,
    intermediates: &[u8],
    action: char,
) -> ParsedCsiAction<'a> {
    let params = BorrowedParams::from_vte(params);
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
            '}' => ParsedCsiAction::SetLocalFunctionKeys { params },
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
        b"+" if action == 'q' => ParsedCsiAction::SetLocalFunctions { params },
        b"+" if action == 'r' => ParsedCsiAction::SetModifierKeyReporting { params },
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
    action: MainCsiAction<'_>,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    pending_output: &mut Vec<u8>,
    pending_resize: &mut Option<(u32, u32)>,
    default_cursor_style: CursorStyle,
    cursor_style: &mut CursorStyle,
    saved_alt_cursor_style: &mut Option<CursorStyle>,
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
    shell_integration_phase: &mut ShellIntegrationPhase,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    udks: &mut UdkState,
    feature_permissions: &FeaturePermissions,
    drcs: &mut DrcsStore,
) {
    let pending_output = &mut *pending_output;
    let screen = &mut *screen;
    let mut viewport = screen::screen_viewport(screen, viewport);
    let preserve_top_origin_scrollback = !*on_alt_screen && !screen::page_memory_active(screen);

    trace!("Applying main CSI action: {action:?}");

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
                    .default_cursor_style(default_cursor_style)
                    .cursor_style(cursor_style)
                    .saved_alt_cursor_style(saved_alt_cursor_style)
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
                    .conformance_level(ConformanceLevel::Level4)
                    .c1_mode(C1Mode::SevenBit)
                    .call();
            }
        }
        MainCsiAction::ReportPrimaryDeviceAttrs => {
            let macro_allowed = feature_permissions.macros.allow();
            let udk_allowed = feature_permissions.udks.allow();
            let level = if macro_allowed || udk_allowed {
                modes.conformance_level.da1_code()
            } else {
                modes.conformance_level.da1_code().min(63)
            };
            let udk_feature = if udk_allowed { ";8" } else { "" };
            let macro_feature = if macro_allowed { ";32" } else { "" };
            conformance::write_csi(
                pending_output,
                modes.c1_mode,
                format_args!("?{level};7{udk_feature};21;22;28;29{macro_feature}c"),
            );
        }
        MainCsiAction::DeviceStatusReport { selector } => {
            let Ok(selector) = DsrParameters::try_from(selector) else {
                return;
            };

            match selector {
                DsrParameters::Ok => {
                    conformance::write_csi(pending_output, modes.c1_mode, format_args!("0n"));
                }
                DsrParameters::Cpr => {
                    let row = screen.cursor.row + 1;
                    let col = screen.cursor.col + 1;
                    conformance::write_csi(
                        pending_output,
                        modes.c1_mode,
                        format_args!("{row};{col}R"),
                    );
                }
            }
        }
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
                    put_char_with_scrollback_policy(
                        screen,
                        &view,
                        ch.clone(),
                        insert,
                        preserve_top_origin_scrollback,
                    );
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
            let cols = row_display_cols(screen, &viewport, screen.cursor.row);
            screen.cursor.col = (screen.cursor.col + count.max(1) as u32).min(cols - 1);
        }
        MainCsiAction::CursorBackward { count } => {
            screen.cursor.col = screen.cursor.col.saturating_sub(count.max(1) as u32);
        }
        MainCsiAction::CursorNextLine { count } => {
            let n = count.max(1) as u32;
            screen.cursor.row = (screen.cursor.row + n).min(viewport.rows - 1);
            screen.cursor.col = 0;
            clamp_cursor_to_row_width(screen, &viewport);
        }
        MainCsiAction::CursorPreviousLine { count } => {
            let n = count.max(1) as u32;
            screen.cursor.row = screen.cursor.row.saturating_sub(n);
            screen.cursor.col = 0;
            clamp_cursor_to_row_width(screen, &viewport);
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
            screen::ensure_cursor_row_exists(screen, &viewport);
            screen.cursor.col = col.min(cols - 1);
        }
        MainCsiAction::EraseInDisplay { mode } => {
            screen::ensure_cursor_row_exists(screen, &viewport);
            grid::erase_in_display(
                &mut screen.grid,
                &screen.cursor,
                &viewport,
                &mut screen.images,
                mode,
            );
        }
        MainCsiAction::EraseInLine { mode } => {
            screen::ensure_cursor_row_exists(screen, &viewport);
            grid::erase_in_line(
                &mut screen.grid,
                &screen.cursor,
                &viewport,
                &mut screen.images,
                mode,
            );
        }
        MainCsiAction::SetGraphicsRendition { params } => {
            apply_sgr_groups(
                &mut screen.fg,
                &mut screen.bg,
                &mut screen.attrs,
                &mut screen.underline_color,
                params,
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
            screen::ensure_cursor_row_exists(screen, &viewport);
            let col = col.max(1) as u32 - 1;
            let cols = current_row_display_cols(screen, &viewport);
            screen.cursor.col = col.min(cols - 1);
        }
        MainCsiAction::CursorForwardRelative { count } => {
            screen::ensure_cursor_row_exists(screen, &viewport);
            let cols = row_display_cols(screen, &viewport, screen.cursor.row);
            screen.cursor.col = (screen.cursor.col + count.max(1) as u32).min(cols - 1);
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
                    grid::scroll_down_in_rect(
                        &mut screen.grid,
                        &viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        screen.left_margin,
                        screen.right_margin,
                        n,
                    );
                } else {
                    grid::scroll_down_in_region(
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
                    grid::scroll_up_in_rect(
                        &mut screen.grid,
                        &viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        screen.left_margin,
                        screen.right_margin,
                        n,
                    );
                } else {
                    grid::scroll_up_in_region_with_scrollback_policy(
                        &mut screen.grid,
                        &viewport,
                        &mut screen.images,
                        top,
                        screen.scroll_bottom,
                        n,
                        preserve_top_origin_scrollback,
                    );
                }
            }
        }
        MainCsiAction::DeleteChars { count } => {
            screen::ensure_cursor_row_exists(screen, &viewport);
            grid::delete_chars(
                &mut screen.grid,
                &mut screen.cursor,
                &viewport,
                &mut screen.images,
                count.max(1),
            );
        }
        MainCsiAction::InsertChars { count } => {
            screen::ensure_cursor_row_exists(screen, &viewport);
            grid::shift_chars(
                &mut screen.grid,
                &mut screen.cursor,
                &viewport,
                &mut screen.images,
                count.max(1),
            );
        }
        MainCsiAction::EraseChars { count } => {
            screen::ensure_cursor_row_exists(screen, &viewport);
            grid::erase_chars(
                &mut screen.grid,
                &mut screen.cursor,
                &viewport,
                &mut screen.images,
                count.max(1),
            );
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
                grid::scroll_up_in_region_with_scrollback_policy(
                    &mut screen.grid,
                    &viewport,
                    &mut screen.images,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                    preserve_top_origin_scrollback,
                );
            }
        }
        MainCsiAction::ScrollDown { count } => {
            grid::scroll_down_in_region(
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
        MainCsiAction::TabClear { mode } => {
            let Ok(mode) = TabClearMode::try_from(mode) else {
                return;
            };

            match mode {
                TabClearMode::Current => {
                    let col = screen.cursor.col as usize;
                    if col < screen.tab_stops.len() {
                        screen.tab_stops[col] = false;
                    }
                }
                TabClearMode::All => screen.tab_stops.fill(false),
            }
        }
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
    action: ParsedCsiAction<'_>,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    pending_output: &mut Vec<u8>,
    pending_resize: &mut Option<(u32, u32)>,
    default_cursor_style: CursorStyle,
    cursor_style: &mut CursorStyle,
    saved_alt_cursor_style: &mut Option<CursorStyle>,
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
    shell_integration_phase: &mut ShellIntegrationPhase,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    macros: &mut MacroStore,
    udks: &mut UdkState,
    feature_permissions: &FeaturePermissions,
    drcs: &mut DrcsStore,
) {
    clamp_cursor_to_row_width(screen, viewport);

    match action {
        ParsedCsiAction::Unsupported => (),
        ParsedCsiAction::StatusLine(action) => {
            apply_status_line_csi(screen, viewport, palette, modes.insert_mode, action);
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
                .default_cursor_style(default_cursor_style)
                .cursor_style(cursor_style)
                .saved_alt_cursor_style(saved_alt_cursor_style)
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
                .shell_integration_phase(shell_integration_phase)
                .bell_pending(bell_pending)
                .vt52_cursor_addr(vt52_cursor_addr)
                .macros(macros)
                .udks(udks)
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
                        saved_alt_cursor_style,
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
                        saved_alt_cursor_style,
                        cursor_style,
                        dec_color,
                        mode,
                        saved,
                    );
                }
            }
        }
        ParsedCsiAction::SelectiveEraseDisplay { mode } => {
            screen::ensure_cursor_row_exists(screen, viewport);
            let view = screen::screen_viewport(screen, viewport);
            grid::erase_in_display_selective(
                &mut screen.grid,
                &screen.cursor,
                &view,
                &mut screen.images,
                mode,
            );
        }
        ParsedCsiAction::SelectiveEraseLine { mode } => {
            screen::ensure_cursor_row_exists(screen, viewport);
            let view = screen::screen_viewport(screen, viewport);
            grid::erase_in_line_selective(
                &mut screen.grid,
                &screen.cursor,
                &view,
                &mut screen.images,
                mode,
            );
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
            if selector == DsrParameters::Cpr as u16 {
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
            } else if selector == 25 && feature_permissions.udks.allow() {
                let status = if udks.locked() { 20 } else { 21 };
                conformance::write_csi(pending_output, modes.c1_mode, format_args!("?{status}n"));
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
                    ParsedCsiAction::ChangeRectAttrs { .. } => grid::change_attrs_rect(
                        &mut screen.grid,
                        &view,
                        start_row,
                        start_col,
                        end_row,
                        end_col,
                        &sgr,
                        screen.attr_change_extent,
                    ),
                    ParsedCsiAction::ReverseRectAttrs { .. } => grid::reverse_attrs_rect(
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
                    grid::erase_rect(
                        &mut screen.grid,
                        &view,
                        &mut screen.images,
                        rect_top,
                        rect_left,
                        rect_bottom,
                        rect_right,
                    );
                }
                ParsedCsiAction::SelectiveEraseRect { .. } => {
                    grid::erase_rect_selective(
                        &mut screen.grid,
                        &view,
                        &mut screen.images,
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
                        grid::fill_rect(
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
                    grid::copy_rect(
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
                    grid::change_attrs_rect(
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
                    grid::reverse_attrs_rect(
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
            if style == 0 {
                *cursor_style = default_cursor_style;
            } else {
                DecCusr::apply(style, cursor_style);
            }
        }
        ParsedCsiAction::ScrollLeft { count } => {
            let view = screen::screen_viewport(screen, viewport);
            let n = count.max(1) as u32;
            grid::scroll_left(
                &mut screen.grid,
                &view,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                n,
            );
        }
        ParsedCsiAction::ScrollRight { count } => {
            let view = screen::screen_viewport(screen, viewport);
            let n = count.max(1) as u32;
            grid::scroll_right(
                &mut screen.grid,
                &view,
                &mut screen.images,
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
            grid::insert_cols(
                &mut screen.grid,
                &view,
                &mut screen.images,
                screen.cursor.col,
                screen.scroll_top,
                screen.scroll_bottom,
                count.max(1) as u32,
            );
        }
        ParsedCsiAction::DeleteColumns { count } => {
            let view = screen::screen_viewport(screen, viewport);
            grid::delete_cols(
                &mut screen.grid,
                &view,
                &mut screen.images,
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
            *cursor_style = default_cursor_style;
            *saved_alt_cursor_style = None;
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
                .default_cursor_style(default_cursor_style)
                .cursor_style(cursor_style)
                .saved_alt_cursor_style(saved_alt_cursor_style)
                .current_title(current_title)
                .title_stack(title_stack)
                .saved_modes(saved_modes)
                .current_prompt_row(current_prompt_row)
                .shell_integration_phase(shell_integration_phase)
                .bell_pending(bell_pending)
                .vt52_cursor_addr(vt52_cursor_addr)
                .dec_color(dec_color)
                .default_status_display(default_status_display)
                .macros(macros)
                .udks(udks)
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
        ParsedCsiAction::SetLocalFunctions { params } => {
            if feature_permissions.udks.allow() {
                let groups: Vec<&[u16]> = params.iter().collect();
                udks.set_local_functions(&groups);
            }
        }
        ParsedCsiAction::SetLocalFunctionKeys { params } => {
            if feature_permissions.udks.allow() {
                let groups: Vec<&[u16]> = params.iter().collect();
                udks.set_local_function_keys(&groups);
            }
        }
        ParsedCsiAction::SetModifierKeyReporting { params } => {
            if feature_permissions.udks.allow() {
                let groups: Vec<&[u16]> = params.iter().collect();
                udks.set_modifier_keys(&groups);
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
    default_cursor_style: CursorStyle,
    cursor_style: &mut CursorStyle,
    saved_alt_cursor_style: &mut Option<CursorStyle>,
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
    udks: &mut UdkState,
    feature_permissions: &FeaturePermissions,
    drcs: &mut DrcsStore,
) {
    let action = csi_parse(screen, modes, params, intermediates, action);
    let mut shell_integration_phase = ShellIntegrationPhase::None;
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
        .default_cursor_style(default_cursor_style)
        .cursor_style(cursor_style)
        .saved_alt_cursor_style(saved_alt_cursor_style)
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
        .shell_integration_phase(&mut shell_integration_phase)
        .bell_pending(bell_pending)
        .vt52_cursor_addr(vt52_cursor_addr)
        .macros(macros)
        .udks(udks)
        .feature_permissions(feature_permissions)
        .drcs(drcs)
        .call();
}

#[cfg(test)]
mod tests;

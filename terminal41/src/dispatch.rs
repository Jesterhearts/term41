use std::collections::HashMap;

use pty_pipe41::ForegroundProcessSet;
use smol_str::SmolStr;
use vtepp::Action;
use vtepp::Intermediates;
use vtepp::Params;

use crate::C1Mode;
use crate::DecColorSpace;
use crate::DecColorState;
use crate::FeaturePermissions;
use crate::Terminal;
use crate::TerminalModes;
use crate::Vt52CursorAddr;
use crate::color::ColorPalette;
use crate::conformance;
use crate::cursor::CursorStyle;
use crate::dec::color::TEXT_COLOR_ASSIGNMENT_CLASS;
use crate::dec::color::assign_color;
use crate::dec::color::effective_palette;
use crate::dec_assign_alternate_text_color;
use crate::dec_select_lookup_table;
use crate::graphics;
use crate::io::keyboard::KittyKeyboardState;
use crate::osc::OscContext;
use crate::osc::handle_osc;
use crate::parser;
use crate::parser::CsiContext;
use crate::parser::EscContext;
use crate::parser::csi_dispatch;
use crate::parser::esc_dispatch;
use crate::parser::execute;
use crate::parser::execute_status;
use crate::parser::put_8bit_byte;
use crate::parser::put_ascii_run;
use crate::parser::put_printable;
use crate::parser::put_status_8bit_byte;
use crate::parser::put_status_ascii_run;
use crate::parser::put_status_printable;
use crate::parser::put_status_text_run;
use crate::parser::put_text_run;
use crate::report;
use crate::report_color_table;
use crate::screen;
use crate::screen::Screen;
use crate::screen::StatusDisplayKind;
use crate::screen::grid::Viewport;
use crate::screen::palette_sync::apply_screen_palette;
use crate::screen::palette_sync::sync_screen_erase_defaults;

#[derive(Debug)]
pub(super) enum SpecialCsi {
    InvokeMacro(u16),
    AssignDecColor { item: u16, fg: u16, bg: u16 },
    AssignDecAlternateTextColor { item: u16, fg: u16, bg: u16 },
    SelectDecLookupTable(u16),
    ReportTerminalState,
    ReportColorTable(DecColorSpace),
}

#[derive(Debug)]
pub(super) enum DecodedAction<'a> {
    Ignore,
    Vt52CursorPosition {
        row: u32,
        col: u32,
        trailing_ascii: &'a [u8],
    },
    PrintAscii(&'a [u8]),
    PrintText(&'a str),
    Print(SmolStr),
    Print8Bit(u8),
    Execute(u8),
    SpecialCsi(SpecialCsi),
    Csi {
        params: Params,
        intermediates: Intermediates,
        action: char,
    },
    Esc {
        intermediates: Intermediates,
        byte: u8,
    },
    Osc(Vec<u8>),
    ItermGraphics(Vec<u8>),
    KittyGraphics(Vec<u8>),
}

pub(super) fn decode_action<'a>(
    vt52_mode: bool,
    vt52_cursor_addr: &mut Vt52CursorAddr,
    action: Action<'a>,
) -> DecodedAction<'a> {
    if let Some(decoded) = decode_vt52_cursor_action(vt52_cursor_addr, &action) {
        return decoded;
    }

    if vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
        return DecodedAction::Ignore;
    }

    match action {
        Action::PrintAscii(run) => DecodedAction::PrintAscii(run),
        Action::PrintText(run) => DecodedAction::PrintText(run),
        Action::Print(text) => DecodedAction::Print(text),
        Action::Print8Bit(byte) => DecodedAction::Print8Bit(byte),
        Action::Execute(byte) => DecodedAction::Execute(byte),
        Action::CsiDispatch {
            params,
            intermediates,
            action,
        } => decode_csi_action(params, intermediates, action).unwrap_or(DecodedAction::Csi {
            params,
            intermediates,
            action,
        }),
        Action::EscDispatch {
            intermediates,
            byte,
        } => DecodedAction::Esc {
            intermediates,
            byte,
        },
        Action::OscDispatch(data) => decode_osc_action(data),
        Action::ApcDispatch(data) => DecodedAction::KittyGraphics(data),
        Action::Hook { .. } | Action::Put(_) | Action::Unhook => DecodedAction::Ignore,
    }
}

pub(super) fn apply_vt52_cursor_position(
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
    row: u32,
    col: u32,
    trailing_ascii: &[u8],
) {
    active.cursor.row = row.min(viewport.rows.saturating_sub(1));
    active.cursor.col = col.min(viewport.cols.saturating_sub(1));
    if !trailing_ascii.is_empty() {
        let view = screen::screen_viewport(active, viewport);
        put_ascii_run(active, &view, trailing_ascii, insert_mode);
    }
}

pub(super) fn apply_ascii_run(
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
    run: &[u8],
) {
    if active.active_display == screen::ActiveDisplay::Status
        && screen::status_line_writable(active)
    {
        put_status_ascii_run(active, run, insert_mode);
    } else {
        let view = screen::screen_viewport(active, viewport);
        put_ascii_run(active, &view, run, insert_mode);
    }
}

pub(super) fn apply_text_run(
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
    run: &str,
) {
    if active.active_display == screen::ActiveDisplay::Status
        && screen::status_line_writable(active)
    {
        put_status_text_run(active, run, insert_mode);
    } else {
        let view = screen::screen_viewport(active, viewport);
        put_text_run(active, &view, run, insert_mode);
    }
}

pub(super) fn apply_printable(
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
    text: SmolStr,
) {
    if active.active_display == screen::ActiveDisplay::Status
        && screen::status_line_writable(active)
    {
        put_status_printable(active, text, insert_mode);
    } else {
        let view = screen::screen_viewport(active, viewport);
        put_printable(active, &view, text, insert_mode);
    }
}

pub(super) fn apply_8bit_byte(
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
    byte: u8,
) {
    if active.active_display == screen::ActiveDisplay::Status
        && screen::status_line_writable(active)
    {
        put_status_8bit_byte(active, byte, insert_mode);
    } else {
        let view = screen::screen_viewport(active, viewport);
        put_8bit_byte(active, &view, byte, insert_mode);
    }
}

pub(super) fn apply_execute(
    active: &mut Screen,
    viewport: &Viewport,
    bell_pending: &mut bool,
    newline_mode: bool,
    byte: u8,
) {
    if active.active_display == screen::ActiveDisplay::Status
        && screen::status_line_writable(active)
    {
        execute_status(active, byte, bell_pending, newline_mode);
    } else {
        let view = screen::screen_viewport(active, viewport);
        execute(active, &view, byte, bell_pending, newline_mode);
    }
}

pub(super) fn apply_special_csi(
    terminal: &mut Terminal,
    special: SpecialCsi,
) {
    match special {
        SpecialCsi::InvokeMacro(id) => invoke_macro(terminal, id),
        SpecialCsi::AssignDecColor { item, fg, bg } => {
            assign_dec_color(
                &mut terminal.active,
                &mut terminal.stash,
                &mut terminal.palette,
                &terminal.base_palette,
                &mut terminal.dec_color,
                item,
                fg,
                bg,
            );
        }
        SpecialCsi::AssignDecAlternateTextColor { item, fg, bg } => {
            dec_assign_alternate_text_color(&mut terminal.dec_color, item, fg, bg);
        }
        SpecialCsi::SelectDecLookupTable(selection) => {
            dec_select_lookup_table(&mut terminal.dec_color, selection);
        }
        SpecialCsi::ReportTerminalState => {
            write_terminal_state_report(
                &terminal.active,
                &mut terminal.output.pending_output,
                terminal.modes.c1_mode,
            );
        }
        SpecialCsi::ReportColorTable(space) => {
            write_color_table_report(
                &terminal.dec_color,
                &mut terminal.output.pending_output,
                terminal.modes.c1_mode,
                space,
            );
        }
    }
}

pub(super) fn apply_csi(
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
    saved_modes: &mut HashMap<u16, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut Vt52CursorAddr,
    macros: &mut crate::dec::r#macro::MacroStore,
    feature_permissions: &FeaturePermissions,
    foreground_processes: &Option<ForegroundProcessSet>,
    drcs: &mut crate::drcs::Store,
    params: &Params,
    intermediates: &[u8],
    action: char,
) {
    let mut ctx = CsiContext {
        screen,
        stash,
        viewport,
        on_alt_screen,
        modes,
        kitty_keyboard,
        pending_output,
        pending_resize,
        cursor_style,
        cell_width,
        cell_height,
        colors: parser::PaletteContext {
            palette,
            base_palette,
            dec_color,
        },
        default_status_display,
        title_stack,
        current_title,
        saved_modes,
        current_prompt_row,
        bell_pending,
        vt52_cursor_addr,
        macros,
        feature_permissions,
        foreground_processes,
        drcs,
    };
    csi_dispatch(&mut ctx, params, intermediates, action);
}

pub(super) fn apply_esc(
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    cursor_style: &mut CursorStyle,
    current_title: &mut Option<String>,
    title_stack: &mut Vec<Option<String>>,
    saved_modes: &mut HashMap<u16, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &mut StatusDisplayKind,
    pending_output: &mut Vec<u8>,
    vt52_cursor_addr: &mut Vt52CursorAddr,
    macros: &mut crate::dec::r#macro::MacroStore,
    drcs: &mut crate::drcs::Store,
    intermediates: &[u8],
    byte: u8,
) {
    let mut ctx = EscContext {
        screen,
        stash,
        viewport,
        on_alt_screen,
        modes,
        kitty_keyboard,
        cursor_style,
        current_title,
        title_stack,
        saved_modes,
        current_prompt_row,
        bell_pending,
        colors: parser::PaletteContext {
            palette,
            base_palette,
            dec_color,
        },
        default_status_display,
        pending_output,
        vt52_cursor_addr,
        macros,
        drcs,
    };
    esc_dispatch(&mut ctx, intermediates, byte);
}

pub(super) fn apply_osc(
    clipboard: &mut clip41::Clipboard,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    current_directory: &mut Option<std::path::PathBuf>,
    hyperlinks: &mut crate::screen::hyperlink::HyperlinkRegistry,
    active_screen: &mut Screen,
    viewport: &Viewport,
    current_title: &mut Option<String>,
    current_prompt_row: &mut Option<u64>,
    command_metas: &mut HashMap<u64, crate::CommandMeta>,
    palette: &ColorPalette,
    cell_width: u32,
    cell_height: u32,
    data: &[u8],
) {
    let mut ctx = OscContext {
        clipboard,
        pending_output,
        c1_mode,
        current_directory,
        hyperlinks,
        active_screen,
        viewport,
        current_title,
        current_prompt_row,
        command_metas,
        palette,
        cell_width,
        cell_height,
    };
    handle_osc(data, &mut ctx);
}

pub(super) fn apply_iterm_graphics(
    chunked: &mut image41::iterm::ChunkedTransmission,
    active: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    data: &[u8],
) {
    graphics::handle_iterm_graphics(
        data.strip_prefix(b"1337;").expect("OSC 1337 prefix"),
        chunked,
        active,
        viewport,
        next_image_id,
        cell_height,
        cell_width,
    );
}

pub(super) fn apply_kitty_graphics(
    kitty_images: &mut image41::kitty::KittyImageStore,
    kitty_chunked: &mut image41::kitty::ChunkedTransmission,
    active: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
    data: &[u8],
) {
    graphics::handle_kitty_graphics(
        data,
        kitty_images,
        kitty_chunked,
        active,
        viewport,
        next_image_id,
        cell_height,
        cell_width,
        c1_mode,
        pending_output,
    );
}

fn decode_vt52_cursor_action<'a>(
    vt52_cursor_addr: &mut Vt52CursorAddr,
    action: &Action<'a>,
) -> Option<DecodedAction<'a>> {
    if *vt52_cursor_addr == Vt52CursorAddr::Idle {
        return None;
    }

    let byte_opt = match action {
        Action::PrintAscii(run) => run.first().copied(),
        Action::Execute(byte) => Some(*byte),
        _ => None,
    };

    match (*vt52_cursor_addr, byte_opt) {
        (Vt52CursorAddr::AwaitingRow, Some(byte)) => {
            *vt52_cursor_addr = Vt52CursorAddr::AwaitingCol(byte.saturating_sub(0x20));
            if let Action::PrintAscii(run) = action
                && run.len() >= 2
            {
                *vt52_cursor_addr = Vt52CursorAddr::Idle;
                return Some(DecodedAction::Vt52CursorPosition {
                    row: byte.saturating_sub(0x20) as u32,
                    col: run[1].saturating_sub(0x20) as u32,
                    trailing_ascii: &run[2..],
                });
            }
            Some(DecodedAction::Ignore)
        }
        (Vt52CursorAddr::AwaitingCol(row), Some(byte)) => {
            *vt52_cursor_addr = Vt52CursorAddr::Idle;
            let trailing_ascii = match action {
                Action::PrintAscii(run) if run.len() > 1 => &run[1..],
                _ => &[],
            };
            Some(DecodedAction::Vt52CursorPosition {
                row: row as u32,
                col: byte.saturating_sub(0x20) as u32,
                trailing_ascii,
            })
        }
        _ => {
            *vt52_cursor_addr = Vt52CursorAddr::Idle;
            None
        }
    }
}

fn decode_csi_action<'a>(
    params: Params,
    intermediates: Intermediates,
    action: char,
) -> Option<DecodedAction<'a>> {
    let special = match (intermediates.as_slice(), action) {
        (b"*", 'z') => SpecialCsi::InvokeMacro(first_param(params)),
        (b",", '|') => {
            let Some((item, fg, bg)) = first_triplet(params) else {
                return Some(DecodedAction::Ignore);
            };
            SpecialCsi::AssignDecColor { item, fg, bg }
        }
        (b",", '}') => {
            let Some((item, fg, bg)) = first_triplet(params) else {
                return Some(DecodedAction::Ignore);
            };
            SpecialCsi::AssignDecAlternateTextColor { item, fg, bg }
        }
        (b")", '{') => SpecialCsi::SelectDecLookupTable(first_param(params)),
        (b"$", 'u') => match first_param(params) {
            1 => SpecialCsi::ReportTerminalState,
            2 => {
                let space = params
                    .iter()
                    .nth(1)
                    .and_then(|group| group.first().copied())
                    .and_then(|space| DecColorSpace::from_param(Some(space)));
                let Some(space) = space else {
                    return Some(DecodedAction::Ignore);
                };
                SpecialCsi::ReportColorTable(space)
            }
            _ => return Some(DecodedAction::Ignore),
        },
        _ => return None,
    };
    Some(DecodedAction::SpecialCsi(special))
}

fn decode_osc_action<'a>(data: Vec<u8>) -> DecodedAction<'a> {
    if let Some(rest) = data.strip_prefix(b"1337;")
        && graphics::is_iterm_image_cmd(rest)
    {
        return DecodedAction::ItermGraphics(data);
    }
    DecodedAction::Osc(data)
}

fn write_terminal_state_report(
    active: &Screen,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
) {
    let payload = report::dectsr_payload(active);
    conformance::push_dcs_prefix(pending_output, c1_mode);
    pending_output.extend_from_slice(b"1$s");
    pending_output.extend_from_slice(&payload);
    conformance::push_st(pending_output, c1_mode);
}

fn write_color_table_report(
    dec_color: &DecColorState,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    space: DecColorSpace,
) {
    let report = report_color_table(dec_color, space);
    conformance::write_dcs(pending_output, c1_mode, format_args!("2$s{report}"));
}

fn assign_dec_color(
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    item: u16,
    fg: u16,
    bg: u16,
) {
    if !assign_color(dec_color, item, fg, bg) {
        return;
    }
    if item == TEXT_COLOR_ASSIGNMENT_CLASS {
        apply_dec_color_defaults(active, stash, palette, base_palette, dec_color);
    }
}

fn apply_dec_color_defaults(
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &DecColorState,
) {
    let old_palette = palette.clone();
    *palette = effective_palette(base_palette, dec_color);
    for screen in [active, stash] {
        apply_screen_palette(screen, &old_palette, palette);
        sync_screen_erase_defaults(screen, dec_color);
    }
}

fn invoke_macro(
    terminal: &mut Terminal,
    id: u16,
) {
    let enabled = crate::feature::macro_feature_enabled(
        &terminal.protocol.feature_permissions,
        terminal.protocol.foreground_processes.as_ref(),
    );
    let Some(bytes) = crate::feature::invoke_macro(
        enabled,
        &terminal.protocol.macros,
        terminal.protocol.macro_invocation_depth,
        id,
    ) else {
        return;
    };
    terminal.protocol.macro_invocation_depth += 1;
    crate::feature::apply_macro_bytes(terminal, &bytes);
    terminal.protocol.macro_invocation_depth -= 1;
}

fn first_param(params: Params) -> u16 {
    params
        .iter()
        .next()
        .and_then(|group| group.first().copied())
        .unwrap_or(0)
}

fn first_triplet(params: Params) -> Option<(u16, u16, u16)> {
    let mut groups = params.iter();
    let item = groups.next().and_then(|group| group.first().copied())?;
    let fg = groups.next().and_then(|group| group.first().copied())?;
    let bg = groups.next().and_then(|group| group.first().copied())?;
    Some((item, fg, bg))
}

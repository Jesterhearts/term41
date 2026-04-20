use std::collections::HashMap;
use std::path::PathBuf;

use clip41::Clipboard;
use pty_pipe41::ForegroundProcessSet;
use smol_str::SmolStr;
use vtepp::Action;
use vtepp::Intermediates;
use vtepp::Params;

use crate::C1Mode;
use crate::CommandMeta;
use crate::CursorStyle;
use crate::DecColorSpace;
use crate::DecColorState;
use crate::FeaturePermissions;
use crate::KittyKeyboardState;
use crate::TerminalModes;
use crate::Vt52CursorAddr;
use crate::color::ColorPalette;
use crate::conformance;
use crate::dec::color::TEXT_COLOR_ASSIGNMENT_CLASS;
use crate::dec::color::assign_color;
use crate::dec::color::effective_palette;
use crate::dec::r#macro::MacroStore;
use crate::dec_assign_alternate_text_color;
use crate::dec_select_lookup_table;
use crate::graphics;
use crate::osc::handle_osc;
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
use crate::screen::hyperlink::HyperlinkRegistry;
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
pub(super) enum TerminalAction<'a> {
    Ignore,
    Basic(BasicAction<'a>),
    Vt52(Vt52Action<'a>),
    Csi(CsiAction),
    Esc(EscAction),
    Osc(OscAction),
    Apc(ApcAction),
}

#[derive(Debug)]
pub(super) enum BasicAction<'a> {
    PrintAscii(&'a [u8]),
    PrintText(&'a str),
    Print(SmolStr),
    Print8Bit(u8),
    Execute(u8),
}

#[derive(Debug)]
pub(super) enum Vt52Action<'a> {
    AwaitCursorColumn,
    CursorPosition {
        row: u32,
        col: u32,
        trailing_ascii: &'a [u8],
    },
}

#[derive(Debug)]
pub(super) enum CsiAction {
    Ignore,
    Special(SpecialCsi),
    Dispatch {
        params: Params,
        intermediates: Intermediates,
        action: char,
    },
}

#[derive(Debug)]
pub(super) enum EscAction {
    Dispatch {
        intermediates: Intermediates,
        byte: u8,
    },
}

#[derive(Debug)]
pub(super) enum OscAction {
    Standard(Vec<u8>),
    ItermGraphics(Vec<u8>),
}

#[derive(Debug)]
pub(super) enum ApcAction {
    KittyGraphics(Vec<u8>),
}

pub(crate) enum PendingApplication {
    None,
    Bytes(Vec<u8>),
}

pub(super) fn classify_action<'a>(
    vt52_mode: bool,
    vt52_cursor_addr: &mut Vt52CursorAddr,
    action: Action<'a>,
) -> TerminalAction<'a> {
    if let Some(vt52_action) = classify_vt52_cursor_action(vt52_cursor_addr, &action) {
        return TerminalAction::Vt52(vt52_action);
    }

    if vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
        return TerminalAction::Ignore;
    }

    match action {
        Action::PrintAscii(run) => TerminalAction::Basic(BasicAction::PrintAscii(run)),
        Action::PrintText(run) => TerminalAction::Basic(BasicAction::PrintText(run)),
        Action::Print(text) => TerminalAction::Basic(BasicAction::Print(text)),
        Action::Print8Bit(byte) => TerminalAction::Basic(BasicAction::Print8Bit(byte)),
        Action::Execute(byte) => TerminalAction::Basic(BasicAction::Execute(byte)),
        Action::CsiDispatch {
            params,
            intermediates,
            action,
        } => TerminalAction::Csi(classify_csi_action(params, intermediates, action)),
        Action::EscDispatch {
            intermediates,
            byte,
        } => TerminalAction::Esc(EscAction::Dispatch {
            intermediates,
            byte,
        }),
        Action::OscDispatch(data) => TerminalAction::Osc(classify_osc_action(data)),
        Action::ApcDispatch(data) => TerminalAction::Apc(ApcAction::KittyGraphics(data)),
        Action::Hook { .. } | Action::Put(_) | Action::Unhook => TerminalAction::Ignore,
    }
}

pub(super) fn apply_basic_action(
    action: BasicAction<'_>,
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
    newline_mode: bool,
    bell_pending: &mut bool,
) {
    match action {
        BasicAction::PrintAscii(run) => apply_ascii_run(active, viewport, insert_mode, run),
        BasicAction::PrintText(run) => apply_text_run(active, viewport, insert_mode, run),
        BasicAction::Print(text) => apply_printable(active, viewport, insert_mode, text),
        BasicAction::Print8Bit(byte) => apply_8bit_byte(active, viewport, insert_mode, byte),
        BasicAction::Execute(byte) => {
            apply_execute(active, viewport, bell_pending, newline_mode, byte)
        }
    }
}

pub(super) fn apply_vt52_action(
    action: Vt52Action<'_>,
    active: &mut Screen,
    viewport: &Viewport,
    insert_mode: bool,
) {
    match action {
        Vt52Action::AwaitCursorColumn => {}
        Vt52Action::CursorPosition {
            row,
            col,
            trailing_ascii,
        } => apply_vt52_cursor_position(active, viewport, insert_mode, row, col, trailing_ascii),
    }
}

pub(super) fn apply_csi_action(
    action: CsiAction,
    active: &mut Screen,
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
    default_status_display: &mut StatusDisplayKind,
    title_stack: &mut Vec<Option<String>>,
    current_title: &mut Option<String>,
    saved_modes: &mut HashMap<u16, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut Vt52CursorAddr,
    macros: &mut MacroStore,
    macro_invocation_depth: usize,
    feature_permissions: &FeaturePermissions,
    foreground_processes: &Option<ForegroundProcessSet>,
    drcs: &mut crate::drcs::Store,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
) -> PendingApplication {
    match action {
        CsiAction::Ignore => PendingApplication::None,
        CsiAction::Special(special) => apply_special_csi()
            .special(special)
            .active(active)
            .stash(stash)
            .palette(palette)
            .base_palette(base_palette)
            .dec_color(dec_color)
            .pending_output(pending_output)
            .c1_mode(modes.c1_mode)
            .feature_permissions(feature_permissions)
            .foreground_processes(foreground_processes)
            .macros(macros)
            .macro_invocation_depth(macro_invocation_depth)
            .call(),
        CsiAction::Dispatch {
            params,
            intermediates,
            action,
        } => {
            csi_dispatch()
                .params(&params)
                .intermediates(intermediates.as_slice())
                .action(action)
                .screen(active)
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
                .default_status_display(default_status_display)
                .title_stack(title_stack)
                .current_title(current_title)
                .saved_modes(saved_modes)
                .current_prompt_row(current_prompt_row)
                .bell_pending(bell_pending)
                .vt52_cursor_addr(vt52_cursor_addr)
                .macros(macros)
                .feature_permissions(feature_permissions)
                .foreground_processes(foreground_processes)
                .drcs(drcs)
                .palette(palette)
                .base_palette(base_palette)
                .dec_color(dec_color)
                .call();
            PendingApplication::None
        }
    }
}

pub(super) fn apply_esc_action(
    action: EscAction,
    active: &mut Screen,
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
    macros: &mut MacroStore,
    drcs: &mut crate::drcs::Store,
) {
    match action {
        EscAction::Dispatch {
            intermediates,
            byte,
        } => {
            esc_dispatch()
                .screen(active)
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
                .intermediates(intermediates.as_slice())
                .byte(byte)
                .call();
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_osc_action(
    action: OscAction,
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    current_directory: &mut Option<PathBuf>,
    hyperlinks: &mut HyperlinkRegistry,
    active: &mut Screen,
    viewport: &Viewport,
    current_title: &mut Option<String>,
    current_prompt_row: &mut Option<u64>,
    command_metas: &mut HashMap<u64, CommandMeta>,
    palette: &ColorPalette,
    cell_width: u32,
    cell_height: u32,
    iterm_chunked: &mut image41::iterm::ChunkedTransmission,
    next_image_id: &mut u64,
) {
    match action {
        OscAction::Standard(data) => {
            handle_osc()
                .payload(&data)
                .clipboard(clipboard)
                .pending_output(pending_output)
                .c1_mode(c1_mode)
                .current_directory(current_directory)
                .hyperlinks(hyperlinks)
                .active_screen(active)
                .viewport(viewport)
                .current_title(current_title)
                .current_prompt_row(current_prompt_row)
                .command_metas(command_metas)
                .palette(palette)
                .cell_width(cell_width)
                .cell_height(cell_height)
                .call();
        }
        OscAction::ItermGraphics(data) => {
            apply_iterm_graphics(
                iterm_chunked,
                active,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
                &data,
            );
        }
    }
}

pub(super) fn apply_apc_action(
    action: ApcAction,
    kitty_images: &mut image41::kitty::KittyImageStore,
    kitty_chunked: &mut image41::kitty::ChunkedTransmission,
    active: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    match action {
        ApcAction::KittyGraphics(data) => apply_kitty_graphics(
            kitty_images,
            kitty_chunked,
            active,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
            c1_mode,
            pending_output,
            &data,
        ),
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

#[bon::builder]
pub(super) fn apply_special_csi(
    special: SpecialCsi,
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    feature_permissions: &FeaturePermissions,
    foreground_processes: &Option<ForegroundProcessSet>,
    macros: &crate::dec::r#macro::MacroStore,
    macro_invocation_depth: usize,
) -> PendingApplication {
    match special {
        SpecialCsi::InvokeMacro(id) => invoke_macro(
            feature_permissions,
            foreground_processes,
            macros,
            macro_invocation_depth,
            id,
        ),
        SpecialCsi::AssignDecColor { item, fg, bg } => {
            assign_dec_color(
                active,
                stash,
                palette,
                base_palette,
                dec_color,
                item,
                fg,
                bg,
            );
            PendingApplication::None
        }
        SpecialCsi::AssignDecAlternateTextColor { item, fg, bg } => {
            dec_assign_alternate_text_color(dec_color, item, fg, bg);
            PendingApplication::None
        }
        SpecialCsi::SelectDecLookupTable(selection) => {
            dec_select_lookup_table(dec_color, selection);
            PendingApplication::None
        }
        SpecialCsi::ReportTerminalState => {
            write_terminal_state_report(active, pending_output, c1_mode);
            PendingApplication::None
        }
        SpecialCsi::ReportColorTable(space) => {
            write_color_table_report(dec_color, pending_output, c1_mode, space);
            PendingApplication::None
        }
    }
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

fn classify_vt52_cursor_action<'a>(
    vt52_cursor_addr: &mut Vt52CursorAddr,
    action: &Action<'a>,
) -> Option<Vt52Action<'a>> {
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
                return Some(Vt52Action::CursorPosition {
                    row: byte.saturating_sub(0x20) as u32,
                    col: run[1].saturating_sub(0x20) as u32,
                    trailing_ascii: &run[2..],
                });
            }
            Some(Vt52Action::AwaitCursorColumn)
        }
        (Vt52CursorAddr::AwaitingCol(row), Some(byte)) => {
            *vt52_cursor_addr = Vt52CursorAddr::Idle;
            let trailing_ascii = match action {
                Action::PrintAscii(run) if run.len() > 1 => &run[1..],
                _ => &[],
            };
            Some(Vt52Action::CursorPosition {
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

fn classify_csi_action(
    params: Params,
    intermediates: Intermediates,
    action: char,
) -> CsiAction {
    let special = match (intermediates.as_slice(), action) {
        (b"*", 'z') => SpecialCsi::InvokeMacro(first_param(params)),
        (b",", '|') => {
            let Some((item, fg, bg)) = first_triplet(params) else {
                return CsiAction::Ignore;
            };
            SpecialCsi::AssignDecColor { item, fg, bg }
        }
        (b",", '}') => {
            let Some((item, fg, bg)) = first_triplet(params) else {
                return CsiAction::Ignore;
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
                    return CsiAction::Ignore;
                };
                SpecialCsi::ReportColorTable(space)
            }
            _ => return CsiAction::Ignore,
        },
        _ => {
            return CsiAction::Dispatch {
                params,
                intermediates,
                action,
            };
        }
    };
    CsiAction::Special(special)
}

fn classify_osc_action(data: Vec<u8>) -> OscAction {
    if let Some(rest) = data.strip_prefix(b"1337;")
        && graphics::is_iterm_image_cmd(rest)
    {
        return OscAction::ItermGraphics(data);
    }
    OscAction::Standard(data)
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
    feature_permissions: &FeaturePermissions,
    foreground_processes: &Option<ForegroundProcessSet>,
    macros: &crate::dec::r#macro::MacroStore,
    macro_invocation_depth: usize,
    id: u16,
) -> PendingApplication {
    let enabled =
        crate::feature::macro_feature_enabled(feature_permissions, foreground_processes.as_ref());
    let Some(bytes) = crate::feature::invoke_macro(enabled, macros, macro_invocation_depth, id)
    else {
        return PendingApplication::None;
    };
    PendingApplication::Bytes(bytes)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_standard_csi_as_csi_dispatch() {
        let mut parser = vtepp::Parser::new();
        let action = parser.parse(b"\x1b[31m").next().expect("parsed action");
        let classified = classify_action(false, &mut Vt52CursorAddr::Idle, action);
        assert!(matches!(
            classified,
            TerminalAction::Csi(CsiAction::Dispatch { action: 'm', .. })
        ));
    }

    #[test]
    fn classify_special_csi_as_special_variant() {
        let mut parser = vtepp::Parser::new();
        let action = parser.parse(b"\x1b[1$u").next().expect("parsed action");
        let classified = classify_action(false, &mut Vt52CursorAddr::Idle, action);
        assert!(matches!(
            classified,
            TerminalAction::Csi(CsiAction::Special(SpecialCsi::ReportTerminalState))
        ));
    }

    #[test]
    fn classify_iterm_osc_as_iterm_graphics() {
        let mut parser = vtepp::Parser::new();
        let action = parser
            .parse(b"\x1b]1337;File=name=test:aGVsbG8=\x07")
            .next()
            .expect("parsed action");
        let classified = classify_action(false, &mut Vt52CursorAddr::Idle, action);
        assert!(matches!(
            classified,
            TerminalAction::Osc(OscAction::ItermGraphics(_))
        ));
    }

    #[test]
    fn classify_vt52_cursor_bytes_as_vt52_action() {
        let mut state = Vt52CursorAddr::AwaitingRow;
        let classified = classify_action(false, &mut state, Action::PrintAscii(b"!\"rest"));
        match classified {
            TerminalAction::Vt52(Vt52Action::CursorPosition {
                row,
                col,
                trailing_ascii,
            }) => {
                assert_eq!(row, 1);
                assert_eq!(col, 2);
                assert_eq!(trailing_ascii, b"rest");
            }
            other => panic!("unexpected action: {other:?}"),
        }
        assert_eq!(state, Vt52CursorAddr::Idle);
    }
}

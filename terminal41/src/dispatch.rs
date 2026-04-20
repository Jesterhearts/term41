use smol_str::SmolStr;
use vtepp::Action;
use vtepp::Intermediates;
use vtepp::Params;

use crate::DecColorSpace;
use crate::Vt52CursorAddr;
use crate::graphics;

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

use crate::Terminal;
use crate::TerminalEffects;
use crate::charset;
use crate::dec::udk;
use crate::drcs;
use crate::report;

pub(crate) struct HookState {
    pub(crate) bytes: Vec<u8>,
    pub(crate) params: vtepp::Params,
    pub(crate) intermediates: vtepp::Intermediates,
    pub(crate) action: char,
    pub(crate) truncated: bool,
}

#[derive(Debug)]
enum ParsedDcsAction {
    Ignore,
    XtGetTCaps {
        payload: Vec<u8>,
    },
    RequestStatusString {
        payload: Vec<u8>,
    },
    Sixel {
        params: vtepp::Params,
        payload: Vec<u8>,
    },
    AssignUserPreferredSupplementalSet {
        ps: u16,
        payload: Vec<u8>,
    },
    DefineDrcs {
        params: Vec<u16>,
        payload: Vec<u8>,
    },
    DefineUserDefinedKeys {
        params: vtepp::Params,
        payload: Vec<u8>,
    },
    RestoreCursorInformationReport {
        payload: Vec<u8>,
    },
    RestoreTabStopReport {
        payload: Vec<u8>,
    },
    RestoreTerminalStateReport {
        payload: Vec<u8>,
    },
    RestoreColorTable {
        payload: Vec<u8>,
    },
    DefineMacro {
        params: vtepp::Params,
        payload: Vec<u8>,
    },
}

fn hook_payload_limit(
    action: char,
    intermediates: &[u8],
) -> Option<usize> {
    match (action, intermediates) {
        ('{', []) => Some(drcs::MAX_DRCS_PAYLOAD_BYTES),
        ('|', []) => Some(udk::MAX_DECUDK_PAYLOAD_BYTES),
        _ => None,
    }
}

pub(crate) fn push_hook_state(
    hooks: &mut Vec<HookState>,
    params: vtepp::Params,
    intermediates: vtepp::Intermediates,
    action: char,
) {
    hooks.push(HookState {
        bytes: vec![],
        params,
        intermediates,
        action,
        truncated: false,
    });
}

pub(crate) fn append_hook_bytes(
    hooks: &mut [HookState],
    chunk: &[u8],
) {
    let Some(last) = hooks.last_mut() else {
        return;
    };
    if last.truncated {
        return;
    }
    if let Some(limit) = hook_payload_limit(last.action, last.intermediates.as_slice()) {
        let remaining = limit.saturating_sub(last.bytes.len());
        let take = remaining.min(chunk.len());
        last.bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            last.truncated = true;
        }
    } else {
        last.bytes.extend_from_slice(chunk);
    }
}

pub(crate) fn dispatch_hook(
    hook: HookState,
    terminal: &mut Terminal,
    effects: &mut TerminalEffects,
) {
    let action = parse_dcs_hook(hook);
    apply_dcs_action(action, terminal, effects);
}

fn parse_dcs_hook(hook: HookState) -> ParsedDcsAction {
    let HookState {
        bytes,
        params,
        intermediates,
        action,
        truncated,
    } = hook;

    if truncated {
        return ParsedDcsAction::Ignore;
    }

    match (action, intermediates.as_slice()) {
        ('q', b"+") => ParsedDcsAction::XtGetTCaps { payload: bytes },
        ('q', b"$") => ParsedDcsAction::RequestStatusString { payload: bytes },
        ('q', []) => ParsedDcsAction::Sixel {
            params,
            payload: bytes,
        },
        ('u', b"!") => ParsedDcsAction::AssignUserPreferredSupplementalSet {
            ps: first_param(&params),
            payload: bytes,
        },
        ('{', []) => ParsedDcsAction::DefineDrcs {
            params: flat_params(&params),
            payload: bytes,
        },
        ('|', []) => ParsedDcsAction::DefineUserDefinedKeys {
            params,
            payload: bytes,
        },
        ('t', b"$") => match first_param(&params) {
            1 => ParsedDcsAction::RestoreCursorInformationReport { payload: bytes },
            2 => ParsedDcsAction::RestoreTabStopReport { payload: bytes },
            _ => ParsedDcsAction::Ignore,
        },
        ('p', b"$") => match first_param(&params) {
            1 => ParsedDcsAction::RestoreTerminalStateReport { payload: bytes },
            2 => ParsedDcsAction::RestoreColorTable { payload: bytes },
            _ => ParsedDcsAction::Ignore,
        },
        ('z', b"!") => ParsedDcsAction::DefineMacro {
            params,
            payload: bytes,
        },
        _ => ParsedDcsAction::Ignore,
    }
}

fn first_param(params: &vtepp::Params) -> u16 {
    params
        .iter()
        .next()
        .and_then(|group| group.first().copied())
        .unwrap_or(0)
}

fn flat_params(params: &vtepp::Params) -> Vec<u16> {
    params.iter().flat_map(|g| g.iter().copied()).collect()
}

fn apply_dcs_action(
    action: ParsedDcsAction,
    terminal: &mut Terminal,
    effects: &mut TerminalEffects,
) {
    match action {
        ParsedDcsAction::Ignore => {}
        ParsedDcsAction::XtGetTCaps { payload } => {
            let c1_mode = terminal.modes.c1_mode;
            report::handle_xtgettcap(&payload, c1_mode, &mut effects.host_bytes);
        }
        ParsedDcsAction::RequestStatusString { payload } => {
            report::handle_decrqss(&payload, terminal, &mut effects.host_bytes);
        }
        ParsedDcsAction::Sixel { params, payload } => {
            let image = image41::sixel::parse_sixel(params, payload);
            terminal.place_sixel_image(image);
        }
        ParsedDcsAction::AssignUserPreferredSupplementalSet { ps, payload } => {
            if let Some(upss) = charset::parse_upss_assignment(ps, &payload) {
                for screen in [&mut terminal.active, &mut terminal.stash] {
                    screen.upss = upss;
                }
            }
        }
        ParsedDcsAction::DefineDrcs { params, payload } => {
            terminal.protocol.drcs.define(&params, &payload);
        }
        ParsedDcsAction::DefineUserDefinedKeys { params, payload } => {
            terminal.define_udk(params, &payload);
        }
        ParsedDcsAction::RestoreCursorInformationReport { payload } => {
            report::restore_deccir(
                &payload,
                &mut terminal.active,
                &terminal.viewport,
                &mut terminal.modes,
                &terminal.protocol.drcs,
            );
        }
        ParsedDcsAction::RestoreTabStopReport { payload } => {
            report::restore_dectabsr(&payload, &mut terminal.active);
        }
        ParsedDcsAction::RestoreTerminalStateReport { payload } => {
            report::restore_dectsr(&payload, &mut terminal.active);
        }
        ParsedDcsAction::RestoreColorTable { payload } => {
            terminal.restore_dec_color_table(&payload);
        }
        ParsedDcsAction::DefineMacro { params, payload } => {
            terminal.define_macro(params, &payload);
        }
    }
}

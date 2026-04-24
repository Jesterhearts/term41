use crate::Terminal;
use crate::TerminalEffects;
use crate::charset;
use crate::dec::udk;
use crate::drcs;
use crate::report;

#[derive(Default)]
pub(crate) struct HookAccumulator {
    hooks: Vec<HookState>,
}

pub(crate) enum HookAccumulation<'a> {
    NotDcs(vtepp::Action<'a>),
    Pending,
    Complete(HookState),
}

pub(crate) struct HookState {
    pub(crate) bytes: Vec<u8>,
    pub(crate) params: vtepp::Params,
    pub(crate) intermediates: vtepp::Intermediates,
    pub(crate) action: char,
    pub(crate) truncated: bool,
}

impl HookAccumulator {
    pub(crate) fn consume<'a>(
        &mut self,
        action: vtepp::Action<'a>,
    ) -> HookAccumulation<'a> {
        match action {
            vtepp::Action::Hook {
                params,
                intermediates,
                action,
            } => {
                self.push(params, intermediates, action);
                HookAccumulation::Pending
            }
            vtepp::Action::Put(bytes) => {
                self.append(bytes);
                HookAccumulation::Pending
            }
            vtepp::Action::Unhook => self
                .complete()
                .map(HookAccumulation::Complete)
                .unwrap_or(HookAccumulation::Pending),
            action => HookAccumulation::NotDcs(action),
        }
    }

    fn push(
        &mut self,
        params: vtepp::Params,
        intermediates: vtepp::Intermediates,
        action: char,
    ) {
        self.hooks.push(HookState {
            bytes: vec![],
            params,
            intermediates,
            action,
            truncated: false,
        });
    }

    fn append(
        &mut self,
        chunk: &[u8],
    ) {
        let Some(last) = self.hooks.last_mut() else {
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

    fn complete(&mut self) -> Option<HookState> {
        self.hooks.pop()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_returns_non_dcs_actions_to_caller() {
        let mut accumulator = HookAccumulator::default();
        let mut parser = vtepp::Parser::new();
        let action = parser.parse(b"text").next().expect("print action");
        assert!(matches!(
            accumulator.consume(action),
            HookAccumulation::NotDcs(vtepp::Action::PrintAscii(b"text"))
        ));
    }

    #[test]
    fn accumulator_collects_completed_hook_payload() {
        let mut accumulator = HookAccumulator::default();
        let mut parser = vtepp::Parser::new();
        let mut completed = None;

        for action in parser.parse(b"\x1bP$qpayload\x1b\\") {
            if let HookAccumulation::Complete(hook) = accumulator.consume(action) {
                completed = Some(hook);
            }
        }

        let hook = completed.expect("completed DCS hook");
        assert_eq!(hook.action, 'q');
        assert_eq!(hook.intermediates.as_slice(), b"$");
        assert_eq!(hook.bytes, b"payload");
        assert!(!hook.truncated);
    }

    #[test]
    fn accumulator_truncates_limited_payloads() {
        let mut accumulator = HookAccumulator::default();
        let mut parser = vtepp::Parser::new();
        let payload = vec![b'0'; drcs::MAX_DRCS_PAYLOAD_BYTES + 1];
        let input = [b"\x1bP{" as &[u8], payload.as_slice(), b"\x1b\\"].concat();
        let mut completed = None;

        for action in parser.parse(&input) {
            if let HookAccumulation::Complete(hook) = accumulator.consume(action) {
                completed = Some(hook);
            }
        }

        let hook = completed.expect("completed DCS hook");
        assert_eq!(hook.action, '{');
        assert_eq!(hook.bytes.len(), drcs::MAX_DRCS_PAYLOAD_BYTES);
        assert!(hook.truncated);
    }
}

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
    if hook.truncated {
        return;
    }
    if hook.action == 'q' && hook.intermediates.as_slice() == b"+" {
        let c1_mode = terminal.modes.c1_mode;
        report::handle_xtgettcap(&hook.bytes, c1_mode, &mut effects.host_bytes);
    } else if hook.action == 'q' && hook.intermediates.as_slice() == b"$" {
        report::handle_decrqss(&hook.bytes, terminal, &mut effects.host_bytes);
    } else if hook.action == 'q' && hook.intermediates.as_slice().is_empty() {
        let image = image41::sixel::parse_sixel(hook.params, hook.bytes);
        terminal.place_sixel_image(image);
    } else {
        handle_dcs(
            hook.params,
            hook.intermediates.as_slice(),
            hook.action,
            &hook.bytes,
            terminal,
            effects,
        );
    }
}

fn handle_dcs(
    params: vtepp::Params,
    intermediates: &[u8],
    action: char,
    payload: &[u8],
    terminal: &mut Terminal,
    effects: &mut TerminalEffects,
) {
    if action == 'q' && intermediates == b"+" {
        let c1_mode = terminal.modes.c1_mode;
        report::handle_xtgettcap(payload, c1_mode, &mut effects.host_bytes);
    } else if action == 'q' && intermediates == b"$" {
        report::handle_decrqss(payload, terminal, &mut effects.host_bytes);
    } else if action == 'u' && intermediates == b"!" {
        let ps = params
            .iter()
            .next()
            .and_then(|group| group.first().copied())
            .unwrap_or(0);
        if let Some(upss) = charset::parse_upss_assignment(ps, payload) {
            for screen in [&mut terminal.active, &mut terminal.stash] {
                screen.upss = upss;
            }
        }
    } else if action == '{' && intermediates.is_empty() {
        let flat_params: Vec<u16> = params.iter().flat_map(|g| g.iter().copied()).collect();
        terminal.protocol.drcs.define(&flat_params, payload);
    } else if action == '|' && intermediates.is_empty() {
        terminal.define_udk(params, payload);
    } else if action == 't' && intermediates == b"$" {
        let ps = params
            .iter()
            .next()
            .and_then(|group| group.first().copied())
            .unwrap_or(0);
        match ps {
            1 => {
                report::restore_deccir(
                    payload,
                    &mut terminal.active,
                    &terminal.viewport,
                    &mut terminal.modes,
                    &terminal.protocol.drcs,
                );
            }
            2 => {
                report::restore_dectabsr(payload, &mut terminal.active);
            }
            _ => {}
        }
    } else if action == 'p' && intermediates == b"$" {
        let ps = params
            .iter()
            .next()
            .and_then(|group| group.first().copied())
            .unwrap_or(0);
        match ps {
            1 => {
                report::restore_dectsr(payload, &mut terminal.active);
            }
            2 => {
                terminal.restore_dec_color_table(payload);
            }
            _ => {}
        }
    } else if action == 'z' && intermediates == b"!" {
        terminal.define_macro(params, payload);
    }
}

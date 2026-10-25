//! Borrowed state groups for terminal control-sequence application.

use std::collections::HashMap;

use crate::Terminal;
use crate::TerminalEffects;
use crate::TerminalModes;
use crate::TerminalProtocolState;
use crate::metadata::TerminalMetadata;
use crate::mode::PrivateMode;

pub(crate) struct ScreenBuffers<'a> {
    pub active: &'a mut crate::Screen,
    pub stash: &'a mut crate::Screen,
    pub viewport: &'a mut crate::Viewport,
    pub on_alt_screen: &'a mut bool,
    pub default_status_display: &'a mut crate::StatusDisplayKind,
    pub cell_width: u32,
    pub cell_height: u32,
}

pub(crate) struct CursorStyles<'a> {
    pub default: crate::CursorStyle,
    pub current: &'a mut crate::CursorStyle,
    pub saved_alt: &'a mut Option<crate::CursorStyle>,
}

pub(crate) struct TerminalColors<'a> {
    pub palette: &'a mut crate::ColorPalette,
    pub base: &'a crate::ColorPalette,
    pub dec: &'a mut crate::DecColorState,
    pub overrides: &'a mut crate::RuntimeColorOverrides,
}

pub(crate) struct ControlState<'a> {
    pub screens: ScreenBuffers<'a>,
    pub cursor: CursorStyles<'a>,
    pub colors: TerminalColors<'a>,
    pub modes: &'a mut TerminalModes,
    pub keyboard: &'a mut crate::KittyKeyboardState,
    pub metadata: &'a mut TerminalMetadata,
    pub protocol: &'a mut TerminalProtocolState,
    pub saved_modes: &'a mut HashMap<PrivateMode, bool>,
    pub vt52_cursor_addr: &'a mut crate::Vt52CursorAddr,
}

impl<'a> ControlState<'a> {
    pub fn new(terminal: &'a mut Terminal) -> Self {
        Self {
            screens: ScreenBuffers {
                active: &mut terminal.active,
                stash: &mut terminal.stash,
                viewport: &mut terminal.viewport,
                on_alt_screen: &mut terminal.on_alt_screen,
                default_status_display: &mut terminal.default_status_display,
                cell_width: terminal.cell_width,
                cell_height: terminal.cell_height,
            },
            cursor: CursorStyles {
                default: terminal.default_cursor_style,
                current: &mut terminal.cursor_style,
                saved_alt: &mut terminal.saved_alt_cursor_style,
            },
            colors: TerminalColors {
                palette: &mut terminal.palette,
                base: &terminal.base_palette,
                dec: &mut terminal.dec_color,
                overrides: &mut terminal.runtime_colors,
            },
            modes: &mut terminal.modes,
            keyboard: &mut terminal.kitty_keyboard,
            metadata: &mut terminal.metadata,
            protocol: &mut terminal.protocol,
            saved_modes: &mut terminal.saved_private_modes,
            vt52_cursor_addr: &mut terminal.vt52_cursor_addr,
        }
    }
}

pub(crate) fn apply_csi(
    state: ControlState<'_>,
    action: crate::CsiAction<'_>,
    effects: &mut TerminalEffects,
) -> crate::dispatch::PendingApplication {
    match action {
        crate::CsiAction::Ignore => crate::dispatch::PendingApplication::None,
        crate::CsiAction::Special(special) => crate::dispatch::apply_special_csi()
            .special(special)
            .active(state.screens.active)
            .stash(state.screens.stash)
            .palette(state.colors.palette)
            .base_palette(state.colors.base)
            .dec_color(state.colors.dec)
            .runtime_colors(state.colors.overrides)
            .pending_output(&mut effects.host_bytes)
            .c1_mode(state.modes.c1_mode)
            .feature_permissions(&state.protocol.feature_permissions)
            .limits(state.protocol.limits)
            .macros(&state.protocol.macros)
            .macro_invocation_depth(state.protocol.macro_invocation_depth)
            .call(),
        crate::CsiAction::Parsed(action) => {
            crate::parser::csi_apply(state, action, effects);
            crate::dispatch::PendingApplication::None
        }
    }
}

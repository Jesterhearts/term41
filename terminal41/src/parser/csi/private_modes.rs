use std::time::Instant;

use crate::CursorStyle;
use crate::DecColorState;
use crate::MouseEncoding;
use crate::MouseTracking;
use crate::Screen;
use crate::TerminalModes;
use crate::Viewport;
use crate::charset;
use crate::io::mouse::apply_mouse_mode;
use crate::mode;
use crate::parser::sync_screen_erase_defaults;
use crate::screen;

pub(super) fn apply_private_mode(
    modes: &mut TerminalModes,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    saved_alt_cursor_style: &mut Option<CursorStyle>,
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
        apply_screen_private_mode(
            mode,
            enable,
            screen,
            stash,
            viewport,
            on_alt_screen,
            saved_alt_cursor_style,
            cursor_style,
        );
    }
}

fn apply_screen_private_mode(
    mode: mode::PrivateMode,
    enable: bool,
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    saved_alt_cursor_style: &mut Option<CursorStyle>,
    cursor_style: &mut CursorStyle,
) {
    if mode != mode::PrivateMode::AltScreenSave {
        screen::set_private_mode(mode, enable, screen, stash, viewport, on_alt_screen);
        return;
    }

    if enable && !*on_alt_screen {
        *saved_alt_cursor_style = Some(*cursor_style);
    }
    let saved = (!enable && *on_alt_screen)
        .then(|| saved_alt_cursor_style.take())
        .flatten();
    screen::set_private_mode(mode, enable, screen, stash, viewport, on_alt_screen);
    if let Some(style) = saved {
        *cursor_style = style;
    }
}

pub(super) fn query_private_mode(
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
        mode::PrivateMode::Utf8Mouse => {
            if modes.mouse_encoding == MouseEncoding::Utf8 {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::SgrMouse => {
            if modes.mouse_encoding == MouseEncoding::Sgr {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::UrxvtMouse => {
            if modes.mouse_encoding == MouseEncoding::Urxvt {
                1
            } else {
                2
            }
        }
        mode::PrivateMode::SgrPixelsMouse => {
            if modes.mouse_encoding == MouseEncoding::SgrPixels {
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

pub(super) fn query_private_mode_by_id(
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

pub(super) fn query_ansi_mode_by_id(
    modes: &TerminalModes,
    ps: u16,
) -> u8 {
    let Ok(mode) = mode::AnsiMode::try_from(ps) else {
        return 0;
    };
    query_ansi_mode(modes, mode)
}

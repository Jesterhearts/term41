use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use smol_str::SmolStr;
use unicode_segmentation::UnicodeSegmentation;
use vtepp::Params;

use crate::C1Mode;
use crate::ColorPalette;
use crate::ConformanceLevel;
use crate::TerminalModes;
use crate::TextMode;
use crate::charset;
use crate::charset::CharacterSet;
use crate::charset::GraphicSetSlot;
use crate::color;
use crate::color::apply_sgr_groups;
use crate::conformance;
use crate::cursor::CursorStyle;
use crate::dec::color::DecColorState;
use crate::dec::color::effective_palette;
use crate::dec::color::erase_background_color;
use crate::dec::r#macro::MacroStore;
use crate::dec_color_state_from_palette;
use crate::drcs::Store as DrcsStore;
use crate::feature::FeaturePermissions;
use crate::io::keyboard::KittyKeyboardState;
use crate::io::keyboard::handle_kitty_keyboard_groups;
use crate::io::mouse::MouseTracking;
use crate::io::mouse::apply_mouse_mode;
use crate::mode;
use crate::screen;
use crate::screen::ActiveDisplay;
use crate::screen::Screen;
use crate::screen::StatusDisplayKind;
use crate::screen::StatusLine;
use crate::screen::grid;
use crate::screen::grid::Viewport;
use crate::screen::row::LineAttr;
use crate::screen::row::Row;

mod csi;
mod esc;
mod status;
mod text;
mod write;

pub(crate) use self::csi::csi_apply;
#[cfg(test)]
pub(crate) use self::csi::csi_dispatch;
pub(crate) use self::csi::csi_parse;
pub(crate) use self::esc::esc_apply;
#[cfg(test)]
pub(crate) use self::esc::esc_dispatch;
pub(crate) use self::esc::esc_parse;
pub(crate) use self::status::apply_status_line_csi;
pub(crate) use self::status::execute_status;
#[cfg(test)]
pub(crate) use self::text::execute;
pub(crate) use self::text::execute_with_scrollback_policy;
#[cfg(test)]
pub(crate) use self::write::put_8bit_byte;
pub(crate) use self::write::put_8bit_byte_with_scrollback_policy;
#[cfg(test)]
pub(crate) use self::write::put_ascii_run;
pub(crate) use self::write::put_ascii_run_with_scrollback_policy;
#[cfg(test)]
pub(crate) use self::write::put_char;
pub(crate) use self::write::put_char_with_scrollback_policy;
#[cfg(test)]
pub(crate) use self::write::put_printable;
pub(crate) use self::write::put_printable_with_scrollback_policy;
pub(crate) use self::write::put_status_8bit_byte;
pub(crate) use self::write::put_status_ascii_run;
pub(crate) use self::write::put_status_printable;
pub(crate) use self::write::put_status_text_run;
#[cfg(test)]
pub(crate) use self::write::put_text_run;
pub(crate) use self::write::put_text_run_with_scrollback_policy;

/// Pre-built inline `SmolStr` for every printable ASCII byte (0x20..=0x7E).
/// `put_ascii_run` clones out of this table instead of constructing a fresh
/// `SmolStr` per byte — inline-backed clones are a short memcpy, so the table
/// eliminates repeated `from_utf8` validation and the inline copy constructor
/// call per cell.
static ASCII_CELLS: LazyLock<[SmolStr; 95]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let b = 0x20u8 + i as u8;
        // SAFETY: b is in 0x20..=0x7E which is valid single-byte UTF-8.
        SmolStr::new_inline(unsafe { std::str::from_utf8_unchecked(std::slice::from_ref(&b)) })
    })
});

fn line_attr_display_cols(
    line_attr: LineAttr,
    viewport: &Viewport,
) -> u32 {
    match line_attr {
        LineAttr::Normal => viewport.cols,
        LineAttr::DoubleWidth | LineAttr::DoubleHeightTop | LineAttr::DoubleHeightBottom => {
            (viewport.cols / 2).max(1)
        }
    }
}

fn visible_row_index(
    screen: &Screen,
    viewport: &Viewport,
    row: u32,
) -> usize {
    if screen::page_memory_active(screen) {
        viewport.top_index(screen.grid.rows.len()) + row as usize
    } else {
        screen
            .grid
            .rows
            .len()
            .saturating_sub(viewport.rows as usize)
            + row as usize
    }
}

fn row_display_cols(
    screen: &Screen,
    viewport: &Viewport,
    row: u32,
) -> u32 {
    let row_index = visible_row_index(screen, viewport, row);
    let line_attr = screen
        .grid
        .rows
        .get(row_index)
        .map(|row| row.line_attr)
        .unwrap_or(LineAttr::Normal);
    line_attr_display_cols(line_attr, viewport)
}

fn current_row_display_cols(
    screen: &Screen,
    viewport: &Viewport,
) -> u32 {
    row_display_cols(screen, viewport, screen.cursor.row)
}

fn clamp_cursor_to_row_width(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    let cols = current_row_display_cols(screen, viewport);
    if screen.cursor.col >= cols {
        screen.cursor.col = cols.saturating_sub(1);
    }
}

/// Sentinel for the second (and beyond) cell of a wide glyph. Distinct from
/// the default blank (`" "`) so neighbour cleanup can tell them apart.
fn continuation_cell() -> SmolStr {
    SmolStr::default()
}

fn blank_cell() -> SmolStr {
    SmolStr::new_inline(" ")
}

#[derive(Debug, Clone)]
pub(super) struct OwnedParams(Vec<Vec<u16>>);

impl OwnedParams {
    fn from_vte(params: Params) -> Self {
        Self(params.iter().map(|group| group.to_vec()).collect())
    }

    fn as_groups(&self) -> &[Vec<u16>] {
        &self.0
    }

    fn iter(&self) -> impl Iterator<Item = &[u16]> {
        self.0.iter().map(Vec::as_slice)
    }

    fn get(
        &self,
        idx: usize,
    ) -> Option<&[u16]> {
        self.0.get(idx).map(Vec::as_slice)
    }
}

#[derive(Debug, Clone)]
pub(super) enum ParsedCsiAction {
    Unsupported,
    SetPrivateModes {
        enable: bool,
        modes: OwnedParams,
    },
    SavePrivateModes {
        modes: OwnedParams,
    },
    RestorePrivateModes {
        modes: OwnedParams,
    },
    SelectiveEraseDisplay {
        mode: u16,
    },
    SelectiveEraseLine {
        mode: u16,
    },
    KittyKeyboard {
        intermediate: u8,
        params: OwnedParams,
    },
    PrivateDeviceStatusReport {
        selector: u16,
    },
    QueryPrivateMode {
        mode: u16,
    },
    SelectActiveDisplay {
        mode: u16,
    },
    SetStatusDisplay {
        mode: u16,
    },
    ReportStatus {
        selector: u16,
    },
    QueryAnsiMode {
        mode: u16,
    },
    ResizeColumns {
        cols: u16,
    },
    EraseRect {
        params: OwnedParams,
    },
    SelectiveEraseRect {
        params: OwnedParams,
    },
    FillRect {
        params: OwnedParams,
    },
    CopyRect {
        params: OwnedParams,
    },
    ChangeRectAttrs {
        params: OwnedParams,
    },
    ReverseRectAttrs {
        params: OwnedParams,
    },
    SetScreenLines {
        lines: u16,
    },
    SetAttrChangeExtent {
        extent: grid::AttrChangeExtent,
    },
    SetCursorStyle {
        style: u16,
    },
    ScrollLeft {
        count: u16,
    },
    ScrollRight {
        count: u16,
    },
    SelectPage {
        page: u16,
    },
    NextPage {
        count: u16,
    },
    PrevPage {
        count: u16,
    },
    SetConformanceLevel {
        level: ConformanceLevel,
        c1_mode: C1Mode,
    },
    SetCharacterProtection {
        mode: u16,
    },
    InsertColumns {
        count: u16,
    },
    DeleteColumns {
        count: u16,
    },
    SoftReset,
    ReportUserPreferredSupplementalSet,
    ResetWithConfirmation {
        confirmation_param: Option<u16>,
    },
    ReportXtVersion,
    ReportSecondaryDeviceAttrs,
    ReportTertiaryDeviceAttrs,
    Main(MainCsiAction),
    StatusLine(StatusLineCsiAction),
}

#[derive(Debug, Clone)]
pub(super) enum MainCsiAction {
    SelfTest {
        requested_tests: Vec<u16>,
    },
    ReportPrimaryDeviceAttrs,
    DeviceStatusReport {
        selector: u16,
    },
    SetPageLines {
        lines: u16,
    },
    PushTitle,
    PopTitle,
    ReportPixelSize,
    ReportCellSize,
    ReportTextSize,
    RepeatLastChar {
        count: u16,
    },
    CursorUp {
        count: u16,
    },
    CursorDown {
        count: u16,
    },
    CursorForward {
        count: u16,
    },
    CursorBackward {
        count: u16,
    },
    CursorNextLine {
        count: u16,
    },
    CursorPreviousLine {
        count: u16,
    },
    CursorPosition {
        row: u16,
        col: u16,
    },
    EraseInDisplay {
        mode: u16,
    },
    EraseInLine {
        mode: u16,
    },
    SetGraphicsRendition {
        params: OwnedParams,
    },
    LinePositionAbsolute {
        row: u16,
    },
    CursorHorizontalAbsolute {
        col: u16,
    },
    CursorForwardRelative {
        count: u16,
    },
    CursorVerticalRelative {
        count: u16,
    },
    InsertLines {
        count: u16,
    },
    DeleteLines {
        count: u16,
    },
    DeleteChars {
        count: u16,
    },
    InsertChars {
        count: u16,
    },
    EraseChars {
        count: u16,
    },
    ScrollUp {
        count: u16,
    },
    ScrollDown {
        count: u16,
    },
    SetScrollRegion {
        top: u16,
        bottom: Option<u16>,
    },
    SetLeftRightMargins {
        left: u16,
        right: Option<u16>,
    },
    SaveCursor,
    RestoreCursor,
    NextPage {
        count: u16,
    },
    PrevPage {
        count: u16,
    },
    CursorForwardTabulation {
        count: u16,
    },
    CursorBackwardTabulation {
        count: u16,
    },
    TabClear {
        mode: u16,
    },
    SetAnsiModes {
        enable: bool,
        modes: Vec<mode::AnsiMode>,
    },
}

#[derive(Debug, Clone)]
pub(super) enum StatusLineCsiAction {
    SetGraphicsRendition { params: OwnedParams },
    InsertChars { count: u16 },
    HomeRow,
    CursorForward { count: u16 },
    CursorBackward { count: u16 },
    CursorHorizontalAbsolute { col: u16 },
    CursorPosition { col: u16 },
    EraseDisplay,
    EraseInLine { mode: u16 },
    DeleteChars { count: u16 },
    EraseChars { count: u16 },
    RepeatLastChar { count: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Vt52EscAction {
    CursorUp,
    CursorDown,
    CursorRight,
    CursorLeft,
    EnterDecSpecialGraphics,
    ExitDecSpecialGraphics,
    CursorHome,
    ReverseIndex,
    EraseToEndOfScreen,
    EraseToEndOfLine,
    DirectCursorAddressStart,
    Identify,
    ExitVt52Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ParsedEscAction {
    Unsupported,
    Vt52(Vt52EscAction),
    UseSevenBitC1Controls,
    UseEightBitC1Controls,
    UseEightBitText,
    UseUtf8Text,
    DesignateCharset {
        slot: GraphicSetSlot,
        charset: CharacterSet,
    },
    ScreenAlignmentTest,
    SetDoubleHeightTopLine,
    SetDoubleHeightBottomLine,
    SetSingleWidthLine,
    SetDoubleWidthLine,
    HardReset,
    SaveCursor,
    RestoreCursor,
    Index,
    NextLine,
    SetTabStop,
    ReverseIndex,
    EnableApplicationKeypad,
    DisableApplicationKeypad,
    SingleShiftG2,
    SingleShiftG3,
    LockingShiftG2ToGl,
    LockingShiftG3ToGl,
    LockingShiftG1ToGr,
    LockingShiftG2ToGr,
    LockingShiftG3ToGr,
    BackIndex,
    ForwardIndex,
}

// C0 control bytes (ECMA-48 / ASCII).
const NUL: u8 = 0x00;
const BEL: u8 = 0x07;
const BS: u8 = 0x08;
const VT: u8 = 0x0B;
const FF: u8 = 0x0C;
const SO: u8 = 0x0E;
const SI: u8 = 0x0F;

// DSR (Device Status Report) parameter values.
const DSR_OK: u16 = 5;
const DSR_CPR: u16 = 6;

// CSI Ps t — window manipulation parameter values.
const WINOP_TITLE_PUSH: u16 = 22;
const WINOP_TITLE_POP: u16 = 23;
const WINOP_REPORT_PIXELS: u16 = 14;
const WINOP_REPORT_CELL_SIZE: u16 = 16;
const WINOP_REPORT_TEXT_SIZE: u16 = 18;

// TBC (Tab Clear) parameter values.
const TBC_CURRENT: u16 = 0;
const TBC_ALL: u16 = 3;

const VALID_SCREEN_LINE_COUNTS: &[u16] = &[24, 25, 36, 48];
const VALID_PAGE_LINE_COUNTS: &[u16] = &[24, 25, 36, 48, 72, 144];

fn valid_screen_lines(ps: u16) -> Option<u32> {
    VALID_SCREEN_LINE_COUNTS.contains(&ps).then_some(ps as u32)
}

fn valid_page_lines(ps: u16) -> Option<u32> {
    VALID_PAGE_LINE_COUNTS.contains(&ps).then_some(ps as u32)
}

fn can_negotiate_c1(modes: &TerminalModes) -> bool {
    !modes.vt52_mode && modes.conformance_level.supports_c1_negotiation()
}

fn sync_screen_erase_defaults(
    screen: &mut Screen,
    dec_color: &DecColorState,
) {
    screen.grid.default_bg = erase_background_color(dec_color, screen.bg);
}

#[bon::builder]
fn apply_hard_reset_state(
    screen: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt_screen: &mut bool,
    modes: &mut TerminalModes,
    kitty_keyboard: &mut KittyKeyboardState,
    cursor_style: &mut CursorStyle,
    current_title: &mut Option<String>,
    title_stack: &mut Vec<Option<String>>,
    saved_modes: &mut std::collections::HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    palette: &mut color::ColorPalette,
    base_palette: &color::ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &StatusDisplayKind,
    macros: &mut MacroStore,
    drcs: &mut DrcsStore,
    conformance_level: ConformanceLevel,
    c1_mode: C1Mode,
) {
    *dec_color = dec_color_state_from_palette(base_palette);
    *palette = effective_palette(base_palette, dec_color);
    if *on_alt_screen {
        std::mem::swap(screen, stash);
        *on_alt_screen = false;
    }
    let total_rows = viewport.rows + screen::status_line_rows(screen);
    for s in [&mut *screen, &mut *stash] {
        s.grid.default_fg = palette.fg;
        s.grid.default_bg = palette.bg;
        s.cursor = grid::Cursor::default();
        s.fg = palette.fg;
        s.bg = palette.bg;
        s.attrs = CellAttrs::default();
        s.underline = UnderlineStyle::None;
        s.underline_color = None;
        s.scroll_top = 0;
        s.scroll_bottom = viewport.rows.saturating_sub(1);
        s.left_margin = 0;
        s.right_margin = viewport.cols.saturating_sub(1);
        s.offset = 0;
        s.saved_cursor = None;
        s.current_hyperlink = None;
        s.cursor_visible = true;
        s.last_char = None;
        s.tab_stops = screen::init_tab_stops(viewport.cols);
        s.origin_mode = false;
        s.nrc_mode = false;
        s.upss = charset::UserPreferredSupplementalSet::DecSupplemental;
        s.autowrap = true;
        s.app_cursor_keys = false;
        s.attr_change_extent = grid::AttrChangeExtent::Stream;
        s.app_keypad = false;
        s.charset = charset::CharsetState::new();
        s.active_display = ActiveDisplay::Main;
        s.status_display = StatusDisplayKind::None;
        s.status_line = None;
        crate::apply_status_display_mode(
            s,
            total_rows,
            viewport.cols,
            *default_status_display,
            palette,
        );
        sync_screen_erase_defaults(s, dec_color);
        screen::clear_visible(s, viewport);
    }
    viewport.rows = total_rows.saturating_sub(screen::status_line_rows(screen));
    *modes = TerminalModes::new();
    modes.conformance_level = conformance_level;
    modes.c1_mode = c1_mode;
    *kitty_keyboard = KittyKeyboardState::new();
    *cursor_style = CursorStyle::default();
    *current_title = None;
    title_stack.clear();
    saved_modes.clear();
    *current_prompt_row = None;
    *bell_pending = false;
    *vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
    macros.clear();
    drcs.clear();
}

/// Forward-scan tab stops from `start_col + 1`. Returns the column of the
/// next set tab stop, or `cols - 1` if none is found.
fn next_tab_stop(
    tab_stops: &[bool],
    start_col: u32,
    cols: u32,
) -> u32 {
    let start = start_col as usize + 1;
    let end = cols as usize;
    if let Some(offset) = tab_stops
        .get(start..end)
        .and_then(|s| s.iter().position(|&v| v))
    {
        (start + offset) as u32
    } else {
        cols - 1
    }
}

/// Backward-scan tab stops from `start_col - 1`. Returns the column of the
/// previous set tab stop, or 0 if none is found.
fn prev_tab_stop(
    tab_stops: &[bool],
    start_col: u32,
) -> u32 {
    if start_col == 0 {
        return 0;
    }
    for c in (0..start_col as usize).rev() {
        if tab_stops[c] {
            return c as u32;
        }
    }
    0
}

/// Fast path for a batched run of printable ASCII bytes (0x20..=0x7E).
///
/// Skips the grapheme/width machinery `put_char` needs — every byte is
/// width-1 and can't fold into a neighbour. Breaks wide-anchor invariants at
/// only the run's two edges (interior cells are entirely overwritten, so any
/// anchors they held are destroyed outright).
#[cfg(test)]
mod tests {
    use palette::Srgb;
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::cursor::CursorStyle;
    use crate::io::keyboard::KittyKeyboardState;
    use crate::screen::Screen;

    const TEST_COLS: u32 = 10;
    const TEST_ROWS: u32 = 4;

    fn setup() -> (Screen, Viewport) {
        let screen = Screen::new(
            TEST_COLS,
            TEST_ROWS,
            100,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let viewport = Viewport {
            rows: TEST_ROWS,
            cols: TEST_COLS,
            top: 0,
        };
        (screen, viewport)
    }

    fn parse_csi_action(input: &[u8]) -> ParsedCsiAction {
        let (screen, _) = setup();
        let modes = TerminalModes::new();
        parse_csi_action_with(input, &screen, &modes)
    }

    fn parse_csi_action_with(
        input: &[u8],
        screen: &Screen,
        modes: &TerminalModes,
    ) -> ParsedCsiAction {
        let mut parser = Parser::new();
        for action in parser.parse(input) {
            if let Action::CsiDispatch {
                params,
                intermediates,
                action,
            } = action
            {
                return csi_parse(screen, modes, params, intermediates.as_slice(), action);
            }
        }
        panic!("no CSI dispatch from input {input:?}");
    }

    fn parse_esc_action(
        input: &[u8],
        modes: &TerminalModes,
    ) -> ParsedEscAction {
        let drcs = DrcsStore::default();
        parse_esc_action_with(input, modes, &drcs)
    }

    fn parse_esc_action_with(
        input: &[u8],
        modes: &TerminalModes,
        drcs: &DrcsStore,
    ) -> ParsedEscAction {
        let mut parser = Parser::new();
        for action in parser.parse(input) {
            if let Action::EscDispatch {
                intermediates,
                byte,
            } = action
            {
                return esc_parse(modes, drcs, intermediates.as_slice(), byte);
            }
        }
        panic!("no ESC dispatch from input {input:?}");
    }

    /// Drive `input` through a VTE parser and dispatch each action through the
    /// parser module under test. This is the same pipeline the live terminal
    /// uses, so tests exercise the same paths callers actually take.
    fn feed(
        input: &[u8],
        screen: &mut Screen,
        viewport: &mut Viewport,
    ) {
        let base_pal = color::ColorPalette::default();
        let mut dec_color = dec_color_state_from_palette(&base_pal);
        let mut pal = effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for action in parser.parse(input) {
            // VT52 ESC Y cursor address state machine (mirrors Terminal::apply).
            if vt52_cursor_addr != crate::Vt52CursorAddr::Idle {
                let byte_opt: Option<u8> = match &action {
                    Action::PrintAscii(run) => run.first().copied(),
                    Action::Execute(b) => Some(*b),
                    _ => None,
                };
                match (vt52_cursor_addr, byte_opt) {
                    (crate::Vt52CursorAddr::AwaitingRow, Some(b)) => {
                        vt52_cursor_addr =
                            crate::Vt52CursorAddr::AwaitingCol(b.saturating_sub(0x20));
                        if let Action::PrintAscii(run) = &action
                            && run.len() >= 2
                        {
                            let row = b.saturating_sub(0x20) as u32;
                            let col = run[1].saturating_sub(0x20) as u32;
                            screen.cursor.row = row.min(viewport.rows.saturating_sub(1));
                            screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                            vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                            if run.len() > 2 {
                                let view = screen::screen_viewport(screen, viewport);
                                put_ascii_run(screen, &view, &run[2..], modes.insert_mode);
                            }
                            continue;
                        }
                        continue;
                    }
                    (crate::Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                        let col = b.saturating_sub(0x20) as u32;
                        screen.cursor.row = (row as u32).min(viewport.rows.saturating_sub(1));
                        screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                        if let Action::PrintAscii(run) = &action
                            && run.len() > 1
                        {
                            let view = screen::screen_viewport(screen, viewport);
                            put_ascii_run(screen, &view, &run[1..], modes.insert_mode);
                        }
                        continue;
                    }
                    _ => {
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                    }
                }
            }
            // In VT52 mode, CSI sequences are invalid and must be dropped.
            if modes.vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
                continue;
            }
            match action {
                Action::PrintAscii(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_ascii_run(screen, &view, run, modes.insert_mode)
                }
                Action::PrintText(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_text_run(screen, &view, run, modes.insert_mode)
                }
                Action::Print(s) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_printable(screen, &view, s, modes.insert_mode)
                }
                Action::Print8Bit(byte) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_8bit_byte(screen, &view, byte, modes.insert_mode)
                }
                Action::Execute(b) => {
                    let view = screen::screen_viewport(screen, viewport);
                    execute(screen, &view, b, &mut bell_pending, modes.newline_mode)
                }
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    csi_dispatch()
                        .screen(screen)
                        .stash(&mut stash)
                        .viewport(viewport)
                        .on_alt_screen(&mut on_alt_screen)
                        .modes(&mut modes)
                        .kitty_keyboard(&mut kitty_keyboard)
                        .cursor_style(&mut cursor_style)
                        .current_title(&mut current_title)
                        .title_stack(&mut title_stack)
                        .saved_modes(&mut saved_modes)
                        .current_prompt_row(&mut current_prompt_row)
                        .bell_pending(&mut bell_pending)
                        .palette(&mut pal)
                        .base_palette(&base_pal)
                        .dec_color(&mut dec_color)
                        .default_status_display(&mut default_status_display)
                        .pending_output(&mut pending_output)
                        .vt52_cursor_addr(&mut vt52_cursor_addr)
                        .macros(&mut macros)
                        .drcs(&mut drcs)
                        .params(&params)
                        .intermediates(intermediates.as_slice())
                        .action(action)
                        .pending_resize(&mut pending_resize)
                        .cell_width(8)
                        .cell_height(16)
                        .feature_permissions(&feature_permissions)
                        .call();
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    esc_dispatch()
                        .screen(screen)
                        .stash(&mut stash)
                        .viewport(viewport)
                        .on_alt_screen(&mut on_alt_screen)
                        .modes(&mut modes)
                        .kitty_keyboard(&mut kitty_keyboard)
                        .cursor_style(&mut cursor_style)
                        .current_title(&mut current_title)
                        .title_stack(&mut title_stack)
                        .saved_modes(&mut saved_modes)
                        .current_prompt_row(&mut current_prompt_row)
                        .bell_pending(&mut bell_pending)
                        .palette(&mut pal)
                        .base_palette(&base_pal)
                        .dec_color(&mut dec_color)
                        .default_status_display(&mut default_status_display)
                        .pending_output(&mut pending_output)
                        .vt52_cursor_addr(&mut vt52_cursor_addr)
                        .macros(&mut macros)
                        .drcs(&mut drcs)
                        .intermediates(intermediates.as_slice())
                        .byte(byte)
                        .call();
                }
                _ => {}
            }
        }
    }

    #[test]
    fn csi_parse_maps_private_mode_query_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[?7$p"),
            ParsedCsiAction::QueryPrivateMode { mode: 7 }
        ));
    }

    #[test]
    fn csi_parse_maps_ansi_mode_query_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[4$p"),
            ParsedCsiAction::QueryAnsiMode { mode: 4 }
        ));
    }

    #[test]
    fn csi_parse_maps_status_display_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[2$~"),
            ParsedCsiAction::SetStatusDisplay { mode: 2 }
        ));
    }

    #[test]
    fn csi_parse_maps_private_mode_set_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[?2004h"),
            ParsedCsiAction::SetPrivateModes { enable: true, .. }
        ));
    }

    #[test]
    fn csi_parse_maps_attr_change_extent_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[2*x"),
            ParsedCsiAction::SetAttrChangeExtent {
                extent: grid::AttrChangeExtent::Rectangle
            }
        ));
    }

    #[test]
    fn csi_parse_maps_cursor_style_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[5 q"),
            ParsedCsiAction::SetCursorStyle { style: 5 }
        ));
    }

    #[test]
    fn csi_parse_maps_soft_reset_semantically() {
        assert!(matches!(
            parse_csi_action(b"\x1b[!p"),
            ParsedCsiAction::SoftReset
        ));
    }

    #[test]
    fn csi_parse_uses_declrmm_to_disambiguate_csi_s() {
        let (screen, _) = setup();
        let mut modes = TerminalModes::new();
        modes.declrmm = true;
        assert!(matches!(
            parse_csi_action_with(b"\x1b[2;8s", &screen, &modes),
            ParsedCsiAction::Main(MainCsiAction::SetLeftRightMargins {
                left: 2,
                right: Some(8)
            })
        ));
    }

    #[test]
    fn csi_parse_uses_status_line_context_for_plain_actions() {
        let (mut screen, _) = setup();
        screen::set_status_display(
            &mut screen,
            TEST_COLS,
            StatusDisplayKind::HostWritable,
            color::default_fg(),
            color::default_bg(),
        );
        screen.active_display = ActiveDisplay::Status;
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_csi_action_with(b"\x1b[31m", &screen, &modes),
            ParsedCsiAction::StatusLine(StatusLineCsiAction::SetGraphicsRendition { .. })
        ));
    }

    #[test]
    fn esc_parse_maps_hard_reset_semantically() {
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_esc_action(b"\x1bc", &modes),
            ParsedEscAction::HardReset
        ));
    }

    #[test]
    fn esc_parse_maps_hash_intermediate_semantically() {
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_esc_action(b"\x1b#8", &modes),
            ParsedEscAction::ScreenAlignmentTest
        ));
    }

    #[test]
    fn esc_parse_maps_charset_designation_semantically() {
        let modes = TerminalModes::new();
        assert!(matches!(
            parse_esc_action(b"\x1b(0", &modes),
            ParsedEscAction::DesignateCharset {
                slot: GraphicSetSlot::G0,
                charset: CharacterSet::DecSpecialGraphics
            }
        ));
    }

    #[test]
    fn esc_parse_resolves_soft_charset_designations_semantically() {
        let modes = TerminalModes::new();
        let mut drcs = DrcsStore::default();
        drcs.define(&[0, 0, 0, 0, 0, 0, 0, 0], b"@?");
        assert!(matches!(
            parse_esc_action_with(b"\x1b(@", &modes, &drcs),
            ParsedEscAction::DesignateCharset {
                slot: GraphicSetSlot::G0,
                charset: CharacterSet::Drcs(0, crate::drcs::CharsetSize::Cs94)
            }
        ));
    }

    #[test]
    fn esc_parse_maps_vt52_sequences_using_mode_state() {
        let mut modes = TerminalModes::new();
        modes.vt52_mode = true;
        assert!(matches!(
            parse_esc_action(b"\x1bH", &modes),
            ParsedEscAction::Vt52(Vt52EscAction::CursorHome)
        ));
    }

    fn row_text(
        screen: &Screen,
        viewport: &Viewport,
        row: u32,
    ) -> String {
        let first_visible = screen.grid.rows.len() - viewport.rows as usize;
        let r = first_visible + row as usize;
        let mut s = String::new();
        for cell in &screen.grid.rows[r].cells {
            s.push_str(cell);
        }
        s
    }

    // -- put_char -----------------------------------------------------------

    #[test]
    fn put_char_writes_with_current_colors_and_advances() {
        let (mut screen, viewport) = setup();
        screen.fg = Srgb::new(1, 2, 3);
        screen.bg = Srgb::new(4, 5, 6);

        put_char(&mut screen, &viewport, SmolStr::new_inline("A"), false);

        assert_eq!(row_text(&screen, &viewport, 0).chars().next(), Some('A'));
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].fg[0], Srgb::new(1, 2, 3));
        assert_eq!(screen.grid.rows[r].bg[0], Srgb::new(4, 5, 6));
        assert_eq!(screen.cursor.col, 1);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn put_char_soft_wraps_at_right_edge() {
        let (mut screen, mut viewport) = setup();
        feed(b"abcdefghij", &mut screen, &mut viewport);

        // Cursor sits past the right edge; the next char should wrap.
        assert_eq!(screen.cursor.col, TEST_COLS);
        feed(b"k", &mut screen, &mut viewport);

        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
        assert!(
            screen.grid.rows[screen.grid.active_row_index(&screen.cursor, &viewport) - 1].wrapped
        );
        assert_eq!(&row_text(&screen, &viewport, 1)[..1], "k");
    }

    #[test]
    fn put_char_folds_combining_mark_into_previous_cell() {
        let (mut screen, mut viewport) = setup();
        // U+0301 COMBINING ACUTE ACCENT — feeding "e" then the combining mark
        // should store the full grapheme "é" in one cell without advancing.
        feed("e\u{0301}".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "e\u{0301}");
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_vs16_emoji_stays_in_single_cell() {
        let (mut screen, mut viewport) = setup();
        // `UnicodeWidthStr::width("❤\u{FE0F}") == 2`, but glibc `wcswidth`
        // reports 1 because it treats VS16 as a zero-width variation
        // selector without upgrading the base to emoji presentation. The
        // host shell tracks cursor position via wcswidth, so our grid must
        // agree — otherwise a single backspace from readline lands on the
        // continuation cell and the user can't delete the emoji. Keep the
        // cluster in one cell; the shaper still sees the full cluster
        // text and renders it scaled to that cell.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{2764}\u{FE0F}");
        assert_eq!(
            screen.grid.rows[r].cells[1].as_str(),
            " ",
            "VS16 must not widen the cell — cells[1] stays blank"
        );
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_write_after_vs16_emoji_preserves_the_emoji() {
        // Reproduces the reported "heart vanishes when you type anything
        // after it" bug. Before the fix, `is_wide_anchor` re-measured the
        // cell text with `UnicodeWidthStr` — which returns 2 for "❤\u{FE0F}"
        // — so `break_wide_glyphs_around_write` treated the single-cell
        // emoji as a misaligned wide anchor and blanked it. The grid-state
        // check in `is_wide_anchor_at` looks at the right neighbour
        // instead, matching the physical layout.
        let (mut screen, mut viewport) = setup();
        feed("\u{2764}\u{FE0F}X".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(
            screen.grid.rows[r].cells[0].as_str(),
            "\u{2764}\u{FE0F}",
            "heart must survive subsequent write"
        );
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "X");
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn backspace_over_vs16_emoji_moves_one_column() {
        // Readline sends a single BS to rub out `❤\u{FE0F}` because glibc
        // `wcswidth` reports its width as 1. The cursor must land on the
        // anchor column so subsequent rub-out bytes (typically `\b \b`)
        // clear the cell cleanly; widening the cell into 2 columns would
        // leave the cursor sitting on the continuation after one BS and
        // desync the shell's tracking.
        let (mut screen, mut viewport) = setup();
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 1);

        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 0);

        // A full rub-out of `\b \b` from bash lands us back at col 0 with
        // the cell erased.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &mut viewport);
        feed(b"\x08 \x08", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn backspace_guard_absorbs_zwj_width_overcount() {
        // Reproduces the bash/readline over-backspace pattern for 👩‍💻:
        // host codepoint widths sum to 4 (2 + 0 + 2), but terminal cell
        // width is 2. We should absorb the two extra BS bytes so the prompt
        // prefix is not overwritten.
        let (mut screen, mut viewport) = setup();
        feed("ab👩\u{200D}💻".as_bytes(), &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 4);

        feed(b"\x08\x08\x08\x08", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 2, "extra BS bytes are absorbed");

        feed(b"X", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "a");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "X");
    }

    #[test]
    fn put_char_regional_indicators_get_separate_cells() {
        let (mut screen, mut viewport) = setup();
        // `unicode-width` reports width 1 for each regional indicator, so
        // "🇺🇸" advances the cursor by 2 across two 1-col cells. We do not
        // collapse the flag pair into one cell — that would disagree with
        // the host's wcswidth and desync the cursor.
        feed("🇺🇸".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "🇺");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "🇸");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- wide (2-column) glyph handling ------------------------------------

    #[test]
    fn put_char_wide_glyph_occupies_two_cells_and_advances_cursor() {
        let (mut screen, mut viewport) = setup();
        feed("好".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), ""); // continuation
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn put_char_wide_glyph_soft_wraps_when_it_would_overhang() {
        let (mut screen, mut viewport) = setup();
        // Fill 9 of 10 columns with narrow chars so only 1 column is free.
        feed(b"abcdefghi", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 9);

        feed("好".as_bytes(), &mut screen, &mut viewport);

        // The wide glyph didn't fit at col 9, so we soft-wrap and place it
        // on the next row.
        assert_eq!(screen.cursor.row, 1);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.cursor.col, 2);
        assert!(screen.grid.rows[r - 1].wrapped);
    }

    #[test]
    fn put_char_narrow_overwriting_wide_anchor_blanks_continuation() {
        let (mut screen, mut viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &mut viewport);
        // Move cursor back to col 0 and stomp on the anchor with a narrow char.
        feed(b"\x1b[1;1H", &mut screen, &mut viewport);
        feed(b"x", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "x");
        // The continuation at col 1 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_narrow_overwriting_wide_continuation_blanks_anchor() {
        let (mut screen, mut viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &mut viewport);
        // Park cursor on the continuation (col 1) and write a narrow char.
        feed(b"\x1b[1;2H", &mut screen, &mut viewport);
        feed(b"x", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // The anchor at col 0 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "x");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_wide_overwriting_wide_blanks_both_neighbours() {
        let (mut screen, mut viewport) = setup();
        // [好, "", 世, "", a]
        feed("好世a".as_bytes(), &mut screen, &mut viewport);
        // Park on col 1 (好's continuation) and write a new wide glyph that
        // straddles the old layout.
        feed(b"\x1b[1;2H", &mut screen, &mut viewport);
        feed("界".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // 好's anchor (col 0) is orphaned — blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        // New wide glyph at cols 1-2.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "界");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "");
        // 世's orphaned continuation (at col 3) is blanked.
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[4].as_str(), "a");
    }

    #[test]
    fn put_char_zwj_emoji_merges_into_previous_wide_cell() {
        let (mut screen, mut viewport) = setup();
        // 👩‍💻 = 👩 ZWJ 💻. Once the ZWJ has folded into the previous cell,
        // the following emoji should also extend that same grapheme cluster
        // instead of starting a fresh wide glyph cell of its own.
        feed("👩\u{200D}💻".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👩\u{200D}💻");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), " ");
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn put_char_write_after_zwj_emoji_preserves_full_cluster() {
        let (mut screen, mut viewport) = setup();
        feed("👩\u{200D}💻X".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👩\u{200D}💻");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "X");
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn erase_from_zwj_continuation_clears_full_cluster_without_touching_prefix() {
        let (mut screen, mut viewport) = setup();
        feed("> 👩\u{200D}💻".as_bytes(), &mut screen, &mut viewport);

        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 3);

        feed(b"\x1b[K", &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), ">");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), " ");
        assert_eq!(screen.cursor.col, 3);
    }

    // -- execute ------------------------------------------------------------

    #[test]
    fn execute_lf_moves_cursor_down() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\n', &mut false, false);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn execute_lf_at_scroll_bottom_scrolls_up() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();

        execute(&mut screen, &viewport, b'\n', &mut false, false);

        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert_eq!(screen.grid.rows.len(), rows_before + 1);
    }

    #[test]
    fn execute_cr_resets_col_to_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 5;
        execute(&mut screen, &viewport, b'\r', &mut false, false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_bs_saturates_at_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 2;
        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 1);
        execute(&mut screen, &viewport, BS, &mut false, false);
        execute(&mut screen, &viewport, BS, &mut false, false);
        execute(&mut screen, &viewport, BS, &mut false, false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_tab_advances_to_next_tab_stop() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);

        screen.cursor.col = 3;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn execute_tab_clamps_at_rightmost_column() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = TEST_COLS - 1;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn execute_bel_sets_bell_pending() {
        let (mut screen, viewport) = setup();
        let mut bell = false;
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, BEL, &mut bell, false);
        assert!(bell);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn execute_nul_is_noop() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, NUL, &mut false, false);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    // -- csi_dispatch cursor movement --------------------------------------

    #[test]
    fn csi_a_moves_cursor_up_by_count() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 3;
        feed(b"\x1b[2A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_defaults_to_one() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_zero_parameter_treated_as_one() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[0A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_saturates_at_top() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        feed(b"\x1b[99A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn csi_b_moves_cursor_down_clamped() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[99B", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
    }

    #[test]
    fn csi_c_moves_cursor_right_clamped() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[99C", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_d_moves_cursor_left_saturating() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 2;
        feed(b"\x1b[5D", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- CNL / CPL -----------------------------------------------------------

    #[test]
    fn csi_e_moves_down_and_homes_column() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 0;
        screen.cursor.col = 5;
        feed(b"\x1b[2E", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_e_clamps_at_bottom() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 3;
        feed(b"\x1b[99E", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_f_moves_up_and_homes_column() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 3;
        screen.cursor.col = 7;
        feed(b"\x1b[2F", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_f_saturates_at_top() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        screen.cursor.col = 5;
        feed(b"\x1b[99F", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_positions_cursor_one_based() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[3;5H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_h_defaults_to_origin() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 5;
        feed(b"\x1b[H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_clamps_to_viewport() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[99;99H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_f_is_alias_of_h() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3f", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_s_saves_and_csi_u_restores_cursor() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s", &mut screen, &mut viewport);
        // Move elsewhere after saving.
        feed(b"\x1b[4;5H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 3);
        assert_eq!(screen.cursor.col, 4);
        feed(b"\x1b[u", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_u_without_prior_save_homes_cursor() {
        // Matches DECRC semantics: no saved slot → cursor homes to 0,0.
        // Live-updating scripts that call `CSI u` on the first paint
        // before any `CSI s` get predictable behaviour instead of a
        // surprise no-op that leaves the cursor mid-screen.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3H", &mut screen, &mut viewport);
        feed(b"\x1b[u", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_s_shares_slot_with_esc_7() {
        // SCOSC and DECSC write the same slot, so an `ESC 8` after a
        // `CSI s` restores the CSI-written position.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s", &mut screen, &mut viewport);
        feed(b"\x1b[4;5H", &mut screen, &mut viewport);
        feed(b"\x1b8", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_u_does_not_trip_kitty_keyboard_path() {
        // The kitty CSI-u path requires an intermediate (`>`, `<`, `=`,
        // `?`). A plain `CSI u` must fall through to SCORC — this test
        // guards against anyone re-ordering the kitty check in front of
        // the SCORC arm.
        let (mut screen, mut viewport) = setup();
        feed(
            b"\x1b[2;3H\x1b[s\x1b[4;5H\x1b[u",
            &mut screen,
            &mut viewport,
        );
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_g_sets_column_only() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[5G", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_d_lowercase_sets_row_only() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1b[3d", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 5);
    }

    // -- csi_dispatch erase / SGR / scroll region --------------------------

    #[test]
    fn csi_j_2_erases_entire_display() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello\nworld", &mut screen, &mut viewport);
        feed(b"\x1b[2J", &mut screen, &mut viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim(), "");
    }

    #[test]
    fn csi_k_erases_to_end_of_line() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello", &mut screen, &mut viewport);
        feed(b"\x1b[3G", &mut screen, &mut viewport); // col=2
        feed(b"\x1b[K", &mut screen, &mut viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim_end(), "he");
    }

    #[test]
    fn csi_m_applies_sgr_colors() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[31m", &mut screen, &mut viewport);
        // SGR 31 = ANSI red fg, which is (205, 0, 0) in the standard palette.
        assert_eq!(screen.fg, Srgb::new(205, 0, 0));
    }

    #[test]
    fn csi_r_sets_scroll_region_and_homes_cursor() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 3;
        screen.cursor.col = 5;
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_r_clamps_bounds_to_viewport() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;99r", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, TEST_ROWS - 1);
    }

    #[test]
    fn csi_with_intermediate_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // Intermediate ` ` before action `q` is a valid CSI shape but not one
        // we handle — we must leave state untouched.
        feed(b"\x1b[1 q", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn csi_unknown_action_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        screen.cursor.col = 1;
        // Use a genuinely unrecognized CSI action (not Z, which is now CBT).
        feed(b"\x1b[1~", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
    }

    // -- esc_dispatch ------------------------------------------------------

    #[test]
    fn esc_m_at_scroll_top_scrolls_down() {
        let (mut screen, mut viewport) = setup();
        feed(b"top\nmid\nbot", &mut screen, &mut viewport);
        // Cursor is at scroll_top (row 0) after moving back there.
        feed(b"\x1b[H", &mut screen, &mut viewport);
        feed(b"\x1bM", &mut screen, &mut viewport);
        // After scroll-down, the old top row shifts down one and row 0 blanks.
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim_end(), "top");
    }

    #[test]
    fn esc_m_above_scroll_top_moves_cursor_up() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1bM", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn esc_m_at_row_zero_outside_region_is_noop() {
        // scroll_top defaults to 0, so row 0 triggers scroll_down_in_region
        // above. Force a non-zero scroll_top to exercise the cursor.row > 0
        // branch at exactly row 0 of the viewport.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;4r", &mut screen, &mut viewport); // scroll_top = 1
        screen.cursor.row = 0;
        feed(b"\x1bM", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn esc_scs_designator_is_ignored() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // ESC ( B designates US-ASCII as G0. Parser should no-op without
        // dropping state or panicking on the `B` byte (which would otherwise
        // land in the unknown-byte arm).
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn esc_keypad_modes_set_app_keypad() {
        let (mut screen, mut viewport) = setup();
        assert!(!screen.app_keypad);
        feed(b"\x1b=", &mut screen, &mut viewport);
        assert!(screen.app_keypad);
        feed(b"\x1b>", &mut screen, &mut viewport);
        assert!(!screen.app_keypad);
        // Cursor must not be affected.
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- REP (CSI Ps b) ---------------------------------------------------

    #[test]
    fn rep_repeats_last_printed_char() {
        let (mut screen, mut viewport) = setup();
        // Print 'A' then repeat it 3 times.
        feed(b"A\x1b[3b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "A");
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn rep_without_prior_char_is_noop() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[3b", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn rep_defaults_to_one_repetition() {
        let (mut screen, mut viewport) = setup();
        feed(b"X\x1b[b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "X");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- DECSTR (CSI ! p) -------------------------------------------------

    #[test]
    fn decstr_resets_attrs_and_colors() {
        let (mut screen, mut viewport) = setup();
        // Set bold + reverse + custom colors.
        feed(b"\x1b[1;7;31;42m", &mut screen, &mut viewport);
        assert!(screen.attrs.contains(CellAttrs::BOLD));
        assert!(screen.attrs.contains(CellAttrs::REVERSE));
        assert_ne!(screen.fg, color::default_fg());
        // Soft reset.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(screen.attrs, CellAttrs::default());
        assert_eq!(screen.fg, color::default_fg());
        assert_eq!(screen.bg, color::default_bg());
    }

    #[test]
    fn decstr_resets_scroll_region() {
        let (mut screen, mut viewport) = setup();
        // Set a restrictive scroll region.
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        // Soft reset should restore full region.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, viewport.rows - 1);
    }

    #[test]
    fn decstr_preserves_screen_contents() {
        let (mut screen, mut viewport) = setup();
        feed(b"Hello", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        let before: Vec<_> = screen.grid.rows[r].cells[..5]
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        let after: Vec<_> = screen.grid.rows[r].cells[..5]
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        assert_eq!(before, after);
    }

    // -- DECSR / DECSRC (CSI Pr + p / CSI Pr * q) -------------------------

    #[test]
    fn decsr_resets_screen_and_reports_confirmation() {
        let (mut screen, mut viewport) = setup();
        feed(b"Hello\x1b[?7l", &mut screen, &mut viewport);
        assert!(!screen.autowrap);
        let out = feed_with_output(b"\x1b[123+p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[123*q");
        assert!(screen.autowrap);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert_eq!(row.cells[0].as_str(), " ");
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn decsr_without_parameter_does_not_report_confirmation() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[+p", &mut screen, &mut viewport);
        assert!(out.is_empty());
    }

    #[test]
    fn decsr_confirmation_uses_reset_c1_mode() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[64;2\"p\x1b[9+p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[9*q");
    }

    // -- DECRQM (CSI ? Ps $ p) -----------------------------------------------

    /// Like `feed` but returns the `pending_output` bytes written by query
    /// responses (DECRQM, DSR, etc.).
    fn feed_with_output(
        input: &[u8],
        screen: &mut Screen,
        viewport: &mut Viewport,
    ) -> Vec<u8> {
        let base_pal = color::ColorPalette::default();
        let mut dec_color = dec_color_state_from_palette(&base_pal);
        let mut pal = effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for action in parser.parse(input) {
            // VT52 ESC Y cursor address state machine (mirrors Terminal::apply).
            if vt52_cursor_addr != crate::Vt52CursorAddr::Idle {
                let byte_opt: Option<u8> = match &action {
                    Action::PrintAscii(run) => run.first().copied(),
                    Action::Execute(b) => Some(*b),
                    _ => None,
                };
                match (vt52_cursor_addr, byte_opt) {
                    (crate::Vt52CursorAddr::AwaitingRow, Some(b)) => {
                        vt52_cursor_addr =
                            crate::Vt52CursorAddr::AwaitingCol(b.saturating_sub(0x20));
                        if let Action::PrintAscii(run) = &action
                            && run.len() >= 2
                        {
                            let row = b.saturating_sub(0x20) as u32;
                            let col = run[1].saturating_sub(0x20) as u32;
                            screen.cursor.row = row.min(viewport.rows.saturating_sub(1));
                            screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                            vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                            if run.len() > 2 {
                                let view = screen::screen_viewport(screen, viewport);
                                put_ascii_run(screen, &view, &run[2..], modes.insert_mode);
                            }
                            continue;
                        }
                        continue;
                    }
                    (crate::Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                        let col = b.saturating_sub(0x20) as u32;
                        screen.cursor.row = (row as u32).min(viewport.rows.saturating_sub(1));
                        screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                        if let Action::PrintAscii(run) = &action
                            && run.len() > 1
                        {
                            let view = screen::screen_viewport(screen, viewport);
                            put_ascii_run(screen, &view, &run[1..], modes.insert_mode);
                        }
                        continue;
                    }
                    _ => {
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                    }
                }
            }
            // In VT52 mode, CSI sequences are invalid and must be dropped.
            if modes.vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
                continue;
            }
            match action {
                Action::PrintAscii(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_ascii_run(screen, &view, run, modes.insert_mode)
                }
                Action::PrintText(run) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_text_run(screen, &view, run, modes.insert_mode)
                }
                Action::Print(s) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_printable(screen, &view, s, modes.insert_mode)
                }
                Action::Print8Bit(byte) => {
                    let view = screen::screen_viewport(screen, viewport);
                    put_8bit_byte(screen, &view, byte, modes.insert_mode)
                }
                Action::Execute(b) => {
                    let view = screen::screen_viewport(screen, viewport);
                    execute(screen, &view, b, &mut bell_pending, modes.newline_mode)
                }
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    csi_dispatch()
                        .screen(screen)
                        .stash(&mut stash)
                        .viewport(viewport)
                        .on_alt_screen(&mut on_alt_screen)
                        .modes(&mut modes)
                        .kitty_keyboard(&mut kitty_keyboard)
                        .pending_output(&mut pending_output)
                        .pending_resize(&mut pending_resize)
                        .cursor_style(&mut cursor_style)
                        .cell_width(8)
                        .cell_height(16)
                        .palette(&mut pal)
                        .base_palette(&base_pal)
                        .dec_color(&mut dec_color)
                        .default_status_display(&mut default_status_display)
                        .title_stack(&mut title_stack)
                        .current_title(&mut current_title)
                        .saved_modes(&mut saved_modes)
                        .current_prompt_row(&mut current_prompt_row)
                        .bell_pending(&mut bell_pending)
                        .vt52_cursor_addr(&mut vt52_cursor_addr)
                        .macros(&mut macros)
                        .drcs(&mut drcs)
                        .params(&params)
                        .intermediates(intermediates.as_slice())
                        .action(action)
                        .feature_permissions(&feature_permissions)
                        .call();
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    esc_dispatch()
                        .screen(screen)
                        .stash(&mut stash)
                        .viewport(viewport)
                        .on_alt_screen(&mut on_alt_screen)
                        .modes(&mut modes)
                        .kitty_keyboard(&mut kitty_keyboard)
                        .cursor_style(&mut cursor_style)
                        .current_title(&mut current_title)
                        .title_stack(&mut title_stack)
                        .saved_modes(&mut saved_modes)
                        .current_prompt_row(&mut current_prompt_row)
                        .bell_pending(&mut bell_pending)
                        .palette(&mut pal)
                        .base_palette(&base_pal)
                        .dec_color(&mut dec_color)
                        .default_status_display(&mut default_status_display)
                        .pending_output(&mut pending_output)
                        .vt52_cursor_addr(&mut vt52_cursor_addr)
                        .macros(&mut macros)
                        .drcs(&mut drcs)
                        .intermediates(intermediates.as_slice())
                        .byte(byte)
                        .call();
                }
                _ => {}
            }
        }
        pending_output
    }

    #[test]
    fn decrqm_reports_cursor_visible_set() {
        let (mut screen, mut viewport) = setup();
        // Cursor is visible by default.
        let out = feed_with_output(b"\x1b[?25$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?25;1$y");
    }

    #[test]
    fn decrqm_reports_cursor_visible_reset() {
        let (mut screen, mut viewport) = setup();
        screen.cursor_visible = false;
        let out = feed_with_output(b"\x1b[?25$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?25;2$y");
    }

    #[test]
    fn decsnls_resizes_visible_rows_and_activates_page_memory() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[36*|", &mut screen, &mut viewport);
        assert_eq!(viewport.rows, 36);
        assert_eq!(
            screen.page_memory.as_ref().map(|page| page.lines_per_page),
            Some(36)
        );
    }

    #[test]
    fn decslpp_extends_page_length_without_resizing_screen() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[72t", &mut screen, &mut viewport);
        assert_eq!(viewport.rows, TEST_ROWS);
        assert_eq!(
            screen.page_memory.as_ref().map(|page| page.lines_per_page),
            Some(72)
        );
    }

    #[test]
    fn decscpp_resizes_columns() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[132$|", &mut screen, &mut viewport);
        assert_eq!(viewport.cols, 132);
        assert_eq!(screen.right_margin, 131);
    }

    #[test]
    fn decrqpsr_reports_tab_stops() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 3;
        feed(b"\x1bH", &mut screen, &mut viewport);
        let out = feed_with_output(b"\x1b[2$w", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1bP2$u4;9\x1b\\");
    }

    #[test]
    fn np_switches_page_and_homes_cursor() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 5;
        screen.cursor.col = 7;
        feed(b"\x1b[2U", &mut screen, &mut viewport);
        let page = screen.page_memory.as_ref().unwrap();
        assert_eq!(page.active_page, 2);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn deccra_copies_between_pages() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1U\x1b[1;1H", &mut screen, &mut viewport);
        feed(b"Z", &mut screen, &mut viewport);
        feed(b"\x1b[1V", &mut screen, &mut viewport);
        let page1 = screen::page_viewport(&screen, &viewport, 1).unwrap();
        let page2 = screen::page_viewport(&screen, &viewport, 2).unwrap();
        assert_eq!(
            screen.grid.rows[page2.top].cells[0].as_str(),
            "Z",
            "page 2 should receive direct printable writes"
        );
        feed(b"\x1b[1;1;1;1;2;1;1;1$v", &mut screen, &mut viewport);
        assert_eq!(
            screen.grid.rows[page1.top].cells[0].as_str(),
            "Z",
            "page 1 should receive copied cell from page 2"
        );
        assert_eq!(
            screen.grid.rows[page2.top].cells[0].as_str(),
            "Z",
            "source page should remain unchanged"
        );
    }

    #[test]
    fn decsera_skips_protected_cells() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1\"qA\x1b[0\"qB", &mut screen, &mut viewport);
        feed(b"\x1b[1;1;1;2${", &mut screen, &mut viewport);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert_eq!(row.cells[0].as_str(), "A");
        assert_eq!(row.cells[1].as_str(), " ");
    }

    #[test]
    fn deccara_and_decrara_use_vt420_opcodes() {
        let (mut screen, mut viewport) = setup();
        feed(b"X", &mut screen, &mut viewport);
        feed(b"\x1b[1;1;1;1;1$r", &mut screen, &mut viewport);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert!(row.attrs[0].contains(CellAttrs::BOLD));

        feed(b"\x1b[1;1;1;1;1$t", &mut screen, &mut viewport);
        let row = &screen.grid.rows[screen::active_row_index(&screen, &viewport)];
        assert!(!row.attrs[0].contains(CellAttrs::BOLD));
    }

    #[test]
    fn decsace_switches_between_stream_and_rectangle_extent() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;2HA\x1b[3;2HB", &mut screen, &mut viewport);

        feed(b"\x1b[1;2;3;2;1$r", &mut screen, &mut viewport);
        assert!(screen.grid.rows[0].attrs[1].contains(CellAttrs::BOLD));
        assert!(!screen.grid.rows[1].attrs[1].contains(CellAttrs::BOLD));
        assert!(screen.grid.rows[2].attrs[1].contains(CellAttrs::BOLD));

        feed(b"\x1b[2*x\x1b[1;2;3;2;1$r", &mut screen, &mut viewport);
        assert!(screen.grid.rows[1].attrs[1].contains(CellAttrs::BOLD));
    }

    #[test]
    fn decrqm_reports_bracketed_paste() {
        let (mut screen, mut viewport) = setup();
        // Enable bracketed paste first, then query.
        let out = feed_with_output(b"\x1b[?2004h\x1b[?2004$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2004;1$y");
    }

    #[test]
    fn decrqm_unknown_mode_reports_zero() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?9999$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?9999;0$y");
    }

    #[test]
    fn decrqm_ansi_mode_reports_zero_for_unknown() {
        let (mut screen, mut viewport) = setup();
        // Query an unknown ANSI (non-private) mode.
        let out = feed_with_output(b"\x1b[99$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[99;0$y");
    }

    // -- Tab stops -----------------------------------------------------------

    #[test]
    fn default_tab_stops_every_8_columns() {
        // 10-col screen: only column 8 is a stop.
        let (mut screen, viewport) = setup();
        assert_eq!(screen.cursor.col, 0);
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn tab_from_mid_column_goes_to_next_stop() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn tab_at_last_column_stays() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = TEST_COLS - 1;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn hts_sets_custom_tab_stop() {
        let (mut screen, mut viewport) = setup();
        // Move to col 3, set a tab stop with ESC H, then tab from col 0.
        feed(b"\x1b[1;4H\x1bH", &mut screen, &mut viewport);
        assert!(screen.tab_stops[3]);
        screen.cursor.col = 0;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn cht_moves_forward_n_tab_stops() {
        // Use a wider screen so we have at least two default stops.
        let screen_cols = 24;
        let mut screen = Screen::new(
            screen_cols,
            TEST_ROWS,
            100,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut viewport = Viewport {
            rows: TEST_ROWS,
            cols: screen_cols,
            top: 0,
        };
        // Default stops at 8, 16. CSI 2 I from col 0 should jump to 16.
        feed(b"\x1b[2I", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 16);
    }

    #[test]
    fn cbt_moves_backward_n_tab_stops() {
        let screen_cols = 24;
        let mut screen = Screen::new(
            screen_cols,
            TEST_ROWS,
            100,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut viewport = Viewport {
            rows: TEST_ROWS,
            cols: screen_cols,
            top: 0,
        };
        // Park at col 20, then CSI 2 Z (back 2 stops) should land at 8.
        screen.cursor.col = 20;
        feed(b"\x1b[2Z", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, 8);
    }

    #[test]
    fn tbc_0_clears_at_cursor() {
        let (mut screen, mut viewport) = setup();
        // Default stop at col 8. Move there and clear it.
        screen.cursor.col = 8;
        feed(b"\x1b[0g", &mut screen, &mut viewport);
        assert!(!screen.tab_stops[8]);
        // Tab from col 0 should now go to the last column.
        screen.cursor.col = 0;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn tbc_3_clears_all_tab_stops() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[3g", &mut screen, &mut viewport);
        assert!(screen.tab_stops.iter().all(|&s| !s));
        // Tab from col 0 should go to last column.
        screen.cursor.col = 0;
        execute(&mut screen, &viewport, b'\t', &mut false, false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    // -- Insert Mode (IRM) ---------------------------------------------------

    #[test]
    fn default_mode_is_replace() {
        let (mut screen, mut viewport) = setup();
        feed(b"abc", &mut screen, &mut viewport);
        // Overwrite at col 0.
        feed(b"\x1b[1;1H", &mut screen, &mut viewport);
        feed(b"X", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "c");
    }

    #[test]
    fn insert_mode_shifts_text_right() {
        let (mut screen, mut viewport) = setup();
        feed(b"abc", &mut screen, &mut viewport);
        // Enable insert mode (CSI 4 h), move to col 0, type 'X'.
        feed(b"\x1b[4h\x1b[1;1HX", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "a");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "c");
    }

    #[test]
    fn insert_mode_disable_returns_to_replace() {
        let (mut screen, mut viewport) = setup();
        feed(b"abc", &mut screen, &mut viewport);
        // Enable insert, then disable it.
        feed(b"\x1b[4h\x1b[4l\x1b[1;1HX", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // Replace mode: 'X' overwrites 'a'.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "b");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "c");
    }

    // -- Origin Mode (DECOM) -------------------------------------------------

    #[test]
    fn origin_mode_cup_relative_to_scroll_region() {
        let (mut screen, mut viewport) = setup();
        // Set scroll region to rows 2..3 (1-based).
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        // Enable origin mode.
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // CUP(1,1) should land at top of scroll region (row 1 in 0-based).
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
        // CUP(2,1) should land at row 2 (scroll_bottom).
        feed(b"\x1b[2;1H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn origin_mode_cup_clamps_to_scroll_region() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // CUP(99,1) should clamp to scroll_bottom.
        feed(b"\x1b[99;1H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn origin_mode_disable_returns_to_absolute() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // Disable origin mode — cursor homes to absolute (0,0).
        feed(b"\x1b[?6l", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
        // CUP(1,1) is now absolute row 0.
        feed(b"\x1b[1;1H", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn origin_mode_vpa_relative_to_scroll_region() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[2;3r", &mut screen, &mut viewport);
        feed(b"\x1b[?6h", &mut screen, &mut viewport);
        // VPA(2) should land at scroll_top + 1 = row 2.
        feed(b"\x1b[2d", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn decrqm_reports_origin_mode() {
        let (mut screen, mut viewport) = setup();
        // Default is off.
        let out = feed_with_output(b"\x1b[?6$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?6;2$y");
        // Enable and re-query.
        let out = feed_with_output(b"\x1b[?6h\x1b[?6$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?6;1$y");
    }

    #[test]
    fn decrqm_irm_reports_insert_mode() {
        let (mut screen, mut viewport) = setup();
        // Default is replace (off) → Pm=2.
        let out = feed_with_output(b"\x1b[4$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[4;2$y");
        // Enable and re-query → Pm=1.
        let out = feed_with_output(b"\x1b[4h\x1b[4$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[4;1$y");
    }

    // -- DEC Special Graphics (SCS) ------------------------------------------

    #[test]
    fn scs_g0_drawing_translates_box_chars() {
        let (mut screen, mut viewport) = setup();
        // ESC ( 0 designates DEC drawing into G0, then print box-drawing bytes.
        // 0x6C = ┌, 0x71 = ─, 0x6B = ┐
        feed(b"\x1b(0\x6c\x71\x6b", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{2500}"); // ─
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "\u{2510}"); // ┐
    }

    #[test]
    fn scs_g0_ascii_restores_normal() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing, write a box char, then switch back to ASCII.
        feed(b"\x1b(0\x6c\x1b(B\x6c", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "l"); // plain ASCII
    }

    #[test]
    fn scs_drawing_does_not_translate_below_0x60() {
        let (mut screen, mut viewport) = setup();
        // In drawing mode, bytes below 0x60 should pass through as ASCII.
        feed(b"\x1b(0ABC", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "B");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "C");
    }

    #[test]
    fn scs_so_si_switch_between_g0_g1() {
        let (mut screen, mut viewport) = setup();
        // G0 = ASCII (default), G1 = drawing.
        // SO (0x0E) invokes G1, SI (0x0F) invokes G0.
        feed(b"\x1b)0", &mut screen, &mut viewport); // G1 = drawing
        feed(b"\x0E", &mut screen, &mut viewport); // SO → GL = G1
        feed(b"\x6c", &mut screen, &mut viewport); // should translate
        feed(b"\x0F", &mut screen, &mut viewport); // SI → GL = G0
        feed(b"\x6c", &mut screen, &mut viewport); // should be plain ASCII
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{250C}"); // ┌
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "l"); // plain
    }

    #[test]
    fn scs_decstr_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
        // DECSTR should reset charset state.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::Ascii
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G0);
    }

    #[test]
    fn scs_ris_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0\x1b)0\x0E", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::DecSpecialGraphics
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G1);
        // RIS should reset everything.
        feed(b"\x1bc", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G1),
            CharacterSet::Ascii
        );
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G0);
    }

    #[test]
    fn scs_save_restore_cursor_preserves_charset() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing in G0, save cursor.
        feed(b"\x1b(0\x1b7", &mut screen, &mut viewport);
        // Switch back to ASCII.
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
        // Restore cursor — should bring back DEC drawing.
        feed(b"\x1b8", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
    }

    #[test]
    fn scs_technical_charset_translates_math_symbols() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b)>", &mut screen, &mut viewport); // G1 = DEC Technical
        feed(b"\x0Eabc", &mut screen, &mut viewport); // SO -> GL = G1
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{03B2}");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "\u{03C7}");
    }

    #[test]
    fn scs_ls2_maps_g2_into_gl() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.A", &mut screen, &mut viewport); // G2 = ISO Latin-1 supplemental
        feed(b"\x1bn!!", &mut screen, &mut viewport); // LS2 -> GL = G2
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "\u{00A1}");
        assert_eq!(screen.charset.gl_slot(), GraphicSetSlot::G2);
    }

    #[test]
    fn scs_single_shift_uses_g2_for_one_character() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.A\x1bN!!", &mut screen, &mut viewport); // G2 = ISO Latin-1 supplemental
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "!");
    }

    #[test]
    fn scs_ls1r_maps_g1_into_gr_for_utf8_text() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b)>\x1b~", &mut screen, &mut viewport); // G1 = DEC Technical, GR = G1
        feed("á".as_bytes(), &mut screen, &mut viewport); // U+00E1 -> 0x61 in GR
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.charset.gr_slot(), GraphicSetSlot::G1);
    }

    #[test]
    fn scs_ls2r_maps_g2_into_gr_for_utf8_text() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b.%5\x1b}", &mut screen, &mut viewport); // G2 = DEC Supplemental, GR = G2
        feed("¨".as_bytes(), &mut screen, &mut viewport); // U+00A8 -> DEC MCS currency sign
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A4}");
        assert_eq!(screen.charset.gr_slot(), GraphicSetSlot::G2);
    }

    #[test]
    fn docs_8bit_mode_routes_raw_high_bytes_through_gr() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b%@\x1b)>\x1b~\xe1A", &mut screen, &mut viewport); // raw 0xE1 -> 0x61 in GR
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "A");
    }

    #[test]
    fn scs_gr_translation_applies_to_split_utf8_codepoint() {
        let (mut screen, mut viewport) = setup();
        let base_pal = color::ColorPalette::default();
        let mut dec_color = dec_color_state_from_palette(&base_pal);
        let mut pal = effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();

        for chunk in [b"\x1b)>\x1b~\xc3".as_slice(), b"\xa1".as_slice()] {
            for action in parser.parse(chunk) {
                match action {
                    Action::PrintAscii(run) => {
                        put_ascii_run(&mut screen, &viewport, run, modes.insert_mode)
                    }
                    Action::PrintText(run) => {
                        put_text_run(&mut screen, &viewport, run, modes.insert_mode)
                    }
                    Action::Print(s) => put_printable(&mut screen, &viewport, s, modes.insert_mode),
                    Action::Print8Bit(byte) => {
                        put_8bit_byte(&mut screen, &viewport, byte, modes.insert_mode)
                    }
                    Action::Execute(b) => execute(
                        &mut screen,
                        &viewport,
                        b,
                        &mut bell_pending,
                        modes.newline_mode,
                    ),
                    Action::CsiDispatch {
                        params,
                        intermediates,
                        action,
                    } => {
                        csi_dispatch()
                            .screen(&mut screen)
                            .stash(&mut stash)
                            .viewport(&mut viewport)
                            .on_alt_screen(&mut on_alt_screen)
                            .modes(&mut modes)
                            .kitty_keyboard(&mut kitty_keyboard)
                            .pending_output(&mut pending_output)
                            .pending_resize(&mut pending_resize)
                            .cursor_style(&mut cursor_style)
                            .cell_width(8)
                            .cell_height(16)
                            .palette(&mut pal)
                            .base_palette(&base_pal)
                            .dec_color(&mut dec_color)
                            .default_status_display(&mut default_status_display)
                            .title_stack(&mut title_stack)
                            .current_title(&mut current_title)
                            .saved_modes(&mut saved_modes)
                            .current_prompt_row(&mut current_prompt_row)
                            .bell_pending(&mut bell_pending)
                            .vt52_cursor_addr(&mut vt52_cursor_addr)
                            .macros(&mut macros)
                            .drcs(&mut drcs)
                            .params(&params)
                            .intermediates(intermediates.as_slice())
                            .action(action)
                            .feature_permissions(&feature_permissions)
                            .call();
                    }
                    Action::EscDispatch {
                        intermediates,
                        byte,
                    } => {
                        esc_dispatch()
                            .screen(&mut screen)
                            .stash(&mut stash)
                            .viewport(&mut viewport)
                            .on_alt_screen(&mut on_alt_screen)
                            .modes(&mut modes)
                            .kitty_keyboard(&mut kitty_keyboard)
                            .cursor_style(&mut cursor_style)
                            .current_title(&mut current_title)
                            .title_stack(&mut title_stack)
                            .saved_modes(&mut saved_modes)
                            .current_prompt_row(&mut current_prompt_row)
                            .bell_pending(&mut bell_pending)
                            .palette(&mut pal)
                            .base_palette(&base_pal)
                            .dec_color(&mut dec_color)
                            .default_status_display(&mut default_status_display)
                            .pending_output(&mut pending_output)
                            .vt52_cursor_addr(&mut vt52_cursor_addr)
                            .macros(&mut macros)
                            .drcs(&mut drcs)
                            .intermediates(intermediates.as_slice())
                            .byte(byte)
                            .call();
                    }
                    _ => {}
                }
            }
        }

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{03B1}");
    }

    #[test]
    fn scs_decnrcm_gates_nrc_translation() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(A#", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "#");

        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?42h\x1b(A#", &mut screen, &mut viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "\u{00A3}");
    }

    #[test]
    fn decrqupss_reports_default_upss() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[&u", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1bP0!u%5\x1b\\");
    }

    #[test]
    fn scs_full_box_top_bottom() {
        // Simulate a typical box-drawing sequence: ┌──┐ on top, └──┘ on bottom.
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0", &mut screen, &mut viewport);
        feed(b"\x6c\x71\x71\x6b", &mut screen, &mut viewport); // ┌──┐
        feed(b"\r\n", &mut screen, &mut viewport);
        feed(b"\x6d\x71\x71\x6a", &mut screen, &mut viewport); // └──┘
        let top = row_text(&screen, &viewport, 0);
        assert!(top.starts_with("\u{250C}\u{2500}\u{2500}\u{2510}"));
        let bot = row_text(&screen, &viewport, 1);
        assert!(bot.starts_with("\u{2514}\u{2500}\u{2500}\u{2518}"));
    }

    // -- DECALN (ESC # 8) ---------------------------------------------------

    #[test]
    fn decaln_fills_screen_with_e() {
        let (mut screen, mut viewport) = setup();
        feed(b"hello", &mut screen, &mut viewport);
        feed(b"\x1b#8", &mut screen, &mut viewport);
        let text = row_text(&screen, &viewport, 0);
        assert!(text.chars().all(|c| c == 'E'));
        let text2 = row_text(&screen, &viewport, TEST_ROWS - 1);
        assert!(text2.chars().all(|c| c == 'E'));
    }

    // -- IND (ESC D) and NEL (ESC E) ----------------------------------------

    #[test]
    fn ind_moves_cursor_down() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1bD", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 5); // col preserved
    }

    #[test]
    fn ind_at_scroll_bottom_scrolls_up() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();
        feed(b"\x1bD", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert!(screen.grid.rows.len() > rows_before);
    }

    #[test]
    fn nel_moves_to_col_0_of_next_line() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1bE", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0);
    }

    // -- DECAWM (mode ?7) ---------------------------------------------------

    #[test]
    fn decawm_off_prevents_wrap() {
        let (mut screen, mut viewport) = setup();
        // Disable auto-wrap.
        feed(b"\x1b[?7l", &mut screen, &mut viewport);
        // Write more chars than columns — should stay on last column.
        feed(b"abcdefghijXX", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        // Last column should have the last char written.
        let text = row_text(&screen, &viewport, 0);
        assert_eq!(&text[..TEST_COLS as usize], "abcdefghiX");
    }

    #[test]
    fn decawm_on_wraps_normally() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?7l", &mut screen, &mut viewport);
        feed(b"\x1b[?7h", &mut screen, &mut viewport);
        feed(b"abcdefghijkl", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    // -- LNM (mode 20) ------------------------------------------------------

    #[test]
    fn lnm_enabled_lf_implies_cr() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.col = 5;
        // Enable LNM and issue LF in one feed call so the modes object
        // persists across both sequences.
        feed(b"\x1b[20h\n", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 0); // CR implied
    }

    // -- pending wrap cancellation -------------------------------------------

    #[test]
    fn cub_from_pending_wrap_lands_on_second_to_last() {
        let (mut screen, mut viewport) = setup();
        // Fill the row to put cursor into pending wrap (col == viewport.cols).
        feed(b"abcdefghij", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS);
        // CUB 1 should cancel pending wrap (→ last col) then move back 1.
        feed(b"\x1b[D", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS - 2);
    }

    #[test]
    fn cuu_from_pending_wrap_cancels_wrap() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 1;
        feed(b"abcdefghij", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS);
        // CUU 1 should move up without wrapping and cancel the pending
        // wrap column to the last column.
        feed(b"\x1b[A", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn ed_from_pending_wrap_erases_last_column() {
        let (mut screen, mut viewport) = setup();
        feed(b"abcdefghij", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.col, TEST_COLS);
        // ED 0 (erase to end) should erase the last column, not be a no-op.
        feed(b"\x1b[J", &mut screen, &mut viewport);
        let text = row_text(&screen, &viewport, 0);
        assert_eq!(&text[..TEST_COLS as usize], "abcdefghi ");
    }

    // -- VT52 mode -----------------------------------------------------------
    //
    // Each test uses a single feed() / feed_with_output() call so that mode
    // changes set by `CSI ? 2 l` remain active for the sequences that follow.
    // Separate calls create fresh TerminalModes, so VT52 state would not
    // persist across call boundaries.

    /// DECRQM reports DECANM as set (ANSI) by default.
    #[test]
    fn decrqm_reports_decanm_set_in_ansi_mode() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// DECRQM after entering and immediately exiting VT52 mode (via ESC <)
    /// reports DECANM as set again.
    #[test]
    fn decrqm_reports_decanm_restored_after_exit() {
        let (mut screen, mut viewport) = setup();
        // Enter VT52 then exit with ESC < — DECRQM should see ANSI mode.
        let out = feed_with_output(b"\x1b[?2l\x1b<\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// Enter VT52 then exit via `ESC <`; DECRQM should see ANSI mode restored.
    #[test]
    fn vt52_enter_and_exit_via_esc_lt() {
        let (mut screen, mut viewport) = setup();
        // `CSI ? 2 l` → VT52; `ESC <` → back to ANSI; DECRQM → set.
        let out = feed_with_output(b"\x1b[?2l\x1b<\x1b[?2$p", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b[?2;1$y");
    }

    /// VT52 ESC A/B/C/D cursor movement.
    #[test]
    fn vt52_cursor_up() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 2, col 3 (1-based: 3;4), then VT52 ESC A.
        feed(b"\x1b[3;4H\x1b[?2l\x1bA", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (1, 3));
    }

    #[test]
    fn vt52_cursor_down() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 1, col 0, then VT52 ESC B.
        feed(b"\x1b[2;1H\x1b[?2l\x1bB", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 0));
    }

    #[test]
    fn vt52_cursor_right() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;3H\x1b[?2l\x1bC", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    }

    #[test]
    fn vt52_cursor_left() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[1;5H\x1b[?2l\x1bD", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 3));
    }

    /// VT52 cursor up at row 0 does not underflow.
    #[test]
    fn vt52_cursor_up_clamps_at_top() {
        let (mut screen, mut viewport) = setup();
        // Already at row 0 (home). VT52 mode, ESC A.
        feed(b"\x1b[?2l\x1bA", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    /// VT52 ESC H homes the cursor.
    #[test]
    fn vt52_cursor_home() {
        let (mut screen, mut viewport) = setup();
        // CUP to row 3, col 6 (1-based), then VT52 ESC H.
        feed(b"\x1b[3;6H\x1b[?2l\x1bH", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 0));
    }

    /// VT52 ESC Y <row+0x20> <col+0x20> direct cursor address — bytes split.
    #[test]
    fn vt52_direct_cursor_address() {
        let (mut screen, mut viewport) = setup();
        // Enter VT52 then ESC Y: row 2 ('"'=0x22), col 4 ('$'=0x24).
        feed(b"\x1b[?2l\x1bY\"$", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 4));
    }

    /// VT52 ESC Y where both position bytes arrive in the same PrintAscii run.
    #[test]
    fn vt52_direct_cursor_address_batched() {
        let (mut screen, mut viewport) = setup();
        // Row 1 ('!'=0x21), col 3 ('#'=0x23).
        feed(b"\x1b[?2l\x1bY!#", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (1, 3));
    }

    /// Text after ESC Y position bytes is printed normally.
    #[test]
    fn vt52_direct_cursor_address_then_text() {
        let (mut screen, mut viewport) = setup();
        // Row 0, col 0 (both 0x20 = space), then 'A'.
        feed(b"\x1b[?2l\x1bY  A", &mut screen, &mut viewport);
        assert_eq!((screen.cursor.row, screen.cursor.col), (0, 1));
        assert_eq!(&row_text(&screen, &viewport, 0)[..1], "A");
    }

    /// VT52 ESC J erases from cursor to end of screen (same as ED 0).
    #[test]
    fn vt52_erase_to_end_of_screen() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0 with 'a', row 1 with 'b', then enter VT52 at row 0
        // col 5 (via CUP before VT52 entry) and erase.
        feed(
            b"aaaaaaaaaa\r\nbbbbbbbbbb\x1b[1;6H\x1b[?2l\x1bJ",
            &mut screen,
            &mut viewport,
        );
        let r0 = row_text(&screen, &viewport, 0);
        let r1 = row_text(&screen, &viewport, 1);
        assert_eq!(&r0[..5], "aaaaa", "text before cursor preserved");
        assert_eq!(r0[5..].trim(), "", "text from cursor erased");
        assert_eq!(r1.trim(), "", "row 1 cleared");
    }

    /// VT52 ESC K erases from cursor to end of line (same as EL 0).
    #[test]
    fn vt52_erase_to_end_of_line() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0, position at col 3, enter VT52, erase to EOL.
        feed(
            b"aaaaaaaaaa\x1b[1;4H\x1b[?2l\x1bK",
            &mut screen,
            &mut viewport,
        );
        let r0 = row_text(&screen, &viewport, 0);
        assert_eq!(&r0[..3], "aaa");
        assert_eq!(r0[3..].trim(), "");
    }

    /// VT52 ESC F/G toggle DEC Special Graphics on G0 within one parse pass.
    #[test]
    fn vt52_graphics_mode_on() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b[?2l\x1bF", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::DecSpecialGraphics
        );
    }

    #[test]
    fn vt52_graphics_mode_off() {
        let (mut screen, mut viewport) = setup();
        // Enable then disable in the same parse pass.
        feed(b"\x1b[?2l\x1bF\x1bG", &mut screen, &mut viewport);
        assert_eq!(
            screen.charset.designated(GraphicSetSlot::G0),
            CharacterSet::Ascii
        );
    }

    /// VT52 ESC Z identify returns ESC / Z.
    #[test]
    fn vt52_identify() {
        let (mut screen, mut viewport) = setup();
        let out = feed_with_output(b"\x1b[?2l\x1bZ", &mut screen, &mut viewport);
        assert_eq!(out, b"\x1b/Z");
    }

    /// CSI sequences are silently dropped in VT52 mode.
    #[test]
    fn vt52_csi_suppressed() {
        let (mut screen, mut viewport) = setup();
        // Position cursor at col 5 (1-based col 6), enter VT52, send CSI CUB.
        feed(b"\x1b[1;6H\x1b[?2l\x1b[3D", &mut screen, &mut viewport);
        // CSI cursor-back should have been dropped.
        assert_eq!(screen.cursor.col, 5, "cursor should not move in VT52 mode");
    }

    /// VT52 reverse index (ESC I) scrolls down at the top of the scroll region.
    #[test]
    fn vt52_reverse_index_scrolls() {
        let (mut screen, mut viewport) = setup();
        // Fill row 0 with text, CUP to row 0, enter VT52, reverse index.
        feed(
            b"line0\r\nline1\r\nline2\x1b[1;1H\x1b[?2l\x1bI",
            &mut screen,
            &mut viewport,
        );
        // Row 0 should now be blank (scrolled down).
        let r0 = row_text(&screen, &viewport, 0);
        assert_eq!(r0.trim(), "");
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::ConformanceLevel;
    use crate::ProgramAllowlist;
    use crate::test_support::TestTerm;

    fn visible_text(term: &TestTerm) -> String {
        let mut s = String::new();
        for row in 0..term.viewport.rows {
            let row = term.visible_row(row);
            for cell in &row.cells {
                s.push_str(cell);
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn bel_byte_sets_bell_pending() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        assert!(!term.take_bell_pending());
        term.process(b"\x07");
        assert!(term.take_bell_pending());
        assert!(!term.take_bell_pending());
    }

    #[test]
    fn bel_inside_text_is_caught() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"hi\x07there");
        assert!(term.take_bell_pending());
    }

    #[test]
    fn bel_does_not_advance_cursor() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x07");
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.cursor.row, 0);
    }

    #[test]
    fn da1_replies_vt420() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn da1_with_zero_param_also_replies() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[0c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn da2_replies_as_vt420_compatible() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>c");
        assert_eq!(term.take_pending_output(), b"\x1b[>41;0;0c");
    }

    #[test]
    fn decscl_level1_changes_da1_prefix_without_resetting_screen() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"hello\x1b[?1004h\x1b[61\"p");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level1);
        assert_eq!(term.modes.c1_mode, crate::C1Mode::SevenBit);
        assert!(term.modes.focus_reporting);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?61;7;21;22;28;29c");
        let row_text: String = term
            .visible_row(0)
            .cells
            .iter()
            .map(|c| c.as_str())
            .collect();
        assert!(row_text.starts_with("hello"), "row text was {row_text:?}");
    }

    #[test]
    fn decscl_with_8bit_controls_switches_reply_encoding() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[64;2\"p\x1b[>c");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level4);
        assert_eq!(term.modes.c1_mode, crate::C1Mode::EightBit);
        assert_eq!(term.take_pending_output(), b"\x9b>41;0;0c");
    }

    #[test]
    fn s8c1t_is_ignored_in_level1_mode() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[61\"p\x1b G\x1b[>c");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level1);
        assert_eq!(term.modes.c1_mode, crate::C1Mode::SevenBit);
        assert_eq!(term.take_pending_output(), b"\x1b[>41;0;0c");
    }

    #[test]
    fn da1_downgrades_when_macros_are_not_allowlisted() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?63;7;21;22;28;29c");
    }

    #[test]
    fn ris_clears_stored_macros() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.set_macro_permissions(ProgramAllowlist::AllowAll);
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
        term.process(b"\x1bc");
        term.process(b"\x1b[1*z");
        assert!(visible_text(&term).trim().is_empty());
    }

    #[test]
    fn decrqm_reports_vt525_color_private_modes() {
        let mut term = TestTerm::new(10, 3, 10, 16, 8);
        term.process(b"\x1b[?114h\x1b[?115h\x1b[?116h\x1b[?117h");
        term.process(b"\x1b[?114$p\x1b[?115$p\x1b[?116$p\x1b[?117$p");
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[?114;1$y\x1b[?115;1$y\x1b[?116;1$y\x1b[?117;1$y"
        );
    }

    #[test]
    fn page_geometry_commands_queue_host_resize() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[36*|");
        assert_eq!(term.take_pending_host_resize(), Some((80, 36)));
        term.process(b"\x1b[132$|");
        assert_eq!(term.take_pending_host_resize(), Some((132, 36)));
    }

    #[test]
    fn decrqm_reports_permanent_mode_states() {
        let mut term = TestTerm::new(16, 4, 10, 16, 8);
        term.process(b"\x1b[10$p\x1b[20$p\x1b[?60$p");
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[10;4$y\x1b[20;2$y\x1b[?60;4$y"
        );
    }

    #[test]
    fn dectst_power_up_self_test_resets_terminal_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b(0hello");
        term.process(b"\x1b[4;1y");

        assert!(!term.modes.focus_reporting);
        assert_eq!(
            term.active
                .charset
                .designated(crate::charset::GraphicSetSlot::G0),
            crate::charset::CharacterSet::Ascii
        );
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        for r in term.active.grid.rows.iter().rev().take(3) {
            assert_eq!(r.content_len(), 0);
        }
    }

    #[test]
    fn deccolm_ignored_without_mode_40() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?3h");
        assert_eq!(term.viewport.cols, 80);
    }

    #[test]
    fn deccolm_set_resizes_to_132_and_clears() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"hello");
        assert_eq!(term.viewport.cols, 80);
        term.process(b"\x1b[?40h\x1b[?3h");
        assert_eq!(term.viewport.cols, 132);
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.scroll_top, 0);
        let first_vis = term.active.grid.rows.len() - 24;
        assert_eq!(term.active.grid.rows[first_vis].cells[0].as_str(), " ");
    }

    #[test]
    fn deccolm_reset_restores_original_width() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?40h");
        term.process(b"\x1b[?3h");
        assert_eq!(term.viewport.cols, 132);
        term.process(b"\x1b[?3l");
        assert_eq!(term.viewport.cols, 80);
        assert_eq!(term.active.cursor.row, 0);
    }
}

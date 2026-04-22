use std::sync::LazyLock;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use smol_str::SmolStr;
use vtepp::Params;

use crate::C1Mode;
use crate::ConformanceLevel;
use crate::TerminalModes;
use crate::charset;
use crate::charset::CharacterSet;
use crate::charset::GraphicSetSlot;
use crate::color;
use crate::cursor::CursorStyle;
use crate::dec::color::DecColorState;
use crate::dec::color::effective_palette;
use crate::dec::color::erase_background_color;
use crate::dec::r#macro::MacroStore;
use crate::dec_color_state_from_palette;
use crate::drcs::DrcsStore;
use crate::io::keyboard::KittyKeyboardState;
use crate::mode;
use crate::screen;
use crate::screen::ActiveDisplay;
use crate::screen::Screen;
use crate::screen::StatusDisplayKind;
use crate::screen::grid;
use crate::screen::grid::Viewport;
use crate::screen::row::LineAttr;

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
        // SAFETY: `i` is produced by `0..95`, so `b = 0x20 + i` is in
        // 0x20..=0x7E. Every byte in that range is valid single-byte UTF-8.
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
const fn continuation_cell() -> SmolStr {
    SmolStr::new_inline("")
}

const fn blank_cell() -> SmolStr {
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
pub(super) mod test_support {
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::FeaturePermissions;
    use crate::cursor::CursorStyle;
    use crate::io::keyboard::KittyKeyboardState;
    use crate::screen::Screen;

    pub(super) const TEST_COLS: u32 = 10;
    pub(super) const TEST_ROWS: u32 = 4;

    pub(super) fn setup() -> (Screen, Viewport) {
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

    pub(super) fn parse_csi_action(input: &[u8]) -> ParsedCsiAction {
        let (screen, _) = setup();
        let modes = TerminalModes::new();
        parse_csi_action_with(input, &screen, &modes)
    }

    pub(super) fn parse_csi_action_with(
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

    pub(super) fn parse_esc_action(
        input: &[u8],
        modes: &TerminalModes,
    ) -> ParsedEscAction {
        let drcs = DrcsStore::default();
        parse_esc_action_with(input, modes, &drcs)
    }

    pub(super) fn parse_esc_action_with(
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
    pub(super) fn feed(
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

    pub(super) fn row_text(
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

    /// Like `feed` but returns the `pending_output` bytes written by query
    /// responses (DECRQM, DSR, etc.).
    pub(super) fn feed_with_output(
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
}

#[cfg(test)]
mod integration_tests {
    use crate::ConformanceLevel;
    use crate::FeaturePermissions;
    use crate::ProgramAllowlist;
    use crate::settings;
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
        let macros = ProgramAllowlist::AllowAll;
        settings::set_feature_permissions(&mut term.inner.protocol, FeaturePermissions { macros });
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

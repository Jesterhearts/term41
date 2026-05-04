use std::ops::Index;

use config41::ColorPalette;
use config41::CursorStyle;
use font41::attrs::CellAttrs;
use smol_str::SmolStr;
use vtepp::Params;

use crate::C1Mode;
use crate::ConformanceLevel;
use crate::ShellIntegrationPhase;
use crate::TerminalModes;
use crate::charset;
use crate::charset::CharacterSet;
use crate::charset::GraphicSetSlot;
use crate::dec::color::DecColorState;
use crate::dec::color::effective_palette;
use crate::dec::color::erase_background_color;
use crate::dec::r#macro::MacroStore;
use crate::dec::udk::UdkState;
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
pub(crate) use self::write::put_printable_with_scrollback_policy_and_emoji_compat;
pub(crate) use self::write::put_status_8bit_byte;
pub(crate) use self::write::put_status_ascii_run;
pub(crate) use self::write::put_status_printable;
pub(crate) use self::write::put_status_text_run;
#[cfg(test)]
pub(crate) use self::write::put_text_run;
pub(crate) use self::write::put_text_run_with_scrollback_policy_and_emoji_compat;

const fn ascii_cell(byte: u8) -> SmolStr {
    match byte {
        0x20..=0x7E => SmolStr::new_inline(unsafe {
            std::str::from_utf8_unchecked(std::slice::from_ref(&byte))
        }),
        _ => unreachable!(),
    }
}

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
    screen::ensure_cursor_row_exists(screen, viewport);
    let cols = current_row_display_cols(screen, viewport);
    if screen.cursor.col >= cols {
        screen.cursor.col = cols.saturating_sub(1);
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct BorrowedParams<'a> {
    params: &'a Params,
    len: usize,
}

impl<'a> BorrowedParams<'a> {
    pub(crate) fn from_vte(params: &'a Params) -> Self {
        let len = params.iter().count();
        Self { params, len }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &'a [u16]> + '_ {
        self.params.iter()
    }

    pub(crate) fn get(
        &self,
        idx: usize,
    ) -> Option<&'a [u16]> {
        self.params.iter().nth(idx)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

impl Index<usize> for BorrowedParams<'_> {
    type Output = [u16];

    fn index(
        &self,
        index: usize,
    ) -> &Self::Output {
        self.get(index).expect("index in range")
    }
}

#[derive(Debug, Clone)]
pub(super) enum ParsedCsiAction<'a> {
    Unsupported,
    SetPrivateModes {
        enable: bool,
        modes: BorrowedParams<'a>,
    },
    SavePrivateModes {
        modes: BorrowedParams<'a>,
    },
    RestorePrivateModes {
        modes: BorrowedParams<'a>,
    },
    SelectiveEraseDisplay {
        mode: u16,
    },
    SelectiveEraseLine {
        mode: u16,
    },
    KittyKeyboard {
        intermediate: u8,
        params: BorrowedParams<'a>,
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
        params: BorrowedParams<'a>,
    },
    SelectiveEraseRect {
        params: BorrowedParams<'a>,
    },
    FillRect {
        params: BorrowedParams<'a>,
    },
    CopyRect {
        params: BorrowedParams<'a>,
    },
    ChangeRectAttrs {
        params: BorrowedParams<'a>,
    },
    ReverseRectAttrs {
        params: BorrowedParams<'a>,
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
    SetLocalFunctions {
        params: BorrowedParams<'a>,
    },
    SetLocalFunctionKeys {
        params: BorrowedParams<'a>,
    },
    SetModifierKeyReporting {
        params: BorrowedParams<'a>,
    },
    ReportXtVersion,
    ReportSecondaryDeviceAttrs,
    ReportTertiaryDeviceAttrs,
    Main(MainCsiAction<'a>),
    StatusLine(StatusLineCsiAction<'a>),
}

#[derive(Debug, Clone)]
pub(super) enum MainCsiAction<'a> {
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
        params: BorrowedParams<'a>,
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
pub(super) enum StatusLineCsiAction<'a> {
    SetGraphicsRendition { params: BorrowedParams<'a> },
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum AsciiControlBytes {
    Nul = 0x00,
    Bell = 0x07,
    Backspace = 0x08,
    HorizontalTab = 0x09,
    LineFeed = 0x0A,
    VerticalTab = 0x0B,
    FormFeed = 0x0C,
    CarriageReturn = 0x0D,
    ShiftOut = 0x0E,
    ShiftIn = 0x0F,
}

impl TryFrom<u8> for AsciiControlBytes {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Nul),
            0x07 => Ok(Self::Bell),
            0x08 => Ok(Self::Backspace),
            0x09 => Ok(Self::HorizontalTab),
            0x0A => Ok(Self::LineFeed),
            0x0B => Ok(Self::VerticalTab),
            0x0C => Ok(Self::FormFeed),
            0x0D => Ok(Self::CarriageReturn),
            0x0E => Ok(Self::ShiftOut),
            0x0F => Ok(Self::ShiftIn),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum DsrParameters {
    Ok = 5,
    Cpr = 6,
}

impl TryFrom<u16> for DsrParameters {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            5 => Ok(Self::Ok),
            6 => Ok(Self::Cpr),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum WinManipulationAction {
    TitlePush = 22,
    TitlePop = 23,
    ReportPixels = 14,
    ReportCellSize = 16,
    ReportTextSize = 18,
}

impl TryFrom<u16> for WinManipulationAction {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            22 => Ok(Self::TitlePush),
            23 => Ok(Self::TitlePop),
            14 => Ok(Self::ReportPixels),
            16 => Ok(Self::ReportCellSize),
            18 => Ok(Self::ReportTextSize),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TabClearMode {
    Current = 0,
    All = 3,
}

impl TryFrom<u16> for TabClearMode {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Current),
            3 => Ok(Self::All),
            _ => Err(()),
        }
    }
}

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
    default_cursor_style: CursorStyle,
    cursor_style: &mut CursorStyle,
    saved_alt_cursor_style: &mut Option<CursorStyle>,
    current_title: &mut Option<String>,
    title_stack: &mut Vec<Option<String>>,
    saved_modes: &mut std::collections::HashMap<mode::PrivateMode, bool>,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    bell_pending: &mut bool,
    vt52_cursor_addr: &mut crate::Vt52CursorAddr,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    default_status_display: &StatusDisplayKind,
    macros: &mut MacroStore,
    udks: &mut UdkState,
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
        s.scrollback_blocks.clear();
        s.active_command_block_started = false;
        s.cursor = grid::Cursor::default();
        s.fg = palette.fg;
        s.bg = palette.bg;
        s.attrs = CellAttrs::default();
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
    *cursor_style = default_cursor_style;
    *saved_alt_cursor_style = None;
    *current_title = None;
    title_stack.clear();
    saved_modes.clear();
    *current_prompt_row = None;
    *shell_integration_phase = ShellIntegrationPhase::None;
    *bell_pending = false;
    *vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
    macros.clear();
    udks.clear();
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
    use config41::default_bg;
    use config41::default_fg;
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::FeaturePermissions;
    use crate::io::keyboard::KittyKeyboardState;
    use crate::screen::Screen;

    pub(super) const TEST_COLS: u32 = 10;
    pub(super) const TEST_ROWS: u32 = 4;

    pub(super) fn setup() -> (Screen, Viewport) {
        let screen = Screen::new(
            TEST_COLS,
            TEST_ROWS,
            100,
            default_fg(),
            default_bg(),
            default_fg(),
            default_bg(),
        );
        let viewport = Viewport {
            rows: TEST_ROWS,
            cols: TEST_COLS,
            top: 0,
        };
        (screen, viewport)
    }

    pub(super) fn with_csi_action<R>(
        input: &[u8],
        f: impl for<'a> FnOnce(ParsedCsiAction<'a>) -> R,
    ) -> R {
        let (screen, _) = setup();
        let modes = TerminalModes::new();
        with_csi_action_and(input, &screen, &modes, f)
    }

    pub(super) fn with_csi_action_and<R>(
        input: &[u8],
        screen: &Screen,
        modes: &TerminalModes,
        f: impl for<'a> FnOnce(ParsedCsiAction<'a>) -> R,
    ) -> R {
        let mut parser = Parser::new();
        for action in parser.parse(input) {
            if let Action::CsiDispatch {
                params,
                intermediates,
                action,
            } = action
            {
                return f(csi_parse(
                    screen,
                    modes,
                    &params,
                    intermediates.as_slice(),
                    action,
                ));
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
        let base_pal = ColorPalette::default();
        let mut dec_color = dec_color_state_from_palette(&base_pal);
        let mut pal = effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            default_fg(),
            default_bg(),
            default_fg(),
            default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let default_cursor_style = CursorStyle::default();
        let mut cursor_style = CursorStyle::default();
        let mut saved_alt_cursor_style = None;
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut shell_integration_phase = ShellIntegrationPhase::None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();
        let mut udks = UdkState::default();

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
                        .default_cursor_style(default_cursor_style)
                        .cursor_style(&mut cursor_style)
                        .saved_alt_cursor_style(&mut saved_alt_cursor_style)
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
                        .udks(&mut udks)
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
                        .default_cursor_style(default_cursor_style)
                        .cursor_style(&mut cursor_style)
                        .saved_alt_cursor_style(&mut saved_alt_cursor_style)
                        .current_title(&mut current_title)
                        .title_stack(&mut title_stack)
                        .saved_modes(&mut saved_modes)
                        .current_prompt_row(&mut current_prompt_row)
                        .shell_integration_phase(&mut shell_integration_phase)
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
                        .udks(&mut udks)
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
        let base_pal = ColorPalette::default();
        let mut dec_color = dec_color_state_from_palette(&base_pal);
        let mut pal = effective_palette(&base_pal, &dec_color);
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            default_fg(),
            default_bg(),
            default_fg(),
            default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut pending_resize = None;
        let default_cursor_style = CursorStyle::default();
        let mut cursor_style = CursorStyle::default();
        let mut saved_alt_cursor_style = None;
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut shell_integration_phase = ShellIntegrationPhase::None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
        let mut default_status_display = StatusDisplayKind::None;
        let feature_permissions = FeaturePermissions::default();
        let mut macros = MacroStore::default();
        let mut drcs = DrcsStore::default();
        let mut udks = UdkState::default();

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
                        .default_cursor_style(default_cursor_style)
                        .cursor_style(&mut cursor_style)
                        .saved_alt_cursor_style(&mut saved_alt_cursor_style)
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
                        .udks(&mut udks)
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
                        .default_cursor_style(default_cursor_style)
                        .cursor_style(&mut cursor_style)
                        .saved_alt_cursor_style(&mut saved_alt_cursor_style)
                        .current_title(&mut current_title)
                        .title_stack(&mut title_stack)
                        .saved_modes(&mut saved_modes)
                        .current_prompt_row(&mut current_prompt_row)
                        .shell_integration_phase(&mut shell_integration_phase)
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
                        .udks(&mut udks)
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
    use config41::ProgramAllowlist;

    use crate::ConformanceLevel;
    use crate::FeaturePermissions;
    use crate::TerminalLimits;
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
    fn da1_advertises_udks_only_when_allowlisted() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                udks: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?64;7;8;21;22;28;29c");
    }

    #[test]
    fn decudk_is_default_denied() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1bP0;1|17/414243\x1b\\");
        assert_eq!(term.user_defined_key(17), None);
    }

    #[test]
    fn decudk_loads_when_allowlisted() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                udks: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        term.process(b"\x1bP0;1|17/414243\x1b\\");
        assert_eq!(term.user_defined_key(17), Some(b"ABC".to_vec()));
    }

    #[test]
    fn decudk_payload_limit_rejects_oversized_loads() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                udks: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        settings::set_terminal_limits(
            &mut term.inner.protocol,
            TerminalLimits {
                decudk_payload_bytes: 4,
                ..TerminalLimits::default()
            },
        );
        term.process(b"\x1bP0;1|17/414243\x1b\\");
        assert_eq!(term.user_defined_key(17), None);
    }

    #[test]
    fn private_dsr_reports_udk_lock_state_when_allowlisted() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                udks: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        term.process(b"\x1b[?25n");
        assert_eq!(term.take_pending_output(), b"\x1b[?21n");
        term.process(b"\x1bP0;0|17/41\x1b\\");
        term.process(b"\x1b[?25n");
        assert_eq!(term.take_pending_output(), b"\x1b[?20n");
    }

    #[test]
    fn dec_keyboard_controls_are_default_denied() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[1;3*}\x1b[1;2+r");
        assert_eq!(term.local_function_key_control(1), None);
        assert_eq!(
            term.dec_modifier_key_report(crate::DecModifierKey::LeftShift, true),
            None
        );
    }

    #[test]
    fn dec_keyboard_controls_apply_when_allowlisted() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                udks: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        term.process(b"\x1b[1;3*}\x1b[1;2+r");
        assert_eq!(
            term.local_function_key_control(1),
            Some(crate::LocalFunctionKeyControl::Disabled)
        );
        assert_eq!(
            term.dec_modifier_key_report(crate::DecModifierKey::LeftShift, true),
            Some(b"\x1b_:0011\x1b\\".to_vec())
        );
    }

    #[test]
    fn ris_clears_stored_macros() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        let macros = ProgramAllowlist::AllowAll;
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                macros,
                ..FeaturePermissions::default()
            },
        );
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
        term.process(b"\x1bc");
        term.process(b"\x1b[1*z");
        assert!(visible_text(&term).trim().is_empty());
    }

    #[test]
    fn macro_storage_limit_rejects_oversized_definitions() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                macros: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        settings::set_terminal_limits(
            &mut term.inner.protocol,
            TerminalLimits {
                macro_storage_bytes: 2,
                ..TerminalLimits::default()
            },
        );
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
        term.process(b"\x1b[1*z");
        assert!(visible_text(&term).trim().is_empty());
    }

    #[test]
    fn macro_invocation_depth_limit_blocks_expansion() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        settings::set_feature_permissions(
            &mut term.inner.protocol,
            FeaturePermissions {
                macros: ProgramAllowlist::AllowAll,
                ..FeaturePermissions::default()
            },
        );
        settings::set_terminal_limits(
            &mut term.inner.protocol,
            TerminalLimits {
                macro_invocation_depth: 0,
                ..TerminalLimits::default()
            },
        );
        term.process(b"\x1bP1;1;1!z414243\x1b\\");
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
        let first_vis = term.active.grid.rows.len().saturating_sub(24);
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

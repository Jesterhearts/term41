use std::collections::BTreeMap;
use std::collections::VecDeque;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use palette::Srgb;
use smol_str::SmolStr;

use crate::charset::CharsetState;
use crate::charset::UserPreferredSupplementalSet;
use crate::grid::AttrChangeExtent;
use crate::grid::Cursor;
use crate::grid::Grid;
use crate::grid::Viewport;
use crate::hyperlink::HyperlinkId;
use crate::image::PlacedImage;
use crate::image::anchor_images;
use crate::image::clear_in_range;
use crate::image::restore_images;
use crate::mode;
use crate::row::Row;

/// Snapshot of cursor position and active colors, used by DECSC/DECRC
/// (ESC 7 / ESC 8) and the `?1048`/`?1049` private modes.
#[derive(Debug, Clone, Copy)]
pub struct SavedCursor {
    pub cursor: Cursor,
    pub fg: Srgb<u8>,
    pub bg: Srgb<u8>,
    pub attrs: CellAttrs,
    pub underline: UnderlineStyle,
    pub underline_color: Option<Srgb<u8>>,
    pub origin_mode: bool,
    pub charset: CharsetState,
}

#[derive(Debug, Clone)]
pub struct PageMemory {
    /// Total number of lines in each logical page.
    pub lines_per_page: u32,
    /// Local row index of each page's first row inside `grid.rows`.
    pub page_starts: Vec<usize>,
    /// Currently displayed page.
    pub active_page: u32,
    /// Top visible line within the active page.
    pub display_top: u32,
}

impl PageMemory {
    pub fn page_count(&self) -> u32 {
        self.page_starts.len() as u32
    }

    pub fn active_page_start(&self) -> usize {
        self.page_starts[self.active_page as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDisplay {
    Main,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusDisplayKind {
    None,
    Indicator,
    HostWritable,
}

#[derive(Debug)]
pub struct StatusLine {
    pub row: Row,
    pub cursor: Cursor,
    pub fg: Srgb<u8>,
    pub bg: Srgb<u8>,
    pub attrs: CellAttrs,
    pub underline: UnderlineStyle,
    pub underline_color: Option<Srgb<u8>>,
    pub current_hyperlink: Option<HyperlinkId>,
    pub last_char: Option<SmolStr>,
}

impl StatusLine {
    fn new(
        cols: u32,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
    ) -> Self {
        Self {
            row: Row::new(cols, fg, bg),
            cursor: Cursor::default(),
            fg,
            bg,
            attrs: CellAttrs::default(),
            underline: UnderlineStyle::None,
            underline_color: None,
            current_hyperlink: None,
            last_char: None,
        }
    }
}

/// State for a single screen buffer (primary or alt). The terminal holds
/// two of these — an `active` and a `stash` — and swaps between them with
/// a single [`std::mem::swap`] on the alt-screen mode transitions.
#[derive(Debug)]
pub struct Screen {
    pub grid: Grid,
    pub cursor: Cursor,
    pub fg: Srgb<u8>,
    pub bg: Srgb<u8>,
    /// Current text attributes (bold/italic/strikethrough) applied to new cell
    /// writes. Managed via SGR — updated by `apply_sgr`, snapshotted into
    /// `SavedCursor` on DECSC.
    pub attrs: CellAttrs,
    /// Current underline style applied to new cell writes.
    pub underline: UnderlineStyle,
    /// Current underline color override. `None` = use foreground color.
    pub underline_color: Option<Srgb<u8>>,
    /// Top row of the scroll region (0-indexed, inclusive).
    pub scroll_top: u32,
    /// Bottom row of the scroll region (0-indexed, inclusive).
    pub scroll_bottom: u32,
    /// Left column of the horizontal margin region (0-indexed, inclusive).
    /// Only active when DECLRMM (mode 69) is set.
    pub left_margin: u32,
    /// Right column of the horizontal margin region (0-indexed, inclusive).
    /// Only active when DECLRMM (mode 69) is set.
    pub right_margin: u32,
    /// Viewport scroll-back offset. 0 = viewing the live terminal,
    /// positive = scrolled into history. Alt screens keep this at 0 since
    /// their grid has no scrollback.
    pub offset: u32,
    pub images: BTreeMap<u64, PlacedImage>,
    pub saved_cursor: Option<SavedCursor>,
    /// Hyperlink id currently associated with new cell writes (set by OSC 8).
    /// Lives on the screen, not the terminal, so a link span open on the
    /// primary screen doesn't bleed into the alt screen and vice versa.
    pub current_hyperlink: Option<HyperlinkId>,
    /// DECTCEM (`?25`) cursor visibility. `true` by default (xterm's initial
    /// state); an app hides the cursor with `CSI ? 25 l` and restores it with
    /// `CSI ? 25 h`. Per-screen so an alt-screen full-screen TUI that hides
    /// the cursor doesn't leave the primary screen hidden on exit.
    pub cursor_visible: bool,
    /// Last character placed by `put_char` or `put_ascii_run`, used by REP
    /// (`CSI Ps b`) to repeat the preceding graphic character.
    pub last_char: Option<SmolStr>,
    /// Per-column tab stops. `tab_stops[c]` is `true` when column `c` is a
    /// tab stop. Defaults to every 8 columns (8, 16, 24, ...).
    pub tab_stops: Vec<bool>,
    /// DECOM — when set, cursor addressing (CUP, VPA) is relative to the
    /// scroll region rather than the full screen. Homes the cursor on toggle.
    pub origin_mode: bool,
    /// `true` when DECNRCM is active and NRC designations should replace
    /// their ASCII positions.
    pub nrc_mode: bool,
    /// Current user-preferred supplemental set for `<` designations and
    /// DECRQUPSS reporting.
    pub upss: UserPreferredSupplementalSet,
    /// Designated sets plus GL/GR invocation state for DEC/ISO-2022-style
    /// character-set handling.
    pub charset: CharsetState,
    /// DECAWM (`?7`) — when true (default), printing past the right margin
    /// wraps to the next line. When false, the cursor stays at the right
    /// margin and overwrites the last column.
    pub autowrap: bool,
    /// DECCKM (`?1`) — when true, unmodified arrow keys send SS3 form
    /// (ESC O A/B/C/D) instead of CSI form (ESC [ A/B/C/D). Modified
    /// arrows still use the CSI modifier form. Default is false (normal
    /// cursor keys).
    pub app_cursor_keys: bool,
    /// DECSACE — whether DECCARA/DECRARA operate on a stream of character
    /// positions or on the full rectangular area.
    pub attr_change_extent: AttrChangeExtent,
    /// DECKPAM / DECKPNM — when true (application keypad mode), the
    /// numeric keypad sends SS3 sequences instead of their normal
    /// characters. Set by ESC = (DECKPAM) or DECNKM (`?66 h`); cleared
    /// by ESC > (DECKPNM) or DECNKM (`?66 l`).
    pub app_keypad: bool,
    /// VT420 page-memory state. `None` keeps the legacy "visible rows are
    /// the live tail of the grid" behavior. When present, the visible
    /// screen is an explicit slice of rows within page memory.
    pub page_memory: Option<PageMemory>,
    /// `DECSASD` — which display surface receives host output.
    pub active_display: ActiveDisplay,
    /// `DECSSDT` — whether the status line is absent, emulator-owned, or
    /// host-writable.
    pub status_display: StatusDisplayKind,
    /// Dedicated one-row status-line storage. Present whenever
    /// `status_display != None`.
    pub status_line: Option<StatusLine>,
}

impl Screen {
    pub(super) fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
        _status_fg: Srgb<u8>,
        _status_bg: Srgb<u8>,
    ) -> Self {
        let mut grid_rows = VecDeque::with_capacity(rows as usize + scrollback_limit as usize);
        for _ in 0..rows {
            grid_rows.push_back(Row::new(cols, fg, bg));
        }
        Self {
            grid: Grid {
                rows: grid_rows,
                scrollback_limit,
                total_popped: 0,
                default_fg: fg,
                default_bg: bg,
            },
            cursor: Cursor::default(),
            fg,
            bg,
            attrs: CellAttrs::default(),
            underline: UnderlineStyle::None,
            underline_color: None,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            left_margin: 0,
            right_margin: cols.saturating_sub(1),
            offset: 0,
            images: BTreeMap::new(),
            saved_cursor: None,
            current_hyperlink: None,
            cursor_visible: true,
            last_char: None,
            tab_stops: init_tab_stops(cols),
            origin_mode: false,
            nrc_mode: false,
            upss: UserPreferredSupplementalSet::DecSupplemental,
            charset: CharsetState::new(),
            autowrap: true,
            app_cursor_keys: false,
            attr_change_extent: AttrChangeExtent::Stream,
            app_keypad: false,
            page_memory: None,
            active_display: ActiveDisplay::Main,
            status_display: StatusDisplayKind::None,
            status_line: None,
        }
    }
}

pub(super) fn status_line_visible(screen: &Screen) -> bool {
    screen.status_display != StatusDisplayKind::None && screen.status_line.is_some()
}

pub(super) fn status_line_writable(screen: &Screen) -> bool {
    screen.status_display == StatusDisplayKind::HostWritable && screen.status_line.is_some()
}

pub(super) fn status_line_rows(screen: &Screen) -> u32 {
    u32::from(status_line_visible(screen))
}

pub(super) fn ensure_status_line(
    screen: &mut Screen,
    cols: u32,
    fg: Srgb<u8>,
    bg: Srgb<u8>,
) -> &mut StatusLine {
    screen
        .status_line
        .get_or_insert_with(|| StatusLine::new(cols, fg, bg))
}

pub(super) fn resize_status_line(
    screen: &mut Screen,
    cols: u32,
) {
    let Some(status) = screen.status_line.as_mut() else {
        return;
    };
    status.row.resize(cols, status.fg, status.bg);
    status.cursor.col = status.cursor.col.min(cols.saturating_sub(1));
}

pub(super) fn set_status_display(
    screen: &mut Screen,
    cols: u32,
    status_display: StatusDisplayKind,
    status_fg: Srgb<u8>,
    status_bg: Srgb<u8>,
) {
    screen.status_display = status_display;
    match status_display {
        StatusDisplayKind::None => {
            screen.status_line = None;
            screen.active_display = ActiveDisplay::Main;
        }
        StatusDisplayKind::Indicator | StatusDisplayKind::HostWritable => {
            resize_status_line(screen, cols);
            let status = ensure_status_line(screen, cols, status_fg, status_bg);
            status.fg = status_fg;
            status.bg = status_bg;
            if status_display != StatusDisplayKind::HostWritable
                && screen.active_display == ActiveDisplay::Status
            {
                screen.active_display = ActiveDisplay::Main;
            }
        }
    }
}

pub(super) fn screen_viewport(
    screen: &Screen,
    viewport: &Viewport,
) -> Viewport {
    let mut view = Viewport {
        rows: viewport.rows,
        cols: viewport.cols,
        top: screen
            .grid
            .rows
            .len()
            .saturating_sub(viewport.rows as usize),
    };
    if let Some(page) = screen.page_memory.as_ref() {
        view.top = page.active_page_start() + page.display_top as usize;
    }
    view
}

pub(super) fn active_row_index(
    screen: &Screen,
    viewport: &Viewport,
) -> usize {
    if page_memory_active(screen) {
        return viewport.top_index(screen.grid.rows.len()) + screen.cursor.row as usize;
    }

    screen
        .grid
        .rows
        .len()
        .saturating_sub(viewport.rows as usize)
        + screen.cursor.row as usize
}

pub(super) fn page_count_for_lines(lines_per_page: u32) -> u32 {
    144 / lines_per_page.max(1)
}

pub(super) fn page_memory_active(screen: &Screen) -> bool {
    screen.page_memory.is_some()
}

pub(super) fn activate_page_memory(
    screen: &mut Screen,
    viewport: &Viewport,
    lines_per_page: u32,
) {
    let lines_per_page = lines_per_page.max(viewport.rows).max(1);
    if screen.page_memory.is_some() {
        resize_page_memory(screen, viewport, lines_per_page);
        if let Some(page) = screen.page_memory.as_mut() {
            page.display_top = page
                .display_top
                .min(lines_per_page.saturating_sub(viewport.rows));
        }
        return;
    }

    let view = screen_viewport(screen, viewport);
    let page_count = page_count_for_lines(lines_per_page) as usize;
    let page0_start = view.top_index(screen.grid.rows.len());
    let required_tail_rows = page_count * lines_per_page as usize;
    let current_tail_rows = screen.grid.rows.len().saturating_sub(page0_start);
    if current_tail_rows < required_tail_rows {
        let missing = required_tail_rows - current_tail_rows;
        for _ in 0..missing {
            screen.grid.rows.push_back(Row::new(
                viewport.cols,
                screen.grid.default_fg,
                screen.grid.default_bg,
            ));
        }
    }
    let page_starts = (0..page_count)
        .map(|idx| page0_start + idx * lines_per_page as usize)
        .collect();
    screen.page_memory = Some(PageMemory {
        lines_per_page,
        page_starts,
        active_page: 0,
        display_top: 0,
    });
}

pub(super) fn resize_page_memory(
    screen: &mut Screen,
    viewport: &Viewport,
    lines_per_page: u32,
) {
    let lines_per_page = lines_per_page.max(viewport.rows).max(1);
    let Some(page) = screen.page_memory.as_mut() else {
        activate_page_memory(screen, viewport, lines_per_page);
        return;
    };

    let page_count = page_count_for_lines(lines_per_page) as usize;
    let active_page = page.active_page.min(page_count.saturating_sub(1) as u32);
    let display_top = page
        .display_top
        .min(lines_per_page.saturating_sub(viewport.rows));
    let page0_start = page.page_starts.first().copied().unwrap_or(0);
    let required_tail_rows = page_count * lines_per_page as usize;
    let current_tail_rows = screen.grid.rows.len().saturating_sub(page0_start);
    if current_tail_rows < required_tail_rows {
        let missing = required_tail_rows - current_tail_rows;
        for _ in 0..missing {
            screen.grid.rows.push_back(Row::new(
                viewport.cols,
                screen.grid.default_fg,
                screen.grid.default_bg,
            ));
        }
    } else if current_tail_rows > required_tail_rows {
        screen.grid.rows.truncate(page0_start + required_tail_rows);
    }
    page.lines_per_page = lines_per_page;
    page.page_starts = (0..page_count)
        .map(|idx| page0_start + idx * lines_per_page as usize)
        .collect();
    page.active_page = active_page;
    page.display_top = display_top;
}

pub(super) fn page_rows(screen: &Screen) -> Option<u32> {
    screen.page_memory.as_ref().map(|page| page.lines_per_page)
}

pub(super) fn ensure_page_memory(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    if screen.page_memory.is_none() {
        activate_page_memory(screen, viewport, viewport.rows);
    }
}

pub(super) fn page_viewport(
    screen: &Screen,
    viewport: &Viewport,
    page_number: u16,
) -> Option<Viewport> {
    let page_number = page_number.max(1) as usize;
    let Some(page) = screen.page_memory.as_ref() else {
        return (page_number == 1).then_some(screen_viewport(screen, viewport));
    };
    let page_index = page_number - 1;
    let page_start = *page.page_starts.get(page_index)?;
    Some(Viewport {
        rows: viewport.rows.min(page.lines_per_page),
        cols: viewport.cols,
        top: page_start,
    })
}

pub(super) fn page_can_scroll_down(
    screen: &Screen,
    viewport: &Viewport,
) -> bool {
    screen
        .page_memory
        .as_ref()
        .is_some_and(|page| page.display_top + viewport.rows < page.lines_per_page)
}

pub(super) fn scroll_page_down(
    screen: &mut Screen,
    viewport: &Viewport,
    lines: u32,
) {
    let Some(page) = screen.page_memory.as_mut() else {
        return;
    };
    let max_top = page.lines_per_page.saturating_sub(viewport.rows);
    page.display_top = (page.display_top + lines).min(max_top);
}

/// Create the default tab-stop pattern: a stop every 8 columns
/// (i.e. columns 8, 16, 24, ...). Column 0 is never a stop.
pub(super) fn init_tab_stops(cols: u32) -> Vec<bool> {
    let mut stops = vec![false; cols as usize];
    let mut c = 8;
    while c < cols as usize {
        stops[c] = true;
        c += 8;
    }
    stops
}

/// Save the active screen's cursor and colors into its DECSC slot
/// (ESC 7 / `?1048h`).
pub(super) fn save_cursor_slot(screen: &mut Screen) {
    screen.saved_cursor = Some(SavedCursor {
        cursor: screen.cursor,
        fg: screen.fg,
        bg: screen.bg,
        attrs: screen.attrs,
        underline: screen.underline,
        underline_color: screen.underline_color,
        origin_mode: screen.origin_mode,
        charset: screen.charset,
    });
}

/// Restore the active screen's cursor and colors from its DECSC slot
/// (ESC 8 / `?1048l`). If the slot is empty the cursor homes to (0, 0)
/// without touching colors — DEC-terminal behavior for an un-saved state.
pub(super) fn restore_cursor_slot(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    match screen.saved_cursor {
        Some(saved) => {
            screen.cursor.row = saved.cursor.row.min(viewport.rows.saturating_sub(1));
            screen.cursor.col = saved.cursor.col.min(viewport.cols.saturating_sub(1));
            screen.fg = saved.fg;
            screen.bg = saved.bg;
            screen.attrs = saved.attrs;
            screen.underline = saved.underline;
            screen.underline_color = saved.underline_color;
            screen.origin_mode = saved.origin_mode;
            screen.charset = saved.charset;
        }
        None => {
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
    }
}

/// Clear every cell of the visible area. Leaves any scrollback untouched.
/// Also drops images anchored to visible rows — an alt-screen transition
/// that left sixel images behind would render them on top of the fresh
/// screen the app is about to draw.
pub(super) fn clear_visible(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    let first_visible = screen
        .grid
        .rows
        .len()
        .saturating_sub(viewport.rows as usize);
    let fg = screen.grid.default_fg;
    let bg = screen.grid.default_bg;
    for r in first_visible..screen.grid.rows.len() {
        screen.grid.rows[r].clear(fg, bg);
    }
    clear_in_range(&mut screen.images, first_visible, screen.grid.rows.len());
    if let Some(status) = screen.status_line.as_mut() {
        status.row.clear(fg, bg);
        status.cursor = Cursor::default();
    }
}

/// Switch between the primary and alt screens. Idempotent: a no-op if the
/// target screen is already active.
fn switch_screen(
    target_alt: bool,
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt: &mut bool,
) {
    if *on_alt == target_alt {
        return;
    }
    let total_rows = viewport.rows + status_line_rows(active);
    std::mem::swap(active, stash);
    *on_alt = target_alt;
    viewport.rows = total_rows.saturating_sub(status_line_rows(active));
    // Incoming screen's offset is preserved; most apps don't care, and it
    // gives primary back its scroll position on 1049l if the user had
    // scrolled back before the app hijacked the terminal.
}

/// Handle a DECSET/DECRST private mode. `enable` is true for `h` (set),
/// false for `l` (reset). Only the alt-screen family (47/1047/1048/1049)
/// is currently recognized; unknown modes are ignored.
pub(super) fn set_private_mode(
    mode: u16,
    enable: bool,
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &mut Viewport,
    on_alt: &mut bool,
) {
    match mode {
        mode::DECCKM => active.app_cursor_keys = enable,
        // DECCOLM is handled in csi_dispatch where mutable viewport
        // access is available for the resize.
        mode::DECOM => {
            active.origin_mode = enable;
            // Entering/leaving origin mode homes the cursor per DEC spec.
            active.cursor.row = if enable { active.scroll_top } else { 0 };
            active.cursor.col = 0;
        }
        mode::DECAWM => active.autowrap = enable,
        mode::DECNKM => active.app_keypad = enable,
        mode::DECTCEM => active.cursor_visible = enable,
        // DECLRMM does not need action here — the `declrmm` bool lives
        // on TerminalModes and is handled at the csi_dispatch level.
        mode::ALT_SCREEN => switch_screen(enable, active, stash, viewport, on_alt),
        mode::ALT_SCREEN_CLEAR => {
            // xterm clears the alt buffer when leaving via 1047l so stale
            // content isn't re-shown the next time it's entered.
            if !enable && *on_alt {
                clear_visible(active, viewport);
            }
            switch_screen(enable, active, stash, viewport, on_alt);
        }
        mode::SAVE_CURSOR => {
            if enable {
                save_cursor_slot(active);
            } else {
                restore_cursor_slot(active, viewport);
            }
        }
        mode::ALT_SCREEN_SAVE => {
            if enable {
                // Save into primary's DECSC slot before swapping, so the
                // slot rides with primary into the stash and is there for
                // the round trip.
                if !*on_alt {
                    save_cursor_slot(active);
                }
                switch_screen(true, active, stash, viewport, on_alt);
                clear_visible(active, viewport);
            } else {
                if *on_alt {
                    clear_visible(active, viewport);
                }
                switch_screen(false, active, stash, viewport, on_alt);
                restore_cursor_slot(active, viewport);
            }
        }
        _ => {}
    }
}

/// Resize a single screen to new dimensions.
///
/// Reflows soft-wrapped lines when the column count changes, preserves
/// image positions through the reflow via logical-line anchors, clamps
/// the cursor into the new bounds, and resets the scroll region / offset
/// to fit the new viewport.
pub(super) fn resize_screen(
    screen: &mut Screen,
    old_cols: u32,
    old_rows: u32,
    new_cols: u32,
    new_rows: u32,
) {
    resize_status_line(screen, new_cols);

    let grid = &mut screen.grid;
    let cursor = &mut screen.cursor;
    let images = &mut screen.images;

    // Trim trailing empty rows that accumulated from padding in previous
    // resizes, so content stays visible when the viewport shrinks.
    let cursor_abs = grid.rows.len() - old_rows as usize + cursor.row as usize;
    while grid.rows.len() > cursor_abs + 1 {
        if grid.rows.back().is_some_and(|r| r.content_len() == 0) {
            grid.rows.pop_back();
        } else {
            break;
        }
    }
    let effective_old_rows = (old_rows as usize).min(grid.rows.len());
    let visible_start = grid.rows.len().saturating_sub(effective_old_rows);
    cursor.row = cursor_abs.saturating_sub(visible_start) as u32;

    let max_rows = new_rows as usize + grid.scrollback_limit as usize;

    if new_cols as usize != old_cols as usize {
        let anchors = anchor_images(&grid.rows, images);

        let cursor_abs_now = grid.rows.len() - effective_old_rows + cursor.row as usize;
        let old_distance_from_bottom = grid.rows.len().saturating_sub(cursor_abs_now + 1);

        grid.reflow(new_cols);

        while grid.rows.len() > max_rows {
            grid.rows.pop_front();
        }

        restore_images(&grid.rows, &anchors, images);

        let new_abs = grid.rows.len().saturating_sub(old_distance_from_bottom + 1);

        while grid.rows.len() < new_rows as usize {
            grid.rows
                .push_back(Row::new(new_cols, grid.default_fg, grid.default_bg));
        }

        let visible_start = grid.rows.len().saturating_sub(new_rows as usize);
        cursor.row = new_abs
            .saturating_sub(visible_start)
            .min(new_rows as usize - 1) as u32;
        cursor.col = cursor.col.min(new_cols.saturating_sub(1));
    } else {
        let old_len = grid.rows.len();
        let old_abs = grid.rows.len() - effective_old_rows + cursor.row as usize;

        while grid.rows.len() > max_rows {
            grid.rows.pop_front();
        }

        let popped = old_len - grid.rows.len();

        while grid.rows.len() < new_rows as usize {
            grid.rows
                .push_back(Row::new(new_cols, grid.default_fg, grid.default_bg));
        }

        if popped > 0 {
            images.retain(|_, img| img.row >= popped);
            for img in images.values_mut() {
                img.row -= popped;
            }
        }

        let new_abs = old_abs.saturating_sub(popped);
        let visible_start = grid.rows.len().saturating_sub(new_rows as usize);
        cursor.row = new_abs
            .saturating_sub(visible_start)
            .min(new_rows as usize - 1) as u32;
    }

    screen.scroll_top = 0;
    screen.scroll_bottom = new_rows.saturating_sub(1);
    screen.tab_stops = init_tab_stops(new_cols);
    let scrollback = screen.grid.scrollback_len(&Viewport {
        rows: new_rows,
        cols: new_cols,
        top: 0,
    });
    screen.offset = screen.offset.min(scrollback);
}

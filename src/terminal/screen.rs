use std::collections::BTreeMap;
use std::collections::VecDeque;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use palette::Srgb;
use smol_str::SmolStr;

use crate::terminal::color::default_bg;
use crate::terminal::color::default_fg;
use crate::terminal::grid::Cursor;
use crate::terminal::grid::Grid;
use crate::terminal::grid::Viewport;
use crate::terminal::hyperlink::HyperlinkId;
use crate::terminal::image::PlacedImage;
use crate::terminal::image::anchor_images;
use crate::terminal::image::clear_in_range;
use crate::terminal::image::restore_images;
use crate::terminal::row::Row;

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
    pub charset_g0_is_drawing: bool,
    pub charset_g1_is_drawing: bool,
    pub charset_gl_is_g0: bool,
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
    /// Character set designated to G0. `true` = DEC Special Graphics, `false` =
    /// ASCII.
    pub charset_g0_is_drawing: bool,
    /// Character set designated to G1.
    pub charset_g1_is_drawing: bool,
    /// `true` when G0 is active (GL = G0, default). `false` when G1 is active
    /// (GL = G1, after SO).
    pub charset_gl_is_g0: bool,
    /// DECAWM (`?7`) — when true (default), printing past the right margin
    /// wraps to the next line. When false, the cursor stays at the right
    /// margin and overwrites the last column.
    pub autowrap: bool,
}

impl Screen {
    /// Whether the active GL character set is DEC Special Graphics.
    pub fn is_drawing_active(&self) -> bool {
        if self.charset_gl_is_g0 {
            self.charset_g0_is_drawing
        } else {
            self.charset_g1_is_drawing
        }
    }

    pub(super) fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        fg: Srgb<u8>,
        bg: Srgb<u8>,
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
            fg: default_fg(),
            bg: default_bg(),
            attrs: CellAttrs::default(),
            underline: UnderlineStyle::None,
            underline_color: None,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            offset: 0,
            images: BTreeMap::new(),
            saved_cursor: None,
            current_hyperlink: None,
            cursor_visible: true,
            last_char: None,
            tab_stops: init_tab_stops(cols),
            origin_mode: false,
            charset_g0_is_drawing: false,
            charset_g1_is_drawing: false,
            charset_gl_is_g0: true,
            autowrap: true,
        }
    }
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
        charset_g0_is_drawing: screen.charset_g0_is_drawing,
        charset_g1_is_drawing: screen.charset_g1_is_drawing,
        charset_gl_is_g0: screen.charset_gl_is_g0,
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
            screen.charset_g0_is_drawing = saved.charset_g0_is_drawing;
            screen.charset_g1_is_drawing = saved.charset_g1_is_drawing;
            screen.charset_gl_is_g0 = saved.charset_gl_is_g0;
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
}

/// Switch between the primary and alt screens. Idempotent: a no-op if the
/// target screen is already active.
fn switch_screen(
    target_alt: bool,
    active: &mut Screen,
    stash: &mut Screen,
    on_alt: &mut bool,
) {
    if *on_alt == target_alt {
        return;
    }
    std::mem::swap(active, stash);
    *on_alt = target_alt;
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
    viewport: &Viewport,
    on_alt: &mut bool,
) {
    match mode {
        // DECCOLM (mode 3) is handled in csi_dispatch where mutable
        // viewport access is available for the resize.
        6 => {
            active.origin_mode = enable;
            // Entering/leaving origin mode homes the cursor per DEC spec.
            active.cursor.row = if enable { active.scroll_top } else { 0 };
            active.cursor.col = 0;
        }
        7 => active.autowrap = enable,
        25 => active.cursor_visible = enable,
        47 => switch_screen(enable, active, stash, on_alt),
        1047 => {
            // xterm clears the alt buffer when leaving via 1047l so stale
            // content isn't re-shown the next time it's entered.
            if !enable && *on_alt {
                clear_visible(active, viewport);
            }
            switch_screen(enable, active, stash, on_alt);
        }
        1048 => {
            if enable {
                save_cursor_slot(active);
            } else {
                restore_cursor_slot(active, viewport);
            }
        }
        1049 => {
            if enable {
                // Save into primary's DECSC slot before swapping, so the
                // slot rides with primary into the stash and is there for
                // the round trip.
                if !*on_alt {
                    save_cursor_slot(active);
                }
                switch_screen(true, active, stash, on_alt);
                clear_visible(active, viewport);
            } else {
                if *on_alt {
                    clear_visible(active, viewport);
                }
                switch_screen(false, active, stash, on_alt);
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
    });
    screen.offset = screen.offset.min(scrollback);
}

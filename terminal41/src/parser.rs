use std::io::Write;
use std::sync::LazyLock;
use std::time::Instant;

use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use smol_str::SmolStr;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vtepp::Params;

use crate::TerminalModes;
use crate::color;
use crate::color::apply_sgr;
use crate::cursor::CursorStyle;
use crate::grid;
use crate::grid::Viewport;
use crate::keyboard::KittyKeyboardState;
use crate::keyboard::handle_kitty_keyboard;
use crate::mouse::MouseTracking;
use crate::mouse::apply_mouse_mode;
use crate::row::Row;
use crate::screen;
use crate::screen::Screen;

/// Bundles the bits of [`Terminal`](super::Terminal) state that CSI handlers
/// need beyond the active screen. Keeps the call signature stable as new CSI
/// sequences get wired in.
pub(super) struct CsiContext<'a> {
    pub screen: &'a mut Screen,
    pub stash: &'a mut Screen,
    pub viewport: &'a mut Viewport,
    pub on_alt_screen: &'a mut bool,
    pub modes: &'a mut TerminalModes,
    pub kitty_keyboard: &'a mut KittyKeyboardState,
    pub pending_output: &'a mut Vec<u8>,
    pub cursor_style: &'a mut CursorStyle,
    pub cell_width: u32,
    pub cell_height: u32,
    pub palette: &'a color::ColorPalette,
    pub title_stack: &'a mut Vec<Option<String>>,
    pub current_title: &'a mut Option<String>,
    pub saved_modes: &'a mut std::collections::HashMap<u16, bool>,
}

/// Bundles the bits of [`Terminal`](super::Terminal) state that ESC handlers
/// need beyond the active screen. RIS in particular resets nearly everything.
pub(super) struct EscContext<'a> {
    pub screen: &'a mut Screen,
    pub stash: &'a mut Screen,
    pub viewport: &'a Viewport,
    pub on_alt_screen: &'a mut bool,
    pub modes: &'a mut TerminalModes,
    pub kitty_keyboard: &'a mut KittyKeyboardState,
    pub cursor_style: &'a mut CursorStyle,
    pub current_title: &'a mut Option<String>,
    pub title_stack: &'a mut Vec<Option<String>>,
    pub saved_modes: &'a mut std::collections::HashMap<u16, bool>,
    pub current_prompt_row: &'a mut Option<u64>,
    pub bell_pending: &'a mut bool,
    pub palette: &'a color::ColorPalette,
    /// Bytes to write back to the PTY (e.g. VT52 identify response `ESC / Z`).
    pub pending_output: &'a mut Vec<u8>,
    /// State machine for VT52 `ESC Y Pr Pc`. Set to `AwaitingRow` when the
    /// `ESC Y` byte is dispatched; the subsequent bytes are consumed in
    /// [`Terminal::apply`] before any other dispatch occurs.
    pub vt52_cursor_addr: &'a mut crate::Vt52CursorAddr,
}

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

/// Sentinel for the second (and beyond) cell of a wide glyph. Distinct from
/// the default blank (`" "`) so neighbour cleanup can tell them apart.
fn continuation_cell() -> SmolStr {
    SmolStr::default()
}

fn blank_cell() -> SmolStr {
    SmolStr::new_inline(" ")
}

// C0 control bytes (ECMA-48 / ASCII).
const NUL: u8 = 0x00;
const BEL: u8 = 0x07;
const BS: u8 = 0x08;

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

/// SCS (Select Character Set) intermediate bytes that designate G0..G3.
const SCS_INTERMEDIATES: &[u8] = b"()*+";

/// Translate a byte in 0x60..=0x7E to a DEC Special Graphics Unicode character.
/// The mapping follows the VT100/VT220 standard box-drawing character set.
fn translate_drawing_char(byte: u8) -> &'static str {
    const TABLE: [&str; 31] = [
        "\u{25C6}", // 0x60 → ◆
        "\u{2592}", // 0x61 → ▒
        "\u{2409}", // 0x62 → ␉ (HT symbol)
        "\u{240C}", // 0x63 → ␌ (FF symbol)
        "\u{240D}", // 0x64 → ␍ (CR symbol)
        "\u{240A}", // 0x65 → ␊ (LF symbol)
        "\u{00B0}", // 0x66 → °
        "\u{00B1}", // 0x67 → ±
        "\u{2424}", // 0x68 → ␤ (NL symbol)
        "\u{240B}", // 0x69 → ␋ (VT symbol)
        "\u{2518}", // 0x6A → ┘
        "\u{2510}", // 0x6B → ┐
        "\u{250C}", // 0x6C → ┌
        "\u{2514}", // 0x6D → └
        "\u{253C}", // 0x6E → ┼
        "\u{23BA}", // 0x6F → ⎺ (scan line 1)
        "\u{23BB}", // 0x70 → ⎻ (scan line 3)
        "\u{2500}", // 0x71 → ─ (horizontal line)
        "\u{23BC}", // 0x72 → ⎼ (scan line 7)
        "\u{23BD}", // 0x73 → ⎽ (scan line 9)
        "\u{251C}", // 0x74 → ├
        "\u{2524}", // 0x75 → ┤
        "\u{2534}", // 0x76 → ┴
        "\u{252C}", // 0x77 → ┬
        "\u{2502}", // 0x78 → │ (vertical line)
        "\u{2264}", // 0x79 → ≤
        "\u{2265}", // 0x7A → ≥
        "\u{03C0}", // 0x7B → π
        "\u{2260}", // 0x7C → ≠
        "\u{00A3}", // 0x7D → £
        "\u{00B7}", // 0x7E → ·
    ];
    TABLE[(byte - 0x60) as usize]
}

/// Fast path for a batched run of printable ASCII bytes (0x20..=0x7E).
///
/// Skips the grapheme/width machinery `put_char` needs — every byte is
/// width-1 and can't fold into a neighbour. Breaks wide-anchor invariants at
/// only the run's two edges (interior cells are entirely overwritten, so any
/// anchors they held are destroyed outright).
pub(super) fn put_ascii_run(
    screen: &mut Screen,
    viewport: &Viewport,
    run: &[u8],
    insert_mode: bool,
) {
    if run.is_empty() {
        return;
    }

    // Single-shift (SS2/SS3): the first character uses G2 or G3 for one
    // character only. Consume it via put_char (which handles DEC drawing
    // translation), then continue with the rest of the run.
    let run = if let Some(ss) = screen.single_shift.take() {
        let b = run[0];
        let is_drawing = if ss == 2 {
            screen.charset_g2_is_drawing
        } else {
            screen.charset_g3_is_drawing
        };
        let ch = if is_drawing && (0x60..=0x7E).contains(&b) {
            SmolStr::new_inline(translate_drawing_char(b))
        } else {
            ASCII_CELLS[(b - 0x20) as usize].clone()
        };
        put_char(screen, viewport, ch, insert_mode);
        &run[1..]
    } else {
        run
    };
    if run.is_empty() {
        return;
    }

    // When DEC Special Graphics is active, bytes 0x60-0x7E need to be
    // translated to Unicode box-drawing characters. Fall back to a
    // per-character path that handles the translation.
    if screen.is_drawing_active() {
        for &b in run {
            let ch = if (0x60..=0x7E).contains(&b) {
                SmolStr::new_inline(translate_drawing_char(b))
            } else {
                ASCII_CELLS[(b - 0x20) as usize].clone()
            };
            put_char(screen, viewport, ch, insert_mode);
        }
        return;
    }

    let fg = screen.fg;
    let bg = screen.bg;
    let attrs = screen.attrs;
    let ul = screen.underline;
    let ul_color = screen.underline_color;
    let link = screen.current_hyperlink;

    // Record the last byte of the run for REP (CSI Ps b).
    let last_byte = *run.last().unwrap();
    screen.last_char = Some(ASCII_CELLS[(last_byte - 0x20) as usize].clone());

    let mut i = 0;
    while i < run.len() {
        // Pre-wrap: a cursor parked past the last column wraps before
        // writing when DECAWM is on. When off, clamp to the last column
        // so subsequent writes overwrite in place.
        if screen.cursor.col >= viewport.cols {
            if screen.autowrap {
                soft_wrap(screen, viewport);
            } else {
                screen.cursor.col = viewport.cols - 1;
            }
        }

        let r = screen.grid.active_row_index(&screen.cursor, viewport);
        let col = screen.cursor.col as usize;
        let remaining_cols = (viewport.cols - screen.cursor.col) as usize;
        let chunk_len = (run.len() - i).min(remaining_cols);

        // IRM: shift existing content right before overwriting.
        if insert_mode {
            screen
                .grid
                .insert_chars(&screen.cursor, viewport, chunk_len as u16);
        }

        // Break a wide anchor severed by the left edge of the chunk. The
        // right-edge case is covered by passing chunk_len to
        // break_wide_glyphs_around_write.
        let row = &mut screen.grid.rows[r];
        break_wide_glyphs_around_write(row, col, chunk_len);
        let chunk = &run[i..i + chunk_len];
        // Hoist the LazyLock deref so the inner loop sees a plain
        // `&[SmolStr; 95]`; the parser guarantees each byte is 0x20..=0x7E
        // so the bounds check on the table index is provably redundant.
        let table: &[SmolStr; 95] = &ASCII_CELLS;
        for (cell, &b) in row.cells[col..col + chunk_len].iter_mut().zip(chunk) {
            // SAFETY: parser emits PrintAscii only for bytes in 0x20..=0x7E,
            // so (b - 0x20) is in 0..95 and the table index is in range.
            *cell = unsafe { table.get_unchecked((b - 0x20) as usize) }.clone();
        }
        // Attributes are homogeneous across the run — let the compiler lower
        // each of these to a single memset-style fill.
        row.fg[col..col + chunk_len].fill(fg);
        row.bg[col..col + chunk_len].fill(bg);
        row.attrs[col..col + chunk_len].fill(attrs);
        row.underline[col..col + chunk_len].fill(ul);
        row.underline_color[col..col + chunk_len].fill(ul_color);
        row.links[col..col + chunk_len].fill(link);

        screen.cursor.col += chunk_len as u32;
        i += chunk_len;
    }
}

pub(super) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
    insert_mode: bool,
) {
    let raw_width = UnicodeWidthStr::width(s.as_str());

    // Fold only zero-width codepoints (combining marks, ZWJ, variation
    // selectors) into the prior anchor. Folding a *wide* codepoint into a
    // wide anchor would mean the host's wcswidth and our cursor disagree on
    // the cluster's width — e.g. `👨‍💻` is 4 cells per wcswidth (2+0+2) but
    // folding would advance our cursor by only 2, so every subsequent
    // redraw lands two columns off and backspace walks into the prompt.
    // Keeping each wide codepoint in its own cell range matches wcswidth;
    // the font shaper still sees the ZWJ sequence in `row_text` (empty
    // continuations contribute 0 bytes) and renders the ligature if the
    // font has one.
    if raw_width == 0 {
        try_extend_prev_cell(screen, viewport, &s);
        return;
    }

    // Single-shift was already consumed by put_ascii_run for ASCII input.
    // For non-ASCII graphic characters there is no drawing-table translation,
    // so just clear the pending shift so it doesn't linger.
    screen.single_shift = None;

    let width = raw_width.max(1);

    // Soft-wrap when the incoming cluster (possibly wide) would overhang the
    // right edge. When DECAWM is off, clamp instead of wrapping.
    if screen.cursor.col + width as u32 > viewport.cols {
        if screen.autowrap {
            soft_wrap(screen, viewport);
        } else {
            screen.cursor.col = viewport.cols.saturating_sub(width as u32);
        }
    }

    // IRM: shift existing content right before overwriting.
    if insert_mode {
        screen
            .grid
            .insert_chars(&screen.cursor, viewport, width as u16);
    }

    let fg = screen.fg;
    let bg = screen.bg;
    let attrs = screen.attrs;
    let ul = screen.underline;
    let ul_color = screen.underline_color;
    let link = screen.current_hyperlink;
    let r = screen.grid.active_row_index(&screen.cursor, viewport);
    let col = screen.cursor.col as usize;

    // Preserve the "a cell is a continuation iff its left neighbour is a wide
    // anchor" invariant by blanking any wide-anchor/continuation pair the new
    // write would sever. See design note: we only fix this at put_char, not
    // at clear/erase/reflow.
    break_wide_glyphs_around_write(&mut screen.grid.rows[r], col, width);

    screen.grid.rows[r].cells[col] = s.clone();
    screen.grid.rows[r].fg[col] = fg;
    screen.grid.rows[r].bg[col] = bg;
    screen.grid.rows[r].attrs[col] = attrs;
    screen.grid.rows[r].underline[col] = ul;
    screen.grid.rows[r].underline_color[col] = ul_color;
    screen.grid.rows[r].links[col] = link;
    for i in 1..width {
        screen.grid.rows[r].cells[col + i] = continuation_cell();
        screen.grid.rows[r].fg[col + i] = fg;
        screen.grid.rows[r].bg[col + i] = bg;
        screen.grid.rows[r].attrs[col + i] = attrs;
        screen.grid.rows[r].underline[col + i] = ul;
        screen.grid.rows[r].underline_color[col + i] = ul_color;
        screen.grid.rows[r].links[col + i] = link;
    }
    screen.last_char = Some(s);
    screen.cursor.col += width as u32;
}

/// True if the cell at `col` is the anchor of a wide glyph — it holds
/// non-blank text and its right neighbour is the empty continuation
/// sentinel we placed when laying out the wide glyph. Consulting the grid
/// state is more robust than re-measuring the cell text: `unicode-width`
/// disagrees with glibc `wcswidth` on VS16-upgraded emoji (e.g. `❤️`, which
/// `unicode-width` reports as width 2 but `wcswidth` reports as 1), and we
/// keep such clusters single-cell to stay in sync with the shell's cursor
/// tracking. Checking neighbour emptiness reflects the physical invariant.
fn is_wide_anchor_at(
    row: &Row,
    col: usize,
) -> bool {
    let Some(anchor) = row.cells.get(col) else {
        return false;
    };
    let Some(right) = row.cells.get(col + 1) else {
        return false;
    };
    let anchor_str = anchor.as_str();
    !anchor_str.is_empty() && anchor_str != " " && right.as_str().is_empty()
}

/// Keep the wide-anchor/continuation invariant intact across an overwrite.
/// Left edge: if the cell to our left was a wide anchor, our write lands on
/// its continuation, so blank the orphaned anchor. Right edge: if the last
/// cell we're writing *is* a wide anchor, its continuation (at `col + width`)
/// won't be touched by the write and would dangle, so blank it.
fn break_wide_glyphs_around_write(
    row: &mut Row,
    col: usize,
    width: usize,
) {
    if col > 0 && is_wide_anchor_at(row, col - 1) {
        row.cells[col - 1] = blank_cell();
    }
    let last = col + width - 1;
    if is_wide_anchor_at(row, last) {
        let cont = last + 1;
        if cont < row.cells.len() {
            row.cells[cont] = blank_cell();
        }
    }
}

fn soft_wrap(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    screen.cursor.col = 0;
    let r = screen.grid.active_row_index(&screen.cursor, viewport);
    screen.grid.rows[r].wrapped = true;
    if screen.cursor.row == screen.scroll_bottom {
        if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
            screen.grid.push_visible_row(viewport);
        } else {
            screen.grid.scroll_up_in_region(
                viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                1,
            );
        }
    } else if screen.cursor.row < viewport.rows - 1 {
        screen.cursor.row += 1;
    }
}

/// Apply a private mode set/reset from the XTRESTORE path. Mirrors the
/// logic in the `CSI ? h`/`l` handler: terminal-level modes are handled
/// inline, screen/alt-screen modes delegate to `set_private_mode` and
/// `apply_mouse_mode`.
fn apply_private_mode(
    mode: u16,
    enable: bool,
    ctx: &mut CsiContext<'_>,
) {
    if mode == 2 {
        // DECANM — ANSI/VT52 mode. `h` (enable) = ANSI mode; `l` (disable) =
        // VT52 compatibility mode. The sense is inverted: the mode *being set*
        // means ANSI is active, so VT52 is off.
        ctx.modes.vt52_mode = !enable;
    } else if mode == 2004 {
        ctx.modes.bracketed_paste = enable;
    } else if mode == 1004 {
        ctx.modes.focus_reporting = enable;
    } else if mode == 2026 {
        ctx.modes.synchronized_update_since = enable.then(Instant::now);
    } else if mode == 3 {
        // DECCOLM restore is tricky (resizes the grid). Skip for save/restore —
        // xterm itself ignores mode 3 in XTSAVE/XTRESTORE.
    } else if !apply_mouse_mode(
        mode,
        enable,
        &mut ctx.modes.mouse_tracking,
        &mut ctx.modes.mouse_encoding,
    ) {
        screen::set_private_mode(
            mode,
            enable,
            ctx.screen,
            ctx.stash,
            ctx.viewport,
            ctx.on_alt_screen,
        );
    }
}

/// Map a private-mode number to its DECRQM response value:
/// 1 = set, 2 = reset, 0 = not recognized. Queries every private mode
/// we track so apps can probe capabilities without side effects.
fn query_private_mode(
    ps: u16,
    ctx: &CsiContext<'_>,
) -> u8 {
    match ps {
        // DECANM — ANSI/VT52 mode. Set (1) means ANSI mode is active.
        2 => {
            if !ctx.modes.vt52_mode {
                1
            } else {
                2
            }
        }
        // DECCKM — application cursor keys.
        1 => {
            if ctx.screen.app_cursor_keys {
                1
            } else {
                2
            }
        }
        // DECOM — origin mode.
        6 => {
            if ctx.screen.origin_mode {
                1
            } else {
                2
            }
        }
        // DECAWM — auto-wrap mode.
        7 => {
            if ctx.screen.autowrap {
                1
            } else {
                2
            }
        }
        // DECTCEM — cursor visible.
        25 => {
            if ctx.screen.cursor_visible {
                1
            } else {
                2
            }
        }
        // Alt-screen family.
        47 | 1047 | 1049 => {
            if *ctx.on_alt_screen {
                1
            } else {
                2
            }
        }
        // Mouse tracking modes. Report "set" if that specific mode is active.
        9 => match_tracking(ctx.modes.mouse_tracking, MouseTracking::X10),
        1000 => match_tracking(ctx.modes.mouse_tracking, MouseTracking::Normal),
        1002 => match_tracking(ctx.modes.mouse_tracking, MouseTracking::ButtonEvent),
        1003 => match_tracking(ctx.modes.mouse_tracking, MouseTracking::AnyEvent),
        // Focus reporting.
        1004 => {
            if ctx.modes.focus_reporting {
                1
            } else {
                2
            }
        }
        // DECSC/DECRC cursor save (we always support it; "set" = saved).
        1048 => {
            if ctx.screen.saved_cursor.is_some() {
                1
            } else {
                2
            }
        }
        // Bracketed paste.
        2004 => {
            if ctx.modes.bracketed_paste {
                1
            } else {
                2
            }
        }
        // Synchronized update.
        2026 => {
            if ctx.modes.synchronized_update_since.is_some() {
                1
            } else {
                2
            }
        }
        _ => 0,
    }
}

fn match_tracking(
    current: MouseTracking,
    target: MouseTracking,
) -> u8 {
    if current == target { 1 } else { 2 }
}

/// If appending `s` to the previously-written cell keeps it a single grapheme
/// cluster, do so and return `true`. Walks past continuation cells so a
/// combining mark or ZWJ piece folds into the wide anchor it visually
/// decorates, not the empty continuation sitting between them.
fn try_extend_prev_cell(
    screen: &mut Screen,
    viewport: &Viewport,
    s: &str,
) {
    let (prev_row, mut prev_col) = if screen.cursor.col > 0 && screen.cursor.col <= viewport.cols {
        let row = screen.grid.active_row_index(&screen.cursor, viewport);
        (row, (screen.cursor.col - 1) as usize)
    } else if screen.cursor.col == 0 {
        let row = screen.grid.active_row_index(&screen.cursor, viewport);
        if row == 0 || !screen.grid.rows[row].wrapped {
            return;
        }
        let prev_row = row - 1;
        let last_col = screen.grid.rows[prev_row].cells.len().saturating_sub(1);
        (prev_row, last_col)
    } else {
        return;
    };

    // Skip wide-glyph continuation cells to reach the anchor.
    while prev_col > 0 && screen.grid.rows[prev_row].cells[prev_col].is_empty() {
        prev_col -= 1;
    }

    let prev = &screen.grid.rows[prev_row].cells[prev_col];
    if prev.as_str() == " " || prev.is_empty() {
        return;
    }

    // Fold without widening the cell. VS16 etc. can bump `unicode-width` on
    // the combined string (e.g. `❤` + `VS16` → 2), but glibc `wcswidth` —
    // which the host shell uses to track cursor columns — still reports 1.
    // Matching wcswidth keeps backspace/cursor-movement in sync with
    // readline; `is_wide_anchor_at` looks at the grid state (continuation
    // cell to the right) rather than re-measuring this text, so the next
    // write won't misidentify the cell as a wide anchor and blank it.
    let mut combined = String::with_capacity(prev.len() + s.len());
    combined.push_str(prev);
    combined.push_str(s);
    if combined.graphemes(true).count() != 1 {
        return;
    }

    screen.grid.rows[prev_row].cells[prev_col] = SmolStr::new(&combined);
}

pub(super) fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    // Cancel pending wrap for control characters that affect cursor
    // position. Without this, a BS/TAB/CR/LF after writing the last
    // column would see cursor.col == viewport.cols (one past the edge).
    if screen.cursor.col >= viewport.cols {
        screen.cursor.col = viewport.cols - 1;
    }

    match byte {
        // LF, VT, FF all perform the same index-down operation. VT and FF
        // are defined as equivalent to LF by ECMA-48; vttest's "control
        // characters inside ESC sequences" test relies on VT working.
        b'\n' | 0x0B | 0x0C => {
            // LNM (mode 20): when enabled, LF/VT/FF imply CR.
            if newline_mode {
                screen.cursor.col = 0;
            }
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    screen.grid.scroll_up_in_region(
                        viewport,
                        &mut screen.images,
                        screen.scroll_top,
                        screen.scroll_bottom,
                        1,
                    );
                }
            } else if screen.cursor.row < viewport.rows - 1 {
                screen.cursor.row += 1;
            }
        }
        b'\r' => {
            screen.cursor.col = 0;
        }
        BS => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        b'\t' => {
            screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, viewport.cols);
        }
        0x0E => {
            // SO — Shift Out: invoke G1 into GL.
            screen.charset_gl_is_g0 = false;
        }
        0x0F => {
            // SI — Shift In: invoke G0 into GL (default).
            screen.charset_gl_is_g0 = true;
        }
        BEL => {
            *bell_pending = true;
        }
        NUL => {}
        _ => {}
    }
}

pub(super) fn csi_dispatch(
    ctx: &mut CsiContext<'_>,
    params: &Params,
    intermediates: &[u8],
    action: char,
) {
    // Cancel the pending-wrap state. After writing the last column,
    // cursor.col sits at viewport.cols (one past the right edge). Any CSI
    // sequence — cursor movement, erase, DSR report, even SGR — cancels
    // this state so the cursor reports and behaves as if on the last column.
    if ctx.screen.cursor.col >= ctx.viewport.cols {
        ctx.screen.cursor.col = ctx.viewport.cols - 1;
    }

    // -- Sequences that carry intermediates ----------------------------------

    if intermediates == b"?" && matches!(action, 'h' | 'l') {
        let enable = action == 'h';
        for p in params.iter() {
            if p[0] == 2 {
                // DECANM — ANSI/VT52 mode. `h` = ANSI (vt52_mode off); `l` = VT52.
                ctx.modes.vt52_mode = !enable;
            } else if p[0] == 2004 {
                ctx.modes.bracketed_paste = enable;
            } else if p[0] == 1004 {
                ctx.modes.focus_reporting = enable;
            } else if p[0] == 2026 {
                // BSU refreshes the deadline; ESU clears it. Refreshing on a
                // nested BSU matches the contour spec's "keep the window open"
                // rule for apps that chain updates.
                ctx.modes.synchronized_update_since = enable.then(Instant::now);
            } else if p[0] == 3 {
                // DECCOLM — 80/132 column mode. Resize the grid, clear
                // the screen, reset margins, and home the cursor per DEC
                // spec. This lets vttest's 132-column pass work.
                let new_cols = if enable {
                    ctx.modes.deccolm_saved_cols = Some(ctx.viewport.cols);
                    132
                } else {
                    ctx.modes
                        .deccolm_saved_cols
                        .take()
                        .unwrap_or(ctx.viewport.cols)
                };
                let old_cols = ctx.viewport.cols;
                let rows = ctx.viewport.rows;
                for s in [&mut *ctx.screen, &mut *ctx.stash] {
                    screen::resize_screen(s, old_cols, rows, new_cols, rows);
                }
                ctx.viewport.cols = new_cols;
                screen::clear_visible(ctx.screen, ctx.viewport);
                ctx.screen.scroll_top = 0;
                ctx.screen.scroll_bottom = rows.saturating_sub(1);
                ctx.screen.cursor = grid::Cursor::default();
            } else if !apply_mouse_mode(
                p[0],
                enable,
                &mut ctx.modes.mouse_tracking,
                &mut ctx.modes.mouse_encoding,
            ) {
                screen::set_private_mode(
                    p[0],
                    enable,
                    ctx.screen,
                    ctx.stash,
                    ctx.viewport,
                    ctx.on_alt_screen,
                );
            }
        }
        return;
    }

    // XTSAVE — save individual private mode values (CSI ? Ps s).
    if intermediates == b"?" && action == 's' {
        for p in params.iter() {
            let mode = p[0];
            let state = query_private_mode(mode, ctx);
            ctx.saved_modes.insert(mode, state == 1);
        }
        return;
    }

    // XTRESTORE — restore individual private mode values (CSI ? Ps r).
    if intermediates == b"?" && action == 'r' {
        for p in params.iter() {
            let mode = p[0];
            if let Some(&saved) = ctx.saved_modes.get(&mode) {
                apply_private_mode(mode, saved, ctx);
            }
        }
        return;
    }

    if action == 'u' && matches!(intermediates, b">" | b"<" | b"=" | b"?") {
        handle_kitty_keyboard(
            intermediates[0],
            params,
            ctx.kitty_keyboard,
            ctx.pending_output,
        );
        return;
    }

    if action == 'q' && intermediates == b" " {
        // DECSCUSR. The space intermediate is mandatory; the single param
        // picks shape+blink (0/1=blink block, 2=block, 3/4=underline, 5/6=beam).
        let ps = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(0);
        ctx.cursor_style.apply_decscusr(ps);
        return;
    }

    if action == 'q' && intermediates == b">" {
        // XTVERSION (xterm name/version query). Apps use the reply to gate
        // behavior on known-good terminals.
        write!(
            ctx.pending_output,
            "\x1bP>|term41 {}\x1b\\",
            env!("CARGO_PKG_VERSION"),
        )
        .expect("write to Vec is infallible");
        return;
    }

    // DECRQM — Request Mode (CSI ? Ps $ p for private, CSI Ps $ p for ANSI).
    // Apps query terminal capabilities by checking whether specific modes are
    // set or reset. Reply: CSI [?] Ps ; Pm $ y where Pm = 1 (set), 2 (reset),
    // or 0 (not recognized).
    if action == 'p' && (intermediates == b"?$" || intermediates == b"$") {
        let ps = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(0);
        let private = intermediates == b"?$";
        let pm = if private {
            query_private_mode(ps, ctx)
        } else {
            match ps {
                4 => {
                    if ctx.modes.insert_mode {
                        1
                    } else {
                        2
                    }
                }
                _ => 0,
            }
        };
        if private {
            write!(ctx.pending_output, "\x1b[?{ps};{pm}$y").expect("write to Vec is infallible");
        } else {
            write!(ctx.pending_output, "\x1b[{ps};{pm}$y").expect("write to Vec is infallible");
        }
        return;
    }

    if action == 'c' && intermediates == b">" {
        // DA2 (Secondary Device Attributes).
        ctx.pending_output.extend_from_slice(b"\x1b[>41;0;0c");
        return;
    }

    if action == 'p' && intermediates == b"!" {
        // DECSTR (Soft Terminal Reset). Resets modes, colors, attributes,
        // scroll region, and cursor style back to defaults without clearing
        // the screen or scrollback. tmux and neovim use this for a
        // lightweight cleanup between sessions.
        let screen = &mut *ctx.screen;
        screen.fg = ctx.palette.fg;
        screen.bg = ctx.palette.bg;
        screen.attrs = CellAttrs::default();
        screen.underline = UnderlineStyle::None;
        screen.underline_color = None;
        screen.scroll_top = 0;
        screen.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
        screen.saved_cursor = None;
        screen.current_hyperlink = None;
        screen.cursor_visible = true;
        screen.last_char = None;
        screen.tab_stops = screen::init_tab_stops(ctx.viewport.cols);
        screen.origin_mode = false;
        screen.autowrap = true;
        screen.app_cursor_keys = false;
        screen.charset_g0_is_drawing = false;
        screen.charset_g1_is_drawing = false;
        screen.charset_g2_is_drawing = false;
        screen.charset_g3_is_drawing = false;
        screen.charset_gl_is_g0 = true;
        screen.single_shift = None;
        // DECSTR resets all terminal-level modes including DECANM
        // (vt52_mode), returning the terminal to ANSI mode.
        *ctx.modes = TerminalModes::new();
        *ctx.kitty_keyboard = KittyKeyboardState::new();
        *ctx.cursor_style = CursorStyle::default();
        return;
    }

    // DA3 (Tertiary Device Attributes, CSI = c).
    if action == 'c' && intermediates == b"=" {
        ctx.pending_output
            .extend_from_slice(b"\x1bP!|000000000\x1b\\");
        return;
    }

    // DECXCPR — Extended Cursor Position Report (CSI ? 6 n). Reports the
    // cursor row, column, and page (always 1) as CSI ? row ; col ; 1 R.
    if action == 'n' && intermediates == b"?" {
        let ps = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(0);
        if ps == 6 {
            let row = ctx.screen.cursor.row + 1;
            let col = ctx.screen.cursor.col + 1;
            write!(ctx.pending_output, "\x1b[?{row};{col};1R").expect("write to Vec is infallible");
        }
        return;
    }

    // SL — Scroll Left (CSI Ps SP @). Shifts every row in the scroll region
    // left by Ps columns; vacated columns on the right are cleared.
    if action == '@' && intermediates == b" " {
        let n = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(1)
            .max(1) as u32;
        ctx.screen.grid.scroll_left(
            ctx.viewport,
            ctx.screen.scroll_top,
            ctx.screen.scroll_bottom,
            n,
        );
        return;
    }

    // SR — Scroll Right (CSI Ps SP A). Shifts every row in the scroll region
    // right by Ps columns; vacated columns on the left are cleared.
    if action == 'A' && intermediates == b" " {
        let n = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(1)
            .max(1) as u32;
        ctx.screen.grid.scroll_right(
            ctx.viewport,
            ctx.screen.scroll_top,
            ctx.screen.scroll_bottom,
            n,
        );
        return;
    }

    // DECIC — Insert Column (CSI Ps ' }). Inserts Ps blank columns at the
    // cursor column in every row of the scroll region.
    if action == '}' && intermediates == b"'" {
        let n = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(1)
            .max(1) as u32;
        ctx.screen.grid.insert_cols(
            ctx.viewport,
            ctx.screen.cursor.col,
            ctx.screen.scroll_top,
            ctx.screen.scroll_bottom,
            n,
        );
        return;
    }

    // DECDC — Delete Column (CSI Ps ' ~). Deletes Ps columns at the cursor
    // column in every row of the scroll region.
    if action == '~' && intermediates == b"'" {
        let n = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(1)
            .max(1) as u32;
        ctx.screen.grid.delete_cols(
            ctx.viewport,
            ctx.screen.cursor.col,
            ctx.screen.scroll_top,
            ctx.screen.scroll_bottom,
            n,
        );
        return;
    }

    // DEC rectangular-area operations (CSI $ <action>).
    // Params are 1-based; converted to 0-based before calling grid methods.
    if intermediates == b"$" {
        let rows = ctx.viewport.rows;
        let cols = ctx.viewport.cols;
        let p: Vec<u16> = params.iter().map(|p| p[0]).collect();

        // Clamp and convert 1-based DEC rect params to 0-based.
        let rect_top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
        let rect_left = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
        let rect_bottom = (p.get(2).copied().unwrap_or(rows as u16).max(1) as u32 - 1)
            .min(rows.saturating_sub(1));
        let rect_right = (p.get(3).copied().unwrap_or(cols as u16).max(1) as u32 - 1)
            .min(cols.saturating_sub(1));

        // Ignore empty or inverted rects.
        if rect_top > rect_bottom || rect_left > rect_right {
            return;
        }

        match action {
            // DECERA — Erase Rectangular Area. Fills with spaces, default colors.
            'z' => {
                ctx.screen.grid.erase_rect(
                    ctx.viewport,
                    rect_top,
                    rect_left,
                    rect_bottom,
                    rect_right,
                );
            }
            // DECFRA — Fill Rectangular Area with character. Uses current SGR.
            'x' => {
                let ch_code = p.get(4).copied().unwrap_or(0x20) as u32;
                // Only code points 32–126 and 160–255 are valid fill chars.
                let valid = (32..=126).contains(&ch_code) || (160..=255).contains(&ch_code);
                if valid {
                    if let Some(ch) = char::from_u32(ch_code) {
                        let mut buf = [0u8; 4];
                        let s = SmolStr::new(ch.encode_utf8(&mut buf) as &str);
                        ctx.screen.grid.fill_rect(
                            ctx.viewport,
                            rect_top,
                            rect_left,
                            rect_bottom,
                            rect_right,
                            s,
                            ctx.screen.fg,
                            ctx.screen.bg,
                            ctx.screen.attrs,
                            ctx.screen.underline,
                            ctx.screen.underline_color,
                        );
                    }
                }
            }
            // DECCRA — Copy Rectangular Area. Source/dest pages ignored (always 1).
            // Params: src_top, src_left, src_bottom, src_right, _src_page,
            //         dst_top, dst_left [, _dst_page].
            'v' => {
                let dst_top = p.get(5).copied().unwrap_or(1).max(1) as u32 - 1;
                let dst_left = p.get(6).copied().unwrap_or(1).max(1) as u32 - 1;
                ctx.screen.grid.copy_rect(
                    ctx.viewport,
                    rect_top,
                    rect_left,
                    rect_bottom,
                    rect_right,
                    dst_top,
                    dst_left,
                );
            }
            // DECRARA — Reverse Attributes in Rectangular Area.
            // Params: top, left, bottom, right, [SGR attrs...]
            'r' => {
                let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                ctx.screen.grid.reverse_attrs_rect(
                    ctx.viewport,
                    rect_top,
                    rect_left,
                    rect_bottom,
                    rect_right,
                    &sgr,
                );
            }
            // DECCARA — Change Attributes in Rectangular Area.
            // Params: top, left, bottom, right, [SGR attrs...]
            't' => {
                let sgr: Vec<u16> = p.get(4..).unwrap_or(&[]).to_vec();
                ctx.screen.grid.change_attrs_rect(
                    ctx.viewport,
                    rect_top,
                    rect_left,
                    rect_bottom,
                    rect_right,
                    &sgr,
                );
            }
            _ => {}
        }
        return;
    }

    // -- No-intermediates sequences -----------------------------------------

    if !intermediates.is_empty() {
        return;
    }

    // DA1 needs pending_output, which lives on ctx rather than on the screen.
    // Handle it before borrowing ctx.screen for the screen-only match below.
    if action == 'c' {
        // DA1 (Primary Device Attributes). Reply as a VT220 (62) with
        // ANSI color (22) and ANSI text locator (29) attributes.
        ctx.pending_output.extend_from_slice(b"\x1b[?62;22;29c");
        return;
    }

    // DSR — Device Status Report. `CSI 5 n` checks that the terminal is alive;
    // `CSI 6 n` asks for the cursor position. Image viewers (viu, chafa) send
    // `CSI 6 n` after rendering and block on stdin waiting for the reply.
    if action == 'n' {
        let ps = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(0);
        match ps {
            5 => {
                ctx.pending_output.extend_from_slice(b"\x1b[0n");
            }
            6 => {
                // Report is 1-based.
                let row = ctx.screen.cursor.row + 1;
                let col = ctx.screen.cursor.col + 1;
                write!(ctx.pending_output, "\x1b[{row};{col}R")
                    .expect("write to Vec is infallible");
            }
            _ => {}
        }
        return;
    }

    // CSI Ps t — window manipulation + size queries (xterm). Image viewers
    // like viu and chafa send the pixel-size reports (14/16) before
    // transmitting and block on stdin until they arrive.
    if action == 't' {
        let ps = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(0);
        match ps {
            // Title stack push. Second param (0 or 2) selects icon vs
            // window title; we only track one title so both are equivalent.
            22 => {
                if ctx.title_stack.len() < 16 {
                    ctx.title_stack.push(ctx.current_title.clone());
                }
                return;
            }
            // Title stack pop.
            23 => {
                if let Some(title) = ctx.title_stack.pop() {
                    *ctx.current_title = title;
                }
                return;
            }
            14 => {
                // Report window size in pixels: CSI 4 ; height ; width t.
                let h = ctx.viewport.rows * ctx.cell_height;
                let w = ctx.viewport.cols * ctx.cell_width;
                write!(ctx.pending_output, "\x1b[4;{h};{w}t").expect("write to Vec is infallible");
            }
            16 => {
                // Report cell size in pixels: CSI 6 ; height ; width t.
                write!(
                    ctx.pending_output,
                    "\x1b[6;{};{}t",
                    ctx.cell_height, ctx.cell_width
                )
                .expect("write to Vec is infallible");
            }
            18 => {
                // Report terminal size in cells: CSI 8 ; rows ; cols t.
                write!(
                    ctx.pending_output,
                    "\x1b[8;{};{}t",
                    ctx.viewport.rows, ctx.viewport.cols
                )
                .expect("write to Vec is infallible");
            }
            _ => {}
        }
        return;
    }

    // REP (Repeat preceding graphic character). Handled before the main
    // match because `put_char` needs `&mut Screen` which conflicts with
    // the `cursor` borrow below.
    if action == 'b' {
        let n = params
            .iter()
            .next()
            .and_then(|g| g.first().copied())
            .unwrap_or(1)
            .max(1);
        if let Some(ch) = ctx.screen.last_char.clone() {
            let insert = ctx.modes.insert_mode;
            for _ in 0..n {
                put_char(ctx.screen, ctx.viewport, ch.clone(), insert);
            }
        }
        return;
    }

    let screen = &mut *ctx.screen;
    let viewport = &*ctx.viewport;
    let p: Vec<u16> = params.iter().map(|p| p[0]).collect();
    let cursor = &mut screen.cursor;

    match action {
        'A' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let top = if screen.origin_mode {
                screen.scroll_top
            } else {
                0
            };
            cursor.row = cursor.row.saturating_sub(n).max(top);
        }
        'B' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let bottom = if screen.origin_mode {
                screen.scroll_bottom
            } else {
                viewport.rows - 1
            };
            cursor.row = (cursor.row + n).min(bottom);
        }
        'C' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = (cursor.col + n).min(viewport.cols - 1);
        }
        'D' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = cursor.col.saturating_sub(n);
        }
        // CNL — Cursor Next Line. Move down Ps lines and to column 1.
        'E' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = (cursor.row + n).min(viewport.rows - 1);
            cursor.col = 0;
        }
        // CPL — Cursor Previous Line. Move up Ps lines and to column 1.
        'F' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = cursor.row.saturating_sub(n);
            cursor.col = 0;
        }
        'H' | 'f' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
            if screen.origin_mode {
                cursor.row = (screen.scroll_top + row).min(screen.scroll_bottom);
                cursor.col = col.min(viewport.cols - 1);
            } else {
                cursor.row = row.min(viewport.rows - 1);
                cursor.col = col.min(viewport.cols - 1);
            }
        }
        'J' => {
            let mode = p.first().copied().unwrap_or(0);
            screen
                .grid
                .erase_in_display(&screen.cursor, viewport, &mut screen.images, mode);
        }
        'K' => {
            let mode = p.first().copied().unwrap_or(0);
            screen.grid.erase_in_line(&screen.cursor, viewport, mode);
        }
        'm' => apply_sgr(
            &mut screen.fg,
            &mut screen.bg,
            &mut screen.attrs,
            &mut screen.underline,
            &mut screen.underline_color,
            params,
            ctx.palette,
        ),
        'd' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            if screen.origin_mode {
                cursor.row = (screen.scroll_top + row).min(screen.scroll_bottom);
            } else {
                cursor.row = row.min(viewport.rows - 1);
            }
        }
        // CHA — Cursor Horizontal Absolute. HPA (`) is an alias.
        'G' | '`' => {
            let col = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.col = col.min(viewport.cols - 1);
        }
        // HPR — Horizontal Position Relative. Alias for CUF (C).
        'a' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = (cursor.col + n).min(viewport.cols - 1);
        }
        // VPR — Vertical Position Relative. Alias for CUD (B).
        'e' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            let bottom = if screen.origin_mode {
                screen.scroll_bottom
            } else {
                viewport.rows - 1
            };
            cursor.row = (cursor.row + n).min(bottom);
        }
        'L' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= screen.scroll_top && cursor.row <= screen.scroll_bottom {
                let top = cursor.row;
                screen.grid.scroll_down_in_region(
                    viewport,
                    &mut screen.images,
                    top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'M' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= screen.scroll_top && cursor.row <= screen.scroll_bottom {
                let top = cursor.row;
                screen.grid.scroll_up_in_region(
                    viewport,
                    &mut screen.images,
                    top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'P' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            screen.grid.delete_chars(&screen.cursor, viewport, n);
        }
        '@' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            screen.grid.insert_chars(&screen.cursor, viewport, n);
        }
        'X' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            screen.grid.erase_chars(&screen.cursor, viewport, n);
        }
        'S' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                for _ in 0..n {
                    screen.grid.push_visible_row(viewport);
                }
            } else {
                screen.grid.scroll_up_in_region(
                    viewport,
                    &mut screen.images,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'T' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen.grid.scroll_down_in_region(
                viewport,
                &mut screen.images,
                screen.scroll_top,
                screen.scroll_bottom,
                n,
            );
        }
        'r' => {
            let top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let bottom = p.get(1).copied().unwrap_or(viewport.rows as u16).max(1) as u32 - 1;
            screen.scroll_top = top.min(viewport.rows - 1);
            screen.scroll_bottom = bottom.min(viewport.rows - 1).max(screen.scroll_top);
            // Home cursor. In origin mode, home means the scroll region
            // top; in absolute mode, home means row 0.
            screen.cursor.row = if screen.origin_mode {
                screen.scroll_top
            } else {
                0
            };
            screen.cursor.col = 0;
        }
        's' => {
            // SCOSC — save cursor position. Shares the DECSC slot, so an
            // app that mixes `CSI s` with `ESC 7` reads back whichever
            // write came last. Scripts that overlay a live-updating region
            // (progress bars, sixel plots) rely on this plus SCORC to
            // anchor their output; without it every repaint stacks below
            // the previous one.
            screen::save_cursor_slot(screen);
        }
        'u' => {
            // SCORC — restore cursor position. The kitty keyboard `CSI u`
            // variants all carry an intermediate (`>`, `<`, `=`, `?`) and
            // are caught above; a plain no-intermediate `CSI u` is
            // unambiguously the restore form.
            screen::restore_cursor_slot(screen, viewport);
        }
        // CHT — Cursor Forward Tabulation. Advance Ps tab stops.
        'I' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            for _ in 0..n {
                cursor.col = next_tab_stop(&screen.tab_stops, cursor.col, viewport.cols);
            }
        }
        // CBT — Cursor Backward Tabulation. Move back Ps tab stops.
        'Z' => {
            let n = p.first().copied().unwrap_or(1).max(1);
            for _ in 0..n {
                cursor.col = prev_tab_stop(&screen.tab_stops, cursor.col);
            }
        }
        // TBC — Tab Clear. Ps=0: clear at cursor. Ps=3: clear all.
        'g' => {
            let ps = p.first().copied().unwrap_or(0);
            match ps {
                0 => {
                    let col = cursor.col as usize;
                    if col < screen.tab_stops.len() {
                        screen.tab_stops[col] = false;
                    }
                }
                3 => screen.tab_stops.fill(false),
                _ => {}
            }
        }
        'h' | 'l' => {
            // ANSI (non-private) mode set/reset. Private modes (with `?`
            // intermediate) are handled above.
            let enable = action == 'h';
            for &mode in &p {
                match mode {
                    4 => ctx.modes.insert_mode = enable,
                    20 => ctx.modes.newline_mode = enable,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

pub(super) fn esc_dispatch(
    ctx: &mut EscContext<'_>,
    intermediates: &[u8],
    byte: u8,
) {
    // VT52 mode — completely different ESC vocabulary, no CSI or parameters.
    // The `/` intermediate (ESC / Z identify response) shares the intermediate
    // byte space with ANSI SCS, so we must gate on vt52_mode *first*.
    if ctx.modes.vt52_mode && intermediates.is_empty() {
        // Cancel pending wrap before any cursor-moving sequence.
        if ctx.screen.cursor.col >= ctx.viewport.cols {
            ctx.screen.cursor.col = ctx.viewport.cols.saturating_sub(1);
        }
        match byte {
            // ESC A — cursor up (no scroll).
            b'A' => {
                ctx.screen.cursor.row = ctx.screen.cursor.row.saturating_sub(1);
            }
            // ESC B — cursor down (no scroll).
            b'B' => {
                if ctx.screen.cursor.row + 1 < ctx.viewport.rows {
                    ctx.screen.cursor.row += 1;
                }
            }
            // ESC C — cursor right (no scroll).
            b'C' => {
                if ctx.screen.cursor.col + 1 < ctx.viewport.cols {
                    ctx.screen.cursor.col += 1;
                }
            }
            // ESC D — cursor left (no scroll).
            b'D' => {
                ctx.screen.cursor.col = ctx.screen.cursor.col.saturating_sub(1);
            }
            // ESC F — enter DEC Special Graphics mode (same as SCS G0 = 0).
            b'F' => {
                ctx.screen.charset_g0_is_drawing = true;
            }
            // ESC G — exit DEC Special Graphics mode (same as SCS G0 = B).
            b'G' => {
                ctx.screen.charset_g0_is_drawing = false;
            }
            // ESC H — cursor home (0, 0).
            b'H' => {
                ctx.screen.cursor.row = 0;
                ctx.screen.cursor.col = 0;
            }
            // ESC I — reverse index (identical to ANSI RI / ESC M): scroll
            // down if at the top of the scroll region, else cursor up.
            b'I' => {
                if ctx.screen.cursor.row == ctx.screen.scroll_top {
                    ctx.screen.grid.scroll_down_in_region(
                        ctx.viewport,
                        &mut ctx.screen.images,
                        ctx.screen.scroll_top,
                        ctx.screen.scroll_bottom,
                        1,
                    );
                } else if ctx.screen.cursor.row > 0 {
                    ctx.screen.cursor.row -= 1;
                }
            }
            // ESC J — erase to end of screen (same as ANSI ED 0).
            b'J' => {
                ctx.screen.grid.erase_in_display(
                    &ctx.screen.cursor,
                    ctx.viewport,
                    &mut ctx.screen.images,
                    0,
                );
            }
            // ESC K — erase to end of line (same as ANSI EL 0).
            b'K' => {
                ctx.screen
                    .grid
                    .erase_in_line(&ctx.screen.cursor, ctx.viewport, 0);
            }
            // ESC Y — direct cursor address. The two parameter bytes are
            // absorbed by Terminal::apply via Vt52CursorAddr state.
            b'Y' => {
                *ctx.vt52_cursor_addr = crate::Vt52CursorAddr::AwaitingRow;
            }
            // ESC Z — identify. VT52 responds ESC / Z (0x1b 0x2f 0x5a).
            b'Z' => {
                ctx.pending_output.extend_from_slice(b"\x1b/Z");
            }
            // ESC < — exit VT52 mode, return to ANSI mode (sets DECANM).
            b'<' => {
                ctx.modes.vt52_mode = false;
            }
            _ => {}
        }
        return;
    }

    if let Some(&inter) = intermediates.first()
        && SCS_INTERMEDIATES.contains(&inter)
    {
        let is_drawing = byte == b'0';
        match inter {
            b'(' => ctx.screen.charset_g0_is_drawing = is_drawing,
            b')' => ctx.screen.charset_g1_is_drawing = is_drawing,
            b'*' => ctx.screen.charset_g2_is_drawing = is_drawing,
            b'+' => ctx.screen.charset_g3_is_drawing = is_drawing,
            _ => {}
        }
        return;
    }
    // ESC # sequences (DECALN, DECDWL, DECDHL).
    if intermediates == b"#" {
        match byte {
            // DECALN — Screen Alignment Pattern. Fills the entire visible
            // screen with 'E' characters. Used by vttest and for service
            // alignment testing.
            b'8' => {
                let first_visible = ctx
                    .screen
                    .grid
                    .rows
                    .len()
                    .saturating_sub(ctx.viewport.rows as usize);
                let e_cell = SmolStr::new_inline("E");
                let fg = ctx.palette.fg;
                let bg = ctx.palette.bg;
                for r in first_visible..ctx.screen.grid.rows.len() {
                    let row = &mut ctx.screen.grid.rows[r];
                    for cell in row.cells.iter_mut() {
                        *cell = e_cell.clone();
                    }
                    row.fg.fill(fg);
                    row.bg.fill(bg);
                    row.attrs.fill(CellAttrs::default());
                    row.underline.fill(UnderlineStyle::None);
                    row.underline_color.fill(None);
                }
                // DECALN resets margins, origin mode, and homes the cursor
                // per DEC spec. Without this, vttest's border drawing after
                // DECALN misaligns if a previous test left a restricted
                // scroll region or origin mode enabled.
                ctx.screen.scroll_top = 0;
                ctx.screen.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
                ctx.screen.origin_mode = false;
                ctx.screen.cursor.row = 0;
                ctx.screen.cursor.col = 0;
            }
            // DECDWL (# 6), DECDHL (# 3/4), DECSWL (# 5) — double-width /
            // double-height line attributes. Silently accepted as no-ops.
            b'3' | b'4' | b'5' | b'6' => {}
            _ => {}
        }
        return;
    }

    if !intermediates.is_empty() {
        return;
    }

    // Cancel pending wrap before ESC sequences that move the cursor.
    if ctx.screen.cursor.col >= ctx.viewport.cols {
        ctx.screen.cursor.col = ctx.viewport.cols - 1;
    }

    match byte {
        b'7' => screen::save_cursor_slot(ctx.screen),
        b'8' => screen::restore_cursor_slot(ctx.screen, ctx.viewport),
        // IND — Index. Move the cursor down one line; if at the bottom of
        // the scroll region, scroll the region up.
        b'D' => {
            if ctx.screen.cursor.row == ctx.screen.scroll_bottom {
                if ctx.screen.scroll_top == 0 && ctx.screen.scroll_bottom == ctx.viewport.rows - 1 {
                    ctx.screen.grid.push_visible_row(ctx.viewport);
                } else {
                    ctx.screen.grid.scroll_up_in_region(
                        ctx.viewport,
                        &mut ctx.screen.images,
                        ctx.screen.scroll_top,
                        ctx.screen.scroll_bottom,
                        1,
                    );
                }
            } else if ctx.screen.cursor.row < ctx.viewport.rows - 1 {
                ctx.screen.cursor.row += 1;
            }
        }
        // NEL — Next Line. Move to column 0 of the next line; scroll if
        // at the bottom of the scroll region.
        b'E' => {
            ctx.screen.cursor.col = 0;
            if ctx.screen.cursor.row == ctx.screen.scroll_bottom {
                if ctx.screen.scroll_top == 0 && ctx.screen.scroll_bottom == ctx.viewport.rows - 1 {
                    ctx.screen.grid.push_visible_row(ctx.viewport);
                } else {
                    ctx.screen.grid.scroll_up_in_region(
                        ctx.viewport,
                        &mut ctx.screen.images,
                        ctx.screen.scroll_top,
                        ctx.screen.scroll_bottom,
                        1,
                    );
                }
            } else if ctx.screen.cursor.row < ctx.viewport.rows - 1 {
                ctx.screen.cursor.row += 1;
            }
        }
        b'H' => {
            // HTS — set a tab stop at the current cursor column.
            let col = ctx.screen.cursor.col as usize;
            if col < ctx.screen.tab_stops.len() {
                ctx.screen.tab_stops[col] = true;
            }
        }
        b'c' => {
            // RIS (Reset to Initial State). Drop the app's terminal state
            // back to power-on defaults — every mode the app might have
            // flipped, plus the visible screen. Scrollback is preserved: a
            // misbehaving app's reset shouldn't take the user's history.
            //
            // Return to primary first so subsequent resets land on the screen
            // the user will actually see, and so a crashed alt-screen TUI
            // doesn't strand us there.
            if *ctx.on_alt_screen {
                std::mem::swap(ctx.screen, ctx.stash);
                *ctx.on_alt_screen = false;
            }
            screen::clear_visible(ctx.screen, ctx.viewport);
            screen::clear_visible(ctx.stash, ctx.viewport);
            for s in [&mut *ctx.screen, &mut *ctx.stash] {
                s.cursor = grid::Cursor::default();
                s.fg = ctx.palette.fg;
                s.bg = ctx.palette.bg;
                s.attrs = CellAttrs::default();
                s.underline = UnderlineStyle::None;
                s.underline_color = None;
                s.scroll_top = 0;
                s.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
                s.offset = 0;
                s.saved_cursor = None;
                s.current_hyperlink = None;
                s.cursor_visible = true;
                s.last_char = None;
                s.tab_stops = screen::init_tab_stops(ctx.viewport.cols);
                s.origin_mode = false;
                s.autowrap = true;
                s.app_cursor_keys = false;
                s.charset_g0_is_drawing = false;
                s.charset_g1_is_drawing = false;
                s.charset_g2_is_drawing = false;
                s.charset_g3_is_drawing = false;
                s.charset_gl_is_g0 = true;
                s.single_shift = None;
            }
            *ctx.modes = TerminalModes::new();
            *ctx.kitty_keyboard = KittyKeyboardState::new();
            *ctx.cursor_style = CursorStyle::default();
            *ctx.current_title = None;
            ctx.title_stack.clear();
            ctx.saved_modes.clear();
            *ctx.current_prompt_row = None;
            *ctx.bell_pending = false;
        }
        b'M' => {
            if ctx.screen.cursor.row == ctx.screen.scroll_top {
                ctx.screen.grid.scroll_down_in_region(
                    ctx.viewport,
                    &mut ctx.screen.images,
                    ctx.screen.scroll_top,
                    ctx.screen.scroll_bottom,
                    1,
                );
            } else if ctx.screen.cursor.row > 0 {
                ctx.screen.cursor.row -= 1;
            }
        }
        b'=' | b'>' => {}
        // SS2 — Single Shift G2. Next graphic character uses G2.
        b'N' => ctx.screen.single_shift = Some(2),
        // SS3 — Single Shift G3. Next graphic character uses G3.
        b'O' => ctx.screen.single_shift = Some(3),
        // DECBI — Back Index. Scroll region right if at left margin, else
        // move cursor left.
        b'6' => {
            if ctx.screen.cursor.col == 0 {
                ctx.screen.grid.scroll_right(
                    ctx.viewport,
                    ctx.screen.scroll_top,
                    ctx.screen.scroll_bottom,
                    1,
                );
            } else {
                ctx.screen.cursor.col -= 1;
            }
        }
        // DECFI — Forward Index. Scroll region left if at right margin, else
        // move cursor right.
        b'9' => {
            if ctx.screen.cursor.col >= ctx.viewport.cols - 1 {
                ctx.screen.grid.scroll_left(
                    ctx.viewport,
                    ctx.screen.scroll_top,
                    ctx.screen.scroll_bottom,
                    1,
                );
            } else {
                ctx.screen.cursor.col += 1;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use palette::Srgb;
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::cursor::CursorStyle;
    use crate::keyboard::KittyKeyboardState;
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
        );
        let viewport = Viewport {
            rows: TEST_ROWS,
            cols: TEST_COLS,
        };
        (screen, viewport)
    }

    /// Drive `input` through a VTE parser and dispatch each action through the
    /// parser module under test. This is the same pipeline the live terminal
    /// uses, so tests exercise the same paths callers actually take.
    fn feed(
        input: &[u8],
        screen: &mut Screen,
        viewport: &mut Viewport,
    ) {
        let pal = color::ColorPalette::default();
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;

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
                        if let Action::PrintAscii(run) = &action {
                            if run.len() >= 2 {
                                let row = b.saturating_sub(0x20) as u32;
                                let col = run[1].saturating_sub(0x20) as u32;
                                screen.cursor.row = row.min(viewport.rows.saturating_sub(1));
                                screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                                vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                                if run.len() > 2 {
                                    put_ascii_run(screen, viewport, &run[2..], modes.insert_mode);
                                }
                                continue;
                            }
                        }
                        continue;
                    }
                    (crate::Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                        let col = b.saturating_sub(0x20) as u32;
                        screen.cursor.row = (row as u32).min(viewport.rows.saturating_sub(1));
                        screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                        if let Action::PrintAscii(run) = &action {
                            if run.len() > 1 {
                                put_ascii_run(screen, viewport, &run[1..], modes.insert_mode);
                            }
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
                Action::PrintAscii(run) => put_ascii_run(screen, viewport, run, modes.insert_mode),
                Action::Print(s) => put_char(screen, viewport, s, modes.insert_mode),
                Action::Execute(b) => {
                    execute(screen, viewport, b, &mut bell_pending, modes.newline_mode)
                }
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    let mut ctx = CsiContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        pending_output: &mut pending_output,
                        cursor_style: &mut cursor_style,
                        cell_width: 8,
                        cell_height: 16,
                        palette: &pal,
                        title_stack: &mut title_stack,
                        current_title: &mut current_title,
                        saved_modes: &mut saved_modes,
                    };
                    csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    let mut ctx = EscContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        cursor_style: &mut cursor_style,
                        current_title: &mut current_title,
                        title_stack: &mut title_stack,
                        saved_modes: &mut saved_modes,
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
                        palette: &pal,
                        pending_output: &mut pending_output,
                        vt52_cursor_addr: &mut vt52_cursor_addr,
                    };
                    esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
                }
                _ => {}
            }
        }
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
    fn put_char_zwj_emoji_keeps_components_in_separate_wide_cells() {
        let (mut screen, mut viewport) = setup();
        // 👨‍💻 = 👨 ZWJ 💻. wcswidth = 2+0+2 = 4, so the shell expects the
        // cursor to advance by 4. The ZWJ folds into `👨` (width 0 → fold),
        // but the second emoji starts a new wide cell of its own. The font
        // shaper still sees the full ZWJ sequence in `row_text` and renders
        // the ligature if the font has one.
        feed("👨\u{200D}💻".as_bytes(), &mut screen, &mut viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "👨\u{200D}");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "💻");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "");
        assert_eq!(screen.cursor.col, 4);
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
    fn esc_keypad_modes_are_noop() {
        let (mut screen, mut viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        feed(b"\x1b=", &mut screen, &mut viewport);
        feed(b"\x1b>", &mut screen, &mut viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
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

    // -- DECRQM (CSI ? Ps $ p) -----------------------------------------------

    /// Like `feed` but returns the `pending_output` bytes written by query
    /// responses (DECRQM, DSR, etc.).
    fn feed_with_output(
        input: &[u8],
        screen: &mut Screen,
        viewport: &mut Viewport,
    ) -> Vec<u8> {
        let pal = color::ColorPalette::default();
        let mut parser = Parser::new();
        let mut stash = Screen::new(
            viewport.cols,
            viewport.rows,
            0,
            color::default_fg(),
            color::default_bg(),
        );
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut title_stack = Vec::new();
        let mut saved_modes = std::collections::HashMap::new();
        let mut current_prompt_row = None;
        let mut vt52_cursor_addr = crate::Vt52CursorAddr::Idle;

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
                        if let Action::PrintAscii(run) = &action {
                            if run.len() >= 2 {
                                let row = b.saturating_sub(0x20) as u32;
                                let col = run[1].saturating_sub(0x20) as u32;
                                screen.cursor.row = row.min(viewport.rows.saturating_sub(1));
                                screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                                vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                                if run.len() > 2 {
                                    put_ascii_run(screen, viewport, &run[2..], modes.insert_mode);
                                }
                                continue;
                            }
                        }
                        continue;
                    }
                    (crate::Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                        let col = b.saturating_sub(0x20) as u32;
                        screen.cursor.row = (row as u32).min(viewport.rows.saturating_sub(1));
                        screen.cursor.col = col.min(viewport.cols.saturating_sub(1));
                        vt52_cursor_addr = crate::Vt52CursorAddr::Idle;
                        if let Action::PrintAscii(run) = &action {
                            if run.len() > 1 {
                                put_ascii_run(screen, viewport, &run[1..], modes.insert_mode);
                            }
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
                Action::PrintAscii(run) => put_ascii_run(screen, viewport, run, modes.insert_mode),
                Action::Print(s) => put_char(screen, viewport, s, modes.insert_mode),
                Action::Execute(b) => {
                    execute(screen, viewport, b, &mut bell_pending, modes.newline_mode)
                }
                Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    let mut ctx = CsiContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        pending_output: &mut pending_output,
                        cursor_style: &mut cursor_style,
                        cell_width: 8,
                        cell_height: 16,
                        palette: &pal,
                        title_stack: &mut title_stack,
                        current_title: &mut current_title,
                        saved_modes: &mut saved_modes,
                    };
                    csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
                }
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    let mut ctx = EscContext {
                        screen,
                        stash: &mut stash,
                        viewport,
                        on_alt_screen: &mut on_alt_screen,
                        modes: &mut modes,
                        kitty_keyboard: &mut kitty_keyboard,
                        cursor_style: &mut cursor_style,
                        current_title: &mut current_title,
                        title_stack: &mut title_stack,
                        saved_modes: &mut saved_modes,
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
                        palette: &pal,
                        pending_output: &mut pending_output,
                        vt52_cursor_addr: &mut vt52_cursor_addr,
                    };
                    esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
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
        );
        let mut viewport = Viewport {
            rows: TEST_ROWS,
            cols: screen_cols,
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
        );
        let mut viewport = Viewport {
            rows: TEST_ROWS,
            cols: screen_cols,
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
        assert!(screen.charset_g0_is_drawing);
        // DECSTR should reset charset state.
        feed(b"\x1b[!p", &mut screen, &mut viewport);
        assert!(!screen.charset_g0_is_drawing);
        assert!(!screen.charset_g1_is_drawing);
        assert!(screen.charset_gl_is_g0);
    }

    #[test]
    fn scs_ris_resets_charset_state() {
        let (mut screen, mut viewport) = setup();
        feed(b"\x1b(0\x1b)0\x0E", &mut screen, &mut viewport);
        assert!(screen.charset_g0_is_drawing);
        assert!(screen.charset_g1_is_drawing);
        assert!(!screen.charset_gl_is_g0);
        // RIS should reset everything.
        feed(b"\x1bc", &mut screen, &mut viewport);
        assert!(!screen.charset_g0_is_drawing);
        assert!(!screen.charset_g1_is_drawing);
        assert!(screen.charset_gl_is_g0);
    }

    #[test]
    fn scs_save_restore_cursor_preserves_charset() {
        let (mut screen, mut viewport) = setup();
        // Enable drawing in G0, save cursor.
        feed(b"\x1b(0\x1b7", &mut screen, &mut viewport);
        // Switch back to ASCII.
        feed(b"\x1b(B", &mut screen, &mut viewport);
        assert!(!screen.charset_g0_is_drawing);
        // Restore cursor — should bring back DEC drawing.
        feed(b"\x1b8", &mut screen, &mut viewport);
        assert!(screen.charset_g0_is_drawing);
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
        assert!(screen.charset_g0_is_drawing);
    }

    #[test]
    fn vt52_graphics_mode_off() {
        let (mut screen, mut viewport) = setup();
        // Enable then disable in the same parse pass.
        feed(b"\x1b[?2l\x1bF\x1bG", &mut screen, &mut viewport);
        assert!(!screen.charset_g0_is_drawing);
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

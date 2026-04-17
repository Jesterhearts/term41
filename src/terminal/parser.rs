use std::io::Write;
use std::sync::LazyLock;
use std::time::Instant;

use font41::attrs::CellAttrs;
use smol_str::SmolStr;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vtepp::Params;

use crate::terminal::TerminalModes;
use crate::terminal::color;
use crate::terminal::color::apply_sgr;
use crate::terminal::cursor::CursorStyle;
use crate::terminal::grid;
use crate::terminal::grid::Viewport;
use crate::terminal::keyboard::KittyKeyboardState;
use crate::terminal::keyboard::handle_kitty_keyboard;
use crate::terminal::mouse::apply_mouse_mode;
use crate::terminal::row::Row;
use crate::terminal::screen;
use crate::terminal::screen::Screen;

/// Bundles the bits of [`Terminal`](super::Terminal) state that CSI handlers
/// need beyond the active screen. Keeps the call signature stable as new CSI
/// sequences get wired in.
pub(super) struct CsiContext<'a> {
    pub screen: &'a mut Screen,
    pub stash: &'a mut Screen,
    pub viewport: &'a Viewport,
    pub on_alt_screen: &'a mut bool,
    pub modes: &'a mut TerminalModes,
    pub kitty_keyboard: &'a mut KittyKeyboardState,
    pub pending_output: &'a mut Vec<u8>,
    pub cursor_style: &'a mut CursorStyle,
    pub cell_width: u32,
    pub cell_height: u32,
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
    pub current_prompt_row: &'a mut Option<u64>,
    pub bell_pending: &'a mut bool,
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

/// Hardware tab stop width in columns.
const TAB_WIDTH: u32 = 8;

/// SCS (Select Character Set) intermediate bytes that designate G0..G3.
/// We accept and silently ignore these rather than treating the sequence as
/// unknown.
const SCS_INTERMEDIATES: &[u8] = b"()*+";

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
) {
    if run.is_empty() {
        return;
    }

    let fg = screen.fg;
    let bg = screen.bg;
    let attrs = screen.attrs;
    let link = screen.current_hyperlink;

    // Record the last byte of the run for REP (CSI Ps b).
    let last_byte = *run.last().unwrap();
    screen.last_char = Some(ASCII_CELLS[(last_byte - 0x20) as usize].clone());

    let mut i = 0;
    while i < run.len() {
        // Pre-wrap: a cursor parked past the last column wraps before
        // writing, matching put_char's soft-wrap behaviour.
        if screen.cursor.col >= viewport.cols {
            soft_wrap(screen, viewport);
        }

        let r = screen.grid.active_row_index(&screen.cursor, viewport);
        let col = screen.cursor.col as usize;
        let remaining_cols = (viewport.cols - screen.cursor.col) as usize;
        let chunk_len = (run.len() - i).min(remaining_cols);

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
        row.links[col..col + chunk_len].fill(link);

        screen.cursor.col += chunk_len as u32;
        i += chunk_len;
    }
}

pub(super) fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    s: SmolStr,
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

    let width = raw_width.max(1);

    // Soft-wrap when the incoming cluster (possibly wide) would overhang the
    // right edge. The existing pre-wrap also covers the cursor-past-end case
    // left behind by the previous character.
    if screen.cursor.col + width as u32 > viewport.cols {
        soft_wrap(screen, viewport);
    }

    let fg = screen.fg;
    let bg = screen.bg;
    let attrs = screen.attrs;
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
    screen.grid.rows[r].links[col] = link;
    for i in 1..width {
        screen.grid.rows[r].cells[col + i] = continuation_cell();
        screen.grid.rows[r].fg[col + i] = fg;
        screen.grid.rows[r].bg[col + i] = bg;
        screen.grid.rows[r].attrs[col + i] = attrs;
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
) {
    match byte {
        b'\n' => {
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
            let next = (screen.cursor.col / TAB_WIDTH + 1) * TAB_WIDTH;
            screen.cursor.col = next.min(viewport.cols - 1);
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
    // -- Sequences that carry intermediates ----------------------------------

    if intermediates == b"?" && matches!(action, 'h' | 'l') {
        let enable = action == 'h';
        for p in params.iter() {
            if p[0] == 2004 {
                ctx.modes.bracketed_paste = enable;
            } else if p[0] == 1004 {
                ctx.modes.focus_reporting = enable;
            } else if p[0] == 2026 {
                // BSU refreshes the deadline; ESU clears it. Refreshing on a
                // nested BSU matches the contour spec's "keep the window open"
                // rule for apps that chain updates.
                ctx.modes.synchronized_update_since = enable.then(Instant::now);
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
        screen.fg = color::default_fg();
        screen.bg = color::default_bg();
        screen.attrs = CellAttrs::default();
        screen.scroll_top = 0;
        screen.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
        screen.saved_cursor = None;
        screen.current_hyperlink = None;
        screen.cursor_visible = true;
        screen.last_char = None;
        *ctx.modes = TerminalModes::new();
        *ctx.kitty_keyboard = KittyKeyboardState::new();
        *ctx.cursor_style = CursorStyle::default();
        return;
    }

    // -- No-intermediates sequences -----------------------------------------

    if !intermediates.is_empty() {
        return;
    }

    // DA1 needs pending_output, which lives on ctx rather than on the screen.
    // Handle it before borrowing ctx.screen for the screen-only match below.
    if action == 'c' {
        // DA1 (Primary Device Attributes). Reply as a VT220 (62).
        ctx.pending_output.extend_from_slice(b"\x1b[?62;c");
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
            for _ in 0..n {
                put_char(ctx.screen, ctx.viewport, ch.clone());
            }
        }
        return;
    }

    let screen = &mut *ctx.screen;
    let viewport = ctx.viewport;
    let p: Vec<u16> = params.iter().map(|p| p[0]).collect();
    let cursor = &mut screen.cursor;

    match action {
        'A' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = cursor.row.saturating_sub(n);
        }
        'B' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.row = (cursor.row + n).min(viewport.rows - 1);
        }
        'C' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = (cursor.col + n).min(viewport.cols - 1);
        }
        'D' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            cursor.col = cursor.col.saturating_sub(n);
        }
        'H' | 'f' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let col = p.get(1).copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.row = row.min(viewport.rows - 1);
            cursor.col = col.min(viewport.cols - 1);
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
        'm' => apply_sgr(&mut screen.fg, &mut screen.bg, &mut screen.attrs, params),
        'd' => {
            let row = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.row = row.min(viewport.rows - 1);
        }
        'G' => {
            let col = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            cursor.col = col.min(viewport.cols - 1);
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
            screen.cursor.row = 0;
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
        _ => {}
    }
}

pub(super) fn esc_dispatch(
    ctx: &mut EscContext<'_>,
    intermediates: &[u8],
    byte: u8,
) {
    if intermediates
        .first()
        .is_some_and(|&b| SCS_INTERMEDIATES.contains(&b))
    {
        return;
    }
    if !intermediates.is_empty() {
        return;
    }

    match byte {
        b'7' => screen::save_cursor_slot(ctx.screen),
        b'8' => screen::restore_cursor_slot(ctx.screen, ctx.viewport),
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
                s.fg = color::default_fg();
                s.bg = color::default_bg();
                s.attrs = CellAttrs::default();
                s.scroll_top = 0;
                s.scroll_bottom = ctx.viewport.rows.saturating_sub(1);
                s.offset = 0;
                s.saved_cursor = None;
                s.current_hyperlink = None;
                s.cursor_visible = true;
                s.last_char = None;
            }
            *ctx.modes = TerminalModes::new();
            *ctx.kitty_keyboard = KittyKeyboardState::new();
            *ctx.cursor_style = CursorStyle::default();
            *ctx.current_title = None;
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
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use palette::Srgb;
    use vtepp::Action;
    use vtepp::Parser;

    use super::*;
    use crate::terminal::cursor::CursorStyle;
    use crate::terminal::keyboard::KittyKeyboardState;
    use crate::terminal::screen::Screen;

    const TEST_COLS: u32 = 10;
    const TEST_ROWS: u32 = 4;

    fn setup() -> (Screen, Viewport) {
        let screen = Screen::new(TEST_COLS, TEST_ROWS, 100);
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
        viewport: &Viewport,
    ) {
        let mut parser = Parser::new();
        let mut stash = Screen::new(viewport.cols, viewport.rows, 0);
        let mut on_alt_screen = false;
        let mut modes = TerminalModes::new();
        let mut kitty_keyboard = KittyKeyboardState::new();
        let mut pending_output = Vec::new();
        let mut cursor_style = CursorStyle::default();
        let mut bell_pending = false;
        let mut current_title = None;
        let mut current_prompt_row = None;

        for action in parser.parse(input) {
            match action {
                Action::PrintAscii(run) => put_ascii_run(screen, viewport, run),
                Action::Print(s) => put_char(screen, viewport, s),
                Action::Execute(b) => execute(screen, viewport, b, &mut bell_pending),
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
                        current_prompt_row: &mut current_prompt_row,
                        bell_pending: &mut bell_pending,
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

        put_char(&mut screen, &viewport, SmolStr::new_inline("A"));

        assert_eq!(row_text(&screen, &viewport, 0).chars().next(), Some('A'));
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].fg[0], Srgb::new(1, 2, 3));
        assert_eq!(screen.grid.rows[r].bg[0], Srgb::new(4, 5, 6));
        assert_eq!(screen.cursor.col, 1);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn put_char_soft_wraps_at_right_edge() {
        let (mut screen, viewport) = setup();
        feed(b"abcdefghij", &mut screen, &viewport);

        // Cursor sits past the right edge; the next char should wrap.
        assert_eq!(screen.cursor.col, TEST_COLS);
        feed(b"k", &mut screen, &viewport);

        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
        assert!(
            screen.grid.rows[screen.grid.active_row_index(&screen.cursor, &viewport) - 1].wrapped
        );
        assert_eq!(&row_text(&screen, &viewport, 1)[..1], "k");
    }

    #[test]
    fn put_char_folds_combining_mark_into_previous_cell() {
        let (mut screen, viewport) = setup();
        // U+0301 COMBINING ACUTE ACCENT — feeding "e" then the combining mark
        // should store the full grapheme "é" in one cell without advancing.
        feed("e\u{0301}".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "e\u{0301}");
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn put_char_vs16_emoji_stays_in_single_cell() {
        let (mut screen, viewport) = setup();
        // `UnicodeWidthStr::width("❤\u{FE0F}") == 2`, but glibc `wcswidth`
        // reports 1 because it treats VS16 as a zero-width variation
        // selector without upgrading the base to emoji presentation. The
        // host shell tracks cursor position via wcswidth, so our grid must
        // agree — otherwise a single backspace from readline lands on the
        // continuation cell and the user can't delete the emoji. Keep the
        // cluster in one cell; the shaper still sees the full cluster
        // text and renders it scaled to that cell.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &viewport);

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
        let (mut screen, viewport) = setup();
        feed("\u{2764}\u{FE0F}X".as_bytes(), &mut screen, &viewport);

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
        let (mut screen, viewport) = setup();
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 1);

        execute(&mut screen, &viewport, BS, &mut false);
        assert_eq!(screen.cursor.col, 0);

        // A full rub-out of `\b \b` from bash lands us back at col 0 with
        // the cell erased.
        feed("\u{2764}\u{FE0F}".as_bytes(), &mut screen, &viewport);
        feed(b"\x08 \x08", &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn put_char_regional_indicators_get_separate_cells() {
        let (mut screen, viewport) = setup();
        // `unicode-width` reports width 1 for each regional indicator, so
        // "🇺🇸" advances the cursor by 2 across two 1-col cells. We do not
        // collapse the flag pair into one cell — that would disagree with
        // the host's wcswidth and desync the cursor.
        feed("🇺🇸".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "🇺");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "🇸");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- wide (2-column) glyph handling ------------------------------------

    #[test]
    fn put_char_wide_glyph_occupies_two_cells_and_advances_cursor() {
        let (mut screen, viewport) = setup();
        feed("好".as_bytes(), &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "好");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), ""); // continuation
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn put_char_wide_glyph_soft_wraps_when_it_would_overhang() {
        let (mut screen, viewport) = setup();
        // Fill 9 of 10 columns with narrow chars so only 1 column is free.
        feed(b"abcdefghi", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 9);

        feed("好".as_bytes(), &mut screen, &viewport);

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
        let (mut screen, viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &viewport);
        // Move cursor back to col 0 and stomp on the anchor with a narrow char.
        feed(b"\x1b[1;1H", &mut screen, &viewport);
        feed(b"x", &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "x");
        // The continuation at col 1 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_narrow_overwriting_wide_continuation_blanks_anchor() {
        let (mut screen, viewport) = setup();
        feed("好b".as_bytes(), &mut screen, &viewport);
        // Park cursor on the continuation (col 1) and write a narrow char.
        feed(b"\x1b[1;2H", &mut screen, &viewport);
        feed(b"x", &mut screen, &viewport);

        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        // The anchor at col 0 is now orphaned — must be blanked.
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), " ");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "x");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "b");
    }

    #[test]
    fn put_char_wide_overwriting_wide_blanks_both_neighbours() {
        let (mut screen, viewport) = setup();
        // [好, "", 世, "", a]
        feed("好世a".as_bytes(), &mut screen, &viewport);
        // Park on col 1 (好's continuation) and write a new wide glyph that
        // straddles the old layout.
        feed(b"\x1b[1;2H", &mut screen, &viewport);
        feed("界".as_bytes(), &mut screen, &viewport);

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
        let (mut screen, viewport) = setup();
        // 👨‍💻 = 👨 ZWJ 💻. wcswidth = 2+0+2 = 4, so the shell expects the
        // cursor to advance by 4. The ZWJ folds into `👨` (width 0 → fold),
        // but the second emoji starts a new wide cell of its own. The font
        // shaper still sees the full ZWJ sequence in `row_text` and renders
        // the ligature if the font has one.
        feed("👨\u{200D}💻".as_bytes(), &mut screen, &viewport);

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
        execute(&mut screen, &viewport, b'\n', &mut false);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn execute_lf_at_scroll_bottom_scrolls_up() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = screen.scroll_bottom;
        let rows_before = screen.grid.rows.len();

        execute(&mut screen, &viewport, b'\n', &mut false);

        assert_eq!(screen.cursor.row, screen.scroll_bottom);
        assert_eq!(screen.grid.rows.len(), rows_before + 1);
    }

    #[test]
    fn execute_cr_resets_col_to_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 5;
        execute(&mut screen, &viewport, b'\r', &mut false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_bs_saturates_at_zero() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 2;
        execute(&mut screen, &viewport, BS, &mut false);
        assert_eq!(screen.cursor.col, 1);
        execute(&mut screen, &viewport, BS, &mut false);
        execute(&mut screen, &viewport, BS, &mut false);
        execute(&mut screen, &viewport, BS, &mut false);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn execute_tab_advances_to_next_tab_stop() {
        let (mut screen, viewport) = setup();
        execute(&mut screen, &viewport, b'\t', &mut false);
        assert_eq!(screen.cursor.col, TAB_WIDTH);

        screen.cursor.col = 3;
        execute(&mut screen, &viewport, b'\t', &mut false);
        assert_eq!(screen.cursor.col, TAB_WIDTH);
    }

    #[test]
    fn execute_tab_clamps_at_rightmost_column() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = TEST_COLS - 1;
        execute(&mut screen, &viewport, b'\t', &mut false);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn execute_bel_sets_bell_pending() {
        let (mut screen, viewport) = setup();
        let mut bell = false;
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, BEL, &mut bell);
        assert!(bell);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    #[test]
    fn execute_nul_is_noop() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 3;
        screen.cursor.row = 2;
        execute(&mut screen, &viewport, NUL, &mut false);
        assert_eq!(screen.cursor.col, 3);
        assert_eq!(screen.cursor.row, 2);
    }

    // -- csi_dispatch cursor movement --------------------------------------

    #[test]
    fn csi_a_moves_cursor_up_by_count() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 3;
        feed(b"\x1b[2A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_defaults_to_one() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_zero_parameter_treated_as_one() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[0A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn csi_a_saturates_at_top() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 1;
        feed(b"\x1b[99A", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn csi_b_moves_cursor_down_clamped() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[99B", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
    }

    #[test]
    fn csi_c_moves_cursor_right_clamped() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[99C", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_d_moves_cursor_left_saturating() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 2;
        feed(b"\x1b[5D", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_positions_cursor_one_based() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[3;5H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_h_defaults_to_origin() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 5;
        feed(b"\x1b[H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_h_clamps_to_viewport() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[99;99H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, TEST_ROWS - 1);
        assert_eq!(screen.cursor.col, TEST_COLS - 1);
    }

    #[test]
    fn csi_f_is_alias_of_h() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;3f", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_s_saves_and_csi_u_restores_cursor() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s", &mut screen, &viewport);
        // Move elsewhere after saving.
        feed(b"\x1b[4;5H", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 3);
        assert_eq!(screen.cursor.col, 4);
        feed(b"\x1b[u", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_u_without_prior_save_homes_cursor() {
        // Matches DECRC semantics: no saved slot → cursor homes to 0,0.
        // Live-updating scripts that call `CSI u` on the first paint
        // before any `CSI s` get predictable behaviour instead of a
        // surprise no-op that leaves the cursor mid-screen.
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;3H", &mut screen, &viewport);
        feed(b"\x1b[u", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_s_shares_slot_with_esc_7() {
        // SCOSC and DECSC write the same slot, so an `ESC 8` after a
        // `CSI s` restores the CSI-written position.
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s", &mut screen, &viewport);
        feed(b"\x1b[4;5H", &mut screen, &viewport);
        feed(b"\x1b8", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_u_does_not_trip_kitty_keyboard_path() {
        // The kitty CSI-u path requires an intermediate (`>`, `<`, `=`,
        // `?`). A plain `CSI u` must fall through to SCORC — this test
        // guards against anyone re-ordering the kitty check in front of
        // the SCORC arm.
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;3H\x1b[s\x1b[4;5H\x1b[u", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 2);
    }

    #[test]
    fn csi_g_sets_column_only() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1b[5G", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn csi_d_lowercase_sets_row_only() {
        let (mut screen, viewport) = setup();
        screen.cursor.col = 5;
        feed(b"\x1b[3d", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 5);
    }

    // -- csi_dispatch erase / SGR / scroll region --------------------------

    #[test]
    fn csi_j_2_erases_entire_display() {
        let (mut screen, viewport) = setup();
        feed(b"hello\nworld", &mut screen, &viewport);
        feed(b"\x1b[2J", &mut screen, &viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim(), "");
    }

    #[test]
    fn csi_k_erases_to_end_of_line() {
        let (mut screen, viewport) = setup();
        feed(b"hello", &mut screen, &viewport);
        feed(b"\x1b[3G", &mut screen, &viewport); // col=2
        feed(b"\x1b[K", &mut screen, &viewport);
        assert_eq!(row_text(&screen, &viewport, 0).trim_end(), "he");
    }

    #[test]
    fn csi_m_applies_sgr_colors() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[31m", &mut screen, &viewport);
        // SGR 31 = ANSI red fg, which is (205, 0, 0) in the standard palette.
        assert_eq!(screen.fg, Srgb::new(205, 0, 0));
    }

    #[test]
    fn csi_r_sets_scroll_region_and_homes_cursor() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 3;
        screen.cursor.col = 5;
        feed(b"\x1b[2;3r", &mut screen, &viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        assert_eq!(screen.cursor.row, 0);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn csi_r_clamps_bounds_to_viewport() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[1;99r", &mut screen, &viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, TEST_ROWS - 1);
    }

    #[test]
    fn csi_with_intermediate_is_ignored() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // Intermediate ` ` before action `q` is a valid CSI shape but not one
        // we handle — we must leave state untouched.
        feed(b"\x1b[1 q", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn csi_unknown_action_is_ignored() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 1;
        screen.cursor.col = 1;
        feed(b"\x1b[1Z", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
        assert_eq!(screen.cursor.col, 1);
    }

    // -- esc_dispatch ------------------------------------------------------

    #[test]
    fn esc_m_at_scroll_top_scrolls_down() {
        let (mut screen, viewport) = setup();
        feed(b"top\nmid\nbot", &mut screen, &viewport);
        // Cursor is at scroll_top (row 0) after moving back there.
        feed(b"\x1b[H", &mut screen, &viewport);
        feed(b"\x1bM", &mut screen, &viewport);
        // After scroll-down, the old top row shifts down one and row 0 blanks.
        assert_eq!(row_text(&screen, &viewport, 0).trim(), "");
        assert_eq!(row_text(&screen, &viewport, 1).trim_end(), "top");
    }

    #[test]
    fn esc_m_above_scroll_top_moves_cursor_up() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        feed(b"\x1bM", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 1);
    }

    #[test]
    fn esc_m_at_row_zero_outside_region_is_noop() {
        // scroll_top defaults to 0, so row 0 triggers scroll_down_in_region
        // above. Force a non-zero scroll_top to exercise the cursor.row > 0
        // branch at exactly row 0 of the viewport.
        let (mut screen, viewport) = setup();
        feed(b"\x1b[2;4r", &mut screen, &viewport); // scroll_top = 1
        screen.cursor.row = 0;
        feed(b"\x1bM", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 0);
    }

    #[test]
    fn esc_scs_designator_is_ignored() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        // ESC ( B designates US-ASCII as G0. Parser should no-op without
        // dropping state or panicking on the `B` byte (which would otherwise
        // land in the unknown-byte arm).
        feed(b"\x1b(B", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    #[test]
    fn esc_keypad_modes_are_noop() {
        let (mut screen, viewport) = setup();
        screen.cursor.row = 2;
        screen.cursor.col = 3;
        feed(b"\x1b=", &mut screen, &viewport);
        feed(b"\x1b>", &mut screen, &viewport);
        assert_eq!(screen.cursor.row, 2);
        assert_eq!(screen.cursor.col, 3);
    }

    // -- REP (CSI Ps b) ---------------------------------------------------

    #[test]
    fn rep_repeats_last_printed_char() {
        let (mut screen, viewport) = setup();
        // Print 'A' then repeat it 3 times.
        feed(b"A\x1b[3b", &mut screen, &viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[2].as_str(), "A");
        assert_eq!(screen.grid.rows[r].cells[3].as_str(), "A");
        assert_eq!(screen.cursor.col, 4);
    }

    #[test]
    fn rep_without_prior_char_is_noop() {
        let (mut screen, viewport) = setup();
        feed(b"\x1b[3b", &mut screen, &viewport);
        assert_eq!(screen.cursor.col, 0);
    }

    #[test]
    fn rep_defaults_to_one_repetition() {
        let (mut screen, viewport) = setup();
        feed(b"X\x1b[b", &mut screen, &viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        assert_eq!(screen.grid.rows[r].cells[0].as_str(), "X");
        assert_eq!(screen.grid.rows[r].cells[1].as_str(), "X");
        assert_eq!(screen.cursor.col, 2);
    }

    // -- DECSTR (CSI ! p) -------------------------------------------------

    #[test]
    fn decstr_resets_attrs_and_colors() {
        let (mut screen, viewport) = setup();
        // Set bold + reverse + custom colors.
        feed(b"\x1b[1;7;31;42m", &mut screen, &viewport);
        assert!(screen.attrs.contains(CellAttrs::BOLD));
        assert!(screen.attrs.contains(CellAttrs::REVERSE));
        assert_ne!(screen.fg, color::default_fg());
        // Soft reset.
        feed(b"\x1b[!p", &mut screen, &viewport);
        assert_eq!(screen.attrs, CellAttrs::default());
        assert_eq!(screen.fg, color::default_fg());
        assert_eq!(screen.bg, color::default_bg());
    }

    #[test]
    fn decstr_resets_scroll_region() {
        let (mut screen, viewport) = setup();
        // Set a restrictive scroll region.
        feed(b"\x1b[2;3r", &mut screen, &viewport);
        assert_eq!(screen.scroll_top, 1);
        assert_eq!(screen.scroll_bottom, 2);
        // Soft reset should restore full region.
        feed(b"\x1b[!p", &mut screen, &viewport);
        assert_eq!(screen.scroll_top, 0);
        assert_eq!(screen.scroll_bottom, viewport.rows - 1);
    }

    #[test]
    fn decstr_preserves_screen_contents() {
        let (mut screen, viewport) = setup();
        feed(b"Hello", &mut screen, &viewport);
        let r = screen.grid.active_row_index(&screen.cursor, &viewport);
        let before: Vec<_> = screen.grid.rows[r].cells[..5]
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        feed(b"\x1b[!p", &mut screen, &viewport);
        let after: Vec<_> = screen.grid.rows[r].cells[..5]
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        assert_eq!(before, after);
    }
}

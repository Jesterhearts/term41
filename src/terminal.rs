mod attrs;
mod color;
mod cursor;
mod grid;
mod hyperlink;
mod image;
mod keyboard;
mod mouse;
mod osc;
mod parser;
mod row;
mod screen;

use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

pub use self::attrs::CellAttrs;
pub use self::cursor::CursorShape;
pub use self::cursor::CursorStyle;
pub use self::grid::Viewport;
pub use self::hyperlink::HyperlinkRegistry;
pub use self::image::PlacedImage;
pub use self::image::VisibleImage;
pub use self::keyboard::KittyFlags;
pub use self::keyboard::KittyKeyboardState;
pub use self::keyboard::KittyKeys;
pub use self::mouse::MouseButton;
pub use self::mouse::MouseEncoding;
pub use self::mouse::MouseEventKind;
pub use self::mouse::MouseModifiers;
pub use self::mouse::MouseTracking;
pub use self::row::Row;
pub use self::screen::Screen;
use crate::clipboard::Clipboard;
use crate::clipboard::ClipboardKind;
use crate::search::MatchSpan;
use crate::search::SearchState;
use crate::selection::Selection;
use crate::selection::SelectionMode;
use crate::selection::SelectionPoint;
use crate::selection::expand_to_line;
use crate::selection::expand_to_word;
use crate::sixel::parse_sixel;
use crate::terminal::keyboard::handle_kitty_keyboard;
use crate::terminal::mouse::apply_mouse_mode;
use crate::terminal::mouse::encode_mouse_event;
use crate::terminal::mouse::should_report;
use crate::terminal::osc::OscContext;
use crate::terminal::osc::handle_osc;
use crate::terminal::parser::csi_dispatch;
use crate::terminal::parser::esc_dispatch;
use crate::terminal::parser::execute;
use crate::terminal::parser::put_ascii_run;
use crate::terminal::parser::put_char;
use crate::terminal::screen::resize_screen;
use crate::terminal::screen::restore_cursor_slot;
use crate::terminal::screen::save_cursor_slot;
use crate::terminal::screen::set_private_mode;
use crate::vte;
use crate::vte::Params;

#[derive(Debug)]
pub struct Terminal {
    pub active: Screen,
    pub stash: Screen,
    pub viewport: Viewport,

    /// `true` when the alt screen is active, `false` when the primary
    /// screen is active. Initialized to `false`; `stash` starts as the alt
    /// screen.
    pub on_alt_screen: bool,

    /// Cell height in pixels, used to convert sixel image pixel height to rows.
    cell_height: u32,

    next_image_id: u64,

    parser: vte::Parser,
    hook_bytes: Vec<Vec<u8>>,
    hook_params: Vec<Params>,
    hook_action: Vec<char>,

    /// System clipboard gateway. Shared between OSC 52 and mouse-driven
    /// copy/paste paths.
    clipboard: Clipboard,

    /// Bytes produced by the terminal itself that must be written back to
    /// the PTY — responses to queries like OSC 52 `?` reads. Drained by the
    /// event loop after each [`process`](Self::process) call.
    pending_output: Vec<u8>,

    /// Currently-active mouse tracking mode requested by the app via DECSET.
    mouse_tracking: MouseTracking,

    /// Wire encoding used for mouse events.
    mouse_encoding: MouseEncoding,

    /// Mode 2004 — when enabled, pasted text is wrapped in
    /// `\x1b[200~ ... \x1b[201~` so apps can distinguish it from typed input.
    bracketed_paste: bool,

    /// Active text selection, if any. Positions use absolute row indices so
    /// the selection stays locked to content across scrollback trimming.
    pub selection: Option<Selection>,

    /// Search-in-scrollback state: open/closed, query text, match cache.
    /// When `active`, the host reroutes keyboard events into this struct
    /// instead of writing them to the PTY. Lives on the terminal so both
    /// the match renderer and the scroll-to-match navigator can touch it.
    pub search: SearchState,

    /// Last directory reported by the foreground shell via OSC 7. None when
    /// no shell has reported, or after a remote-session shell sent an empty
    /// payload to disclaim its previous report. Useful for "open new window
    /// here" and any title-bar surfacing of the current directory.
    pub current_directory: Option<PathBuf>,

    /// Interns OSC 8 hyperlink targets so each cell only has to carry a
    /// 4-byte id. Lives on the terminal (not per-screen) so a link active
    /// when the alt screen is entered keeps resolving on return.
    pub hyperlinks: HyperlinkRegistry,

    /// Kitty keyboard protocol mode stack. Apps push richer key encodings
    /// here when they want unambiguous Ctrl+letter, Shift+Enter, etc. The
    /// effective flags drive the input encoder in `main.rs`.
    pub kitty_keyboard: KittyKeyboardState,

    /// Cursor shape and blink, settable both via config and at runtime via
    /// DECSCUSR (`CSI Ps SP q`). The renderer reads this each frame; the
    /// blink phase itself is owned by the renderer.
    pub cursor_style: CursorStyle,

    /// Mode `?1004` — when enabled, focus changes are reported to the
    /// foreground app as `\x1b[I` (focus in) and `\x1b[O` (focus out). The
    /// event loop calls [`Self::report_focus_change`] on every winit
    /// `Focused` event; that method gates emission on this flag.
    focus_reporting: bool,

    /// Title last reported by the foreground app via OSC 0 / OSC 2.
    /// `None` means no app has set a title (or one explicitly cleared
    /// it); the host applies its default ("term41") in that case.
    pub current_title: Option<String>,

    /// Latched true whenever the parser sees a BEL byte (0x07). The host
    /// drains this each frame via [`Self::take_bell_pending`] so it can
    /// flash the screen, ping the compositor, etc. Latched (not
    /// counted) because reacting once per frame is the right grain — a
    /// noisy app that bells in a tight loop should still get one
    /// per-frame response, not a queue that backs up forever.
    bell_pending: bool,

    /// Mode 2026 — Synchronized Output (BSU/ESU). `Some(t)` from the moment
    /// `CSI ? 2026 h` arrives until either `CSI ? 2026 l` clears it or the
    /// [`SYNCHRONIZED_UPDATE_TIMEOUT`] safety deadline passes; otherwise
    /// `None`. The host consults [`Self::is_synchronized_update_active`] to
    /// decide whether to skip the frame. State still updates during a BSU —
    /// only the render is deferred, so the eventual ESU (or timeout) lands
    /// on a fully-parsed frame.
    synchronized_update_since: Option<Instant>,

    /// Absolute row index of the most recent OSC 133 `A` (prompt-start)
    /// mark. An OSC 133 `D` resolves to this row and stamps its exit code
    /// there, so the success/failure indicator sits next to the prompt
    /// line — the anchor the user scrolls to — rather than the end of the
    /// command's output. `None` before any shell-integration prompt has
    /// been seen.
    ///
    /// Lives on `Terminal` rather than per-`Screen` because a prompt is
    /// meaningful only on the primary screen; an app on the alt screen
    /// that emits OSC 133 would still write into this slot, but the marks
    /// land on alt's grid and disappear with the alt-screen teardown.
    current_prompt_row: Option<u64>,
}

/// Safety deadline for mode 2026 synchronized updates. If an app sends BSU
/// (`CSI ? 2026 h`) but never sends ESU (because it crashed, was killed,
/// forgot the terminator, etc.) rendering resumes after this window so the
/// UI doesn't appear frozen. 150ms matches the contour-terminal spec.
const SYNCHRONIZED_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

impl Terminal {
    pub fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
        cell_height: u32,
    ) -> Self {
        Self {
            active: Screen::new(cols, rows, scrollback_limit),
            // Stash starts as a blank alt screen (no scrollback). When the
            // first ?1049h / ?47h arrives we simply swap `active` and
            // `stash` — no lazy construction needed.
            stash: Screen::new(cols, rows, 0),
            viewport: Viewport { rows, cols },
            on_alt_screen: false,
            cell_height,
            parser: vte::Parser::new(),
            next_image_id: 0,
            hook_bytes: vec![],
            hook_params: vec![],
            hook_action: vec![],
            clipboard: Clipboard::new(),
            pending_output: Vec::new(),
            mouse_tracking: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Default,
            bracketed_paste: false,
            selection: None,
            search: SearchState::new(),
            current_directory: None,
            hyperlinks: HyperlinkRegistry::new(),
            kitty_keyboard: KittyKeyboardState::new(),
            cursor_style: CursorStyle::default(),
            focus_reporting: false,
            current_title: None,
            bell_pending: false,
            synchronized_update_since: None,
            current_prompt_row: None,
        }
    }

    /// Returns `true` when the foreground app has opened a synchronized
    /// output window (mode 2026) that has not yet been closed or timed out.
    /// The host should skip rendering while this returns `true` so partial
    /// frames (e.g. mid-scroll, mid-reflow) are never presented.
    pub fn is_synchronized_update_active(&self) -> bool {
        self.synchronized_update_since
            .is_some_and(|start| start.elapsed() < SYNCHRONIZED_UPDATE_TIMEOUT)
    }

    /// Drain the bell flag. Returns `true` exactly when at least one BEL
    /// has arrived since the last call, leaving the flag cleared so the
    /// next frame starts fresh.
    pub fn take_bell_pending(&mut self) -> bool {
        std::mem::replace(&mut self.bell_pending, false)
    }

    /// Override the default cursor style. Called once at startup so the
    /// user's `config.toml` preference takes effect before any DECSCUSR
    /// arrives from the shell.
    pub fn set_default_cursor_style(
        &mut self,
        style: CursorStyle,
    ) {
        self.cursor_style = style;
    }

    /// Update the scrollback budget on the primary screen and immediately
    /// trim any history that exceeds the new cap. Trimming on update (not
    /// lazily on next push) makes the live-reload path feel responsive —
    /// the user shrinks the limit, the unwanted history goes away on the
    /// next render. The alt screen always has zero scrollback so its
    /// budget never moves.
    pub fn set_scrollback_limit(
        &mut self,
        limit: u32,
    ) {
        let primary = if self.on_alt_screen {
            &mut self.stash
        } else {
            &mut self.active
        };
        primary.grid.scrollback_limit = limit;

        let max_rows = self.viewport.rows as usize + limit as usize;
        let grid = &mut primary.grid;
        let popped_before = grid.rows.len();
        while grid.rows.len() > max_rows {
            grid.rows.pop_front();
            grid.total_popped += 1;
        }
        let popped = popped_before - grid.rows.len();
        if popped > 0 {
            // Sixel images anchored to popped rows must move with the
            // surviving rows; mirrors the same fix-up `process` does
            // after a row is pushed off the top.
            primary.images.retain(|_, img| img.row >= popped);
            for img in primary.images.values_mut() {
                img.row -= popped;
            }
        }
    }

    /// Queue a focus-in / focus-out report onto `pending_output` if focus
    /// reporting is currently enabled. Safe to call unconditionally.
    pub fn report_focus_change(
        &mut self,
        focused: bool,
    ) {
        if !self.focus_reporting {
            return;
        }
        // Per xterm: CSI I on focus gain, CSI O on focus loss.
        let payload: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
        self.pending_output.extend_from_slice(payload);
    }

    /// Translate a viewport-relative screen row to an absolute row index
    /// (stable under scrollback trimming). `screen_row` is 0 at the top of
    /// the visible area.
    fn screen_row_to_absolute(
        &self,
        screen_row: u32,
    ) -> u64 {
        let base =
            self.active.grid.rows.len() - self.viewport.rows as usize - self.active.offset as usize;
        (self.active.grid.total_popped + base + screen_row as usize) as u64
    }

    /// Convert an absolute row to an index into the grid's VecDeque.
    /// Returns None if the row has already fallen off the top of scrollback.
    fn absolute_row_to_local(
        &self,
        abs: u64,
    ) -> Option<usize> {
        let popped = self.active.grid.total_popped as u64;
        if abs < popped {
            return None;
        }
        let local = (abs - popped) as usize;
        if local >= self.active.grid.rows.len() {
            return None;
        }
        Some(local)
    }

    /// Begin a new selection rooted at `(col, screen_row)`. For Word/Line
    /// modes the anchor and head snap to word/line boundaries immediately.
    pub fn start_selection(
        &mut self,
        col: u32,
        screen_row: u32,
        mode: SelectionMode,
    ) {
        let abs_row = self.screen_row_to_absolute(screen_row);
        let Some(local) = self.absolute_row_to_local(abs_row) else {
            return;
        };
        let row = &self.active.grid.rows[local];
        let origin = SelectionPoint { row: abs_row, col };

        let (anchor, head) = match mode {
            SelectionMode::Char => (origin, origin),
            SelectionMode::Word => {
                let (s, e) = expand_to_word(row, col);
                (
                    SelectionPoint {
                        row: abs_row,
                        col: s,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: e,
                    },
                )
            }
            SelectionMode::Line => {
                let (s, e) = expand_to_line(row);
                (
                    SelectionPoint {
                        row: abs_row,
                        col: s,
                    },
                    SelectionPoint {
                        row: abs_row,
                        col: e,
                    },
                )
            }
        };
        self.selection = Some(Selection {
            anchor,
            head,
            mode,
            origin,
        });
    }

    /// Extend the current selection to `(col, screen_row)`. For Word/Line
    /// selections both the anchor and head snap to word/line boundaries so
    /// the live drag always covers whole words/lines, with the anchor
    /// flipping between the two ends of the origin segment as the drag
    /// direction changes.
    pub fn extend_selection(
        &mut self,
        col: u32,
        screen_row: u32,
    ) {
        let Some(sel) = self.selection.as_ref() else {
            return;
        };
        let mode = sel.mode;
        let origin = sel.origin;

        let abs_row = self.screen_row_to_absolute(screen_row);
        let Some(local) = self.absolute_row_to_local(abs_row) else {
            return;
        };
        let Some(origin_local) = self.absolute_row_to_local(origin.row) else {
            return;
        };

        let head_row = &self.active.grid.rows[local];
        let origin_row = &self.active.grid.rows[origin_local];

        let new_point = SelectionPoint { row: abs_row, col };
        let forward = (new_point.row, new_point.col) >= (origin.row, origin.col);

        let (anchor, head) = match mode {
            SelectionMode::Char => (origin, new_point),
            SelectionMode::Word => {
                let (o_start, o_end) = expand_to_word(origin_row, origin.col);
                let (h_start, h_end) = expand_to_word(head_row, col);
                if forward {
                    (
                        SelectionPoint {
                            row: origin.row,
                            col: o_start,
                        },
                        SelectionPoint {
                            row: abs_row,
                            col: h_end,
                        },
                    )
                } else {
                    (
                        SelectionPoint {
                            row: origin.row,
                            col: o_end,
                        },
                        SelectionPoint {
                            row: abs_row,
                            col: h_start,
                        },
                    )
                }
            }
            SelectionMode::Line => {
                let (o_start, o_end) = expand_to_line(origin_row);
                let (h_start, h_end) = expand_to_line(head_row);
                if forward {
                    (
                        SelectionPoint {
                            row: origin.row,
                            col: o_start,
                        },
                        SelectionPoint {
                            row: abs_row,
                            col: h_end,
                        },
                    )
                } else {
                    (
                        SelectionPoint {
                            row: origin.row,
                            col: o_end,
                        },
                        SelectionPoint {
                            row: abs_row,
                            col: h_start,
                        },
                    )
                }
            }
        };

        let sel = self.selection.as_mut().unwrap();
        sel.anchor = anchor;
        sel.head = head;
    }

    /// Drop the current selection. Called when a click resolves to a
    /// single cell with no drag, or after the selection has been copied.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// True when there is a selection with real content (at least one
    /// cell). Used by right-click to choose between copy and paste.
    pub fn has_selection(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Render-time query: is the given viewport cell currently highlighted?
    pub fn is_cell_selected(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> bool {
        let Some(sel) = &self.selection else {
            return false;
        };
        if sel.is_empty() {
            return false;
        }
        let abs_row = self.screen_row_to_absolute(screen_row);
        sel.contains(SelectionPoint {
            row: abs_row,
            col: screen_col,
        })
    }

    /// Open the search bar. Clears any leftover query and matches so a
    /// re-open starts from a clean slate.
    pub fn open_search(&mut self) {
        self.search.active = true;
        self.search.query.clear();
        self.search.matches.clear();
        self.search.active_idx = 0;
    }

    /// Close the search bar and drop its state. If a match was focused at
    /// close time, promote it to the active selection — users expect the
    /// hit they just navigated to to stay visibly marked (and be ready
    /// for `Ctrl+Shift+C`) once they leave the bar. When no match was
    /// focused the existing selection, if any, stays put.
    pub fn close_search(&mut self) {
        if let Some(&active) = self.search.matches.get(self.search.active_idx) {
            let anchor = SelectionPoint {
                row: active.row,
                col: active.start_col,
            };
            let head = SelectionPoint {
                row: active.row,
                col: active.end_col,
            };
            self.selection = Some(Selection {
                anchor,
                head,
                mode: SelectionMode::Char,
                origin: anchor,
            });
        }
        self.search.active = false;
        self.search.query.clear();
        self.search.matches.clear();
        self.search.active_idx = 0;
    }

    pub fn search_active(&self) -> bool {
        self.search.active
    }

    /// Read-only view of search state, for the overlay renderer. `None`
    /// when the bar isn't open — nothing for the host to draw.
    pub fn search_state(&self) -> Option<&SearchState> {
        if self.search.active {
            Some(&self.search)
        } else {
            None
        }
    }

    /// Append `s` to the current query and rescan. Intended for text input
    /// events while the bar is open — multi-byte paste is fine, control
    /// bytes and newlines aren't filtered here but the host only calls this
    /// with printable input.
    pub fn search_append(
        &mut self,
        s: &str,
    ) {
        if !self.search.active {
            return;
        }
        self.search.query.push_str(s);
        self.refresh_search();
    }

    /// Drop the last character of the query. No-op on empty query so the
    /// host doesn't have to guard the keystroke.
    pub fn search_backspace(&mut self) {
        if !self.search.active {
            return;
        }
        self.search.query.pop();
        self.refresh_search();
    }

    /// Jump to the next match, wrapping from the last back to the first.
    /// Scrolls the viewport so the new active match is visible.
    pub fn search_next(&mut self) {
        if !self.search.active || self.search.matches.is_empty() {
            return;
        }
        self.search.active_idx = (self.search.active_idx + 1) % self.search.matches.len();
        self.scroll_to_active_match();
    }

    /// Jump to the previous match, wrapping from the first to the last.
    pub fn search_prev(&mut self) {
        if !self.search.active || self.search.matches.is_empty() {
            return;
        }
        let n = self.search.matches.len();
        self.search.active_idx = (self.search.active_idx + n - 1) % n;
        self.scroll_to_active_match();
    }

    /// Render-time query: should the cell at the given viewport position
    /// be highlighted as a search match?
    pub fn is_cell_match(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> bool {
        if !self.search.active || self.search.matches.is_empty() {
            return false;
        }
        let abs_row = self.screen_row_to_absolute(screen_row);
        self.search
            .matches
            .iter()
            .any(|m| m.contains(abs_row, screen_col))
    }

    /// Render-time query: is the given viewport cell part of the *active*
    /// match — the one `search_next`/`search_prev` just landed on? The
    /// renderer paints these with a softer blend so the user can tell the
    /// focused hit apart from the other inverted matches at a glance.
    pub fn is_cell_active_match(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> bool {
        if !self.search.active {
            return false;
        }
        let Some(active) = self.search.matches.get(self.search.active_idx) else {
            return false;
        };
        let abs_row = self.screen_row_to_absolute(screen_row);
        active.contains(abs_row, screen_col)
    }

    /// Rescan the grid for the current query and, after the match list
    /// rebuilds, focus the first match at or after the current viewport —
    /// the natural place a user expects their incremental-search cursor to
    /// land. Called after every query edit.
    fn refresh_search(&mut self) {
        self.recompute_matches();
        if self.search.matches.is_empty() {
            self.search.active_idx = 0;
            return;
        }
        let viewport_top = self.screen_row_to_absolute(0);
        self.search.active_idx = self
            .search
            .matches
            .iter()
            .position(|m| m.row >= viewport_top)
            .unwrap_or(0);
        self.scroll_to_active_match();
    }

    /// Walk every row in the primary grid, concatenate its cells into a
    /// byte string, and record every `match_indices` hit as a `MatchSpan`.
    /// Matching is byte-literal so queries stay case-sensitive; wide-glyph
    /// continuation cells contribute zero bytes and drop out of the mapping
    /// naturally.
    fn recompute_matches(&mut self) {
        self.search.matches.clear();
        if self.search.query.is_empty() {
            return;
        }
        let q = self.search.query.as_str();
        let popped = self.active.grid.total_popped as u64;
        let mut text = String::new();
        let mut cell_byte_starts: Vec<usize> = Vec::new();
        for (local, row) in self.active.grid.rows.iter().enumerate() {
            text.clear();
            cell_byte_starts.clear();
            cell_byte_starts.reserve(row.cells.len());
            for cell in &row.cells {
                cell_byte_starts.push(text.len());
                text.push_str(cell);
            }
            if text.len() < q.len() {
                continue;
            }
            let abs_row = popped + local as u64;
            for (byte, _) in text.match_indices(q) {
                let start_col = cell_byte_starts
                    .partition_point(|&s| s <= byte)
                    .saturating_sub(1) as u32;
                let end_byte = byte + q.len();
                let end_col = cell_byte_starts
                    .partition_point(|&s| s < end_byte)
                    .saturating_sub(1) as u32;
                self.search.matches.push(MatchSpan {
                    row: abs_row,
                    start_col,
                    end_col,
                });
            }
        }
    }

    /// Move the viewport so the currently-focused match sits near the
    /// middle of the screen. No-op when the match has already scrolled off
    /// the front of scrollback (defensive — happens only if recompute
    /// missed a trim).
    fn scroll_to_active_match(&mut self) {
        let Some(m) = self.search.matches.get(self.search.active_idx).copied() else {
            return;
        };
        let popped = self.active.grid.total_popped as u64;
        let Some(local) = m.row.checked_sub(popped) else {
            return;
        };
        let local = local as usize;
        let grid_len = self.active.grid.rows.len();
        if local >= grid_len {
            return;
        }
        let rows = self.viewport.rows as usize;
        if grid_len <= rows {
            self.active.offset = 0;
            return;
        }
        let ideal_top = local.saturating_sub(rows / 2);
        let max_top = grid_len - rows;
        let top = ideal_top.min(max_top);
        let offset = (grid_len - rows - top) as u32;
        let max_offset = self.active.grid.scrollback_len(&self.viewport);
        self.active.offset = offset.min(max_offset);
    }

    /// Extract selection text. Trailing padding spaces on intermediate /
    /// line-mode rows are trimmed; soft-wrapped rows join without a
    /// newline, hard-wrapped ones separate with `\n`.
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        if sel.is_empty() {
            return None;
        }
        let (start, end) = sel.ordered();
        let popped = self.active.grid.total_popped as u64;
        let last_idx = self.active.grid.rows.len().saturating_sub(1);

        let mut out = String::new();
        for abs_row in start.row..=end.row {
            let local = abs_row.checked_sub(popped)? as usize;
            if local > last_idx {
                break;
            }
            let row = &self.active.grid.rows[local];
            let row_len_cols = row.cells.len() as u32;
            if row_len_cols == 0 {
                if abs_row < end.row && !row.wrapped {
                    out.push('\n');
                }
                continue;
            }

            let (col_start, col_end, trim) = match sel.mode {
                SelectionMode::Line => (0, row_len_cols - 1, true),
                _ => {
                    let is_first = abs_row == start.row;
                    let is_last = abs_row == end.row;
                    let cs = if is_first { start.col } else { 0 };
                    let ce = if is_last { end.col } else { row_len_cols - 1 };
                    let trim = !is_last;
                    (cs, ce, trim)
                }
            };
            let col_end = col_end.min(row_len_cols - 1);
            if col_start > col_end {
                if abs_row < end.row && !row.wrapped {
                    out.push('\n');
                }
                continue;
            }

            let mut segment = String::new();
            for cell in &row.cells[col_start as usize..=col_end as usize] {
                segment.push_str(cell);
            }
            if trim {
                out.push_str(segment.trim_end_matches(' '));
            } else {
                out.push_str(&segment);
            }

            if abs_row < end.row && !row.wrapped {
                out.push('\n');
            }
        }

        Some(out)
    }

    /// Copy the current selection to the given clipboard. No-op if empty.
    /// Does not clear the selection — callers that want visual feedback
    /// cleared invoke `clear_selection` explicitly.
    pub fn copy_selection(
        &mut self,
        kind: ClipboardKind,
    ) {
        if let Some(text) = self.selection_text() {
            self.clipboard.set(kind, &text);
        }
    }

    /// Queue pasted text for delivery to the PTY. When the foreground app
    /// has enabled bracketed paste (mode 2004) the text is wrapped in
    /// start/end markers so the app can distinguish it from typed input and
    /// skip auto-indent / command-execution heuristics. In either case the
    /// paste-end marker is scrubbed from the interior of the payload so a
    /// crafted clipboard can't break out of the bracket.
    pub fn paste(
        &mut self,
        text: &str,
    ) {
        const PASTE_END: &str = "\x1b[201~";
        if self.bracketed_paste {
            self.pending_output.extend_from_slice(b"\x1b[200~");
            for chunk in text.split(PASTE_END) {
                self.pending_output.extend_from_slice(chunk.as_bytes());
            }
            self.pending_output.extend_from_slice(b"\x1b[201~");
        } else {
            for chunk in text.split(PASTE_END) {
                self.pending_output.extend_from_slice(chunk.as_bytes());
            }
        }
    }

    /// Read the given selection from the system clipboard and paste it.
    /// No-op if the clipboard returned nothing (headless or empty).
    pub fn paste_from_clipboard(
        &mut self,
        kind: ClipboardKind,
    ) {
        if let Some(text) = self.clipboard.get(kind)
            && !text.is_empty()
        {
            self.paste(&text);
        }
    }

    /// Drain bytes the terminal itself has queued for the PTY (e.g. OSC 52
    /// query responses). Called by the event loop after each `process` call.
    pub fn take_pending_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_output)
    }

    /// Returns true if the app has requested any mouse tracking mode.
    pub fn mouse_tracking_enabled(&self) -> bool {
        !matches!(self.mouse_tracking, MouseTracking::Off)
    }

    /// Report a mouse event to the foreground app. Returns true if an event
    /// was emitted, false if the current tracking mode suppressed it (so the
    /// caller knows it can handle the event locally instead — e.g. for
    /// scrollback on wheel when tracking is off).
    ///
    /// `col` and `row` are 0-based cell coordinates within the viewport.
    pub fn mouse_report(
        &mut self,
        kind: MouseEventKind,
        button: MouseButton,
        col: u32,
        row: u32,
        mods: MouseModifiers,
    ) -> bool {
        if !should_report(self.mouse_tracking, kind, button) {
            return false;
        }
        encode_mouse_event(
            self.mouse_encoding,
            kind,
            button,
            col + 1,
            row + 1,
            mods,
            &mut self.pending_output,
        );
        true
    }

    /// Returns the visible row at the given screen position (0 = top of
    /// viewport).
    pub fn visible_row(
        &self,
        screen_row: u32,
    ) -> &Row {
        let base =
            self.active.grid.rows.len() - self.viewport.rows as usize - self.active.offset as usize;
        &self.active.grid.rows[base + screen_row as usize]
    }

    /// Resolve the hyperlink target at the given viewport cell, or `None`
    /// when the cell is not part of an OSC 8 span. Used by the click handler
    /// to decide whether Ctrl+click should open something.
    pub fn hyperlink_at(
        &self,
        screen_row: u32,
        screen_col: u32,
    ) -> Option<&str> {
        if screen_row >= self.viewport.rows || screen_col >= self.viewport.cols {
            return None;
        }
        let row = self.visible_row(screen_row);
        let id = row.links.get(screen_col as usize).copied().flatten()?;
        self.hyperlinks.get(id)
    }

    /// Scroll the viewport up (into history). Returns actual lines scrolled.
    pub fn scroll_viewport_up(
        &mut self,
        lines: u32,
    ) -> u32 {
        let max = self.active.grid.scrollback_len(&self.viewport);
        let delta = lines.min(max.saturating_sub(self.active.offset));
        self.active.offset += delta;
        delta
    }

    /// Move the viewport to the previous OSC 133 prompt (above the current
    /// viewport top). No-op if none exists above or the active screen has
    /// no shell-integration marks.
    pub fn scroll_to_prev_prompt(&mut self) {
        let top = self.screen_row_to_absolute(0);
        // Iterate the grid directly rather than collecting: prompt rows
        // are sparse, so walking the whole buffer once and taking the max
        // matching index is cheaper than building a vec.
        let popped = self.active.grid.total_popped as u64;
        let target = self
            .active
            .grid
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.prompt_start)
            .map(|(i, _)| popped + i as u64)
            .filter(|&r| r < top)
            .max();
        if let Some(target) = target {
            self.scroll_absolute_to_viewport_top(target);
        }
    }

    /// Move the viewport to the next OSC 133 prompt (below the current
    /// viewport top). No-op if none exists below — importantly, this
    /// includes the case where the user is at the most recent prompt, so
    /// repeated presses at the live prompt are silent rather than
    /// flickering.
    pub fn scroll_to_next_prompt(&mut self) {
        let top = self.screen_row_to_absolute(0);
        let popped = self.active.grid.total_popped as u64;
        let target = self
            .active
            .grid
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.prompt_start)
            .map(|(i, _)| popped + i as u64)
            .find(|&r| r > top);
        if let Some(target) = target {
            self.scroll_absolute_to_viewport_top(target);
        }
    }

    /// Adjust `offset` so `target_abs` lands at the top of the visible
    /// viewport. If the target sits inside the live window the viewport
    /// can't scroll further (`offset = 0`) and the target appears wherever
    /// it naturally falls — typically a few rows down from the top.
    fn scroll_absolute_to_viewport_top(
        &mut self,
        target_abs: u64,
    ) {
        let popped = self.active.grid.total_popped as u64;
        let Some(target_local) = target_abs.checked_sub(popped) else {
            return;
        };
        let grid_len = self.active.grid.rows.len();
        let rows = self.viewport.rows as usize;
        if grid_len <= rows || (target_local as usize) >= grid_len {
            self.active.offset = 0;
            return;
        }
        let max_top = grid_len - rows;
        let top = (target_local as usize).min(max_top);
        let offset = (grid_len - rows - top) as u32;
        let max_offset = self.active.grid.scrollback_len(&self.viewport);
        self.active.offset = offset.min(max_offset);
    }

    /// Scroll the viewport down (toward live). Returns actual lines scrolled.
    pub fn scroll_viewport_down(
        &mut self,
        lines: u32,
    ) -> u32 {
        let delta = lines.min(self.active.offset);
        self.active.offset -= delta;
        delta
    }

    /// Reset viewport to the bottom (live terminal).
    pub fn reset_viewport(&mut self) {
        self.active.offset = 0;
    }

    /// Return images whose row range overlaps the current viewport, with
    /// screen-relative row/col positions. `screen_row` is negative when the
    /// image's top edge is above the viewport so the renderer can offset the
    /// quad upward and let the GPU clip to the visible portion.
    pub fn visible_images(&self) -> impl Iterator<Item = VisibleImage<'_>> {
        let viewport_top =
            self.active.grid.rows.len() - self.viewport.rows as usize - self.active.offset as usize;
        let viewport_bottom = viewport_top + self.viewport.rows as usize;
        let cell_height = self.cell_height;

        self.active.images.values().filter_map(move |img| {
            let img_rows = img.image.height.div_ceil(cell_height).max(1) as usize;
            let img_bottom = img.row + img_rows;
            if img.row < viewport_bottom && img_bottom > viewport_top {
                Some(VisibleImage {
                    image: &img.image,
                    id: img.id,
                    screen_row: img.row as i32 - viewport_top as i32,
                    screen_col: img.col,
                })
            } else {
                None
            }
        })
    }

    pub fn resize(
        &mut self,
        cols: u32,
        rows: u32,
    ) {
        let old_cols = self.viewport.cols;
        let old_rows = self.viewport.rows;

        // Keep both screens sized to the new viewport so a swap after a
        // resize doesn't land the cursor outside its own grid.
        for screen in [&mut self.active, &mut self.stash] {
            resize_screen(screen, old_cols, old_rows, cols, rows);
        }

        self.viewport.cols = cols;
        self.viewport.rows = rows;
    }

    /// Process raw bytes from the PTY through the VTE parser.
    pub fn process(
        &mut self,
        data: &[u8],
    ) {
        for action in self.parser.parse(data) {
            let popped_before = self.active.grid.total_popped;

            match action {
                vte::Action::PrintAscii(run) => {
                    put_ascii_run(&mut self.active, &self.viewport, run)
                }
                vte::Action::Print(c) => put_char(&mut self.active, &self.viewport, c),
                vte::Action::Execute(byte) => {
                    if byte == 0x07 {
                        // BEL: surface to the host. The parser already
                        // swallows BEL inside execute(), but routing it
                        // here lets us notify the windowing layer (urgent
                        // hint, visual flash) without coupling the
                        // Screen module to that decision.
                        self.bell_pending = true;
                    } else {
                        execute(&mut self.active, &self.viewport, byte);
                    }
                }
                vte::Action::CsiDispatch {
                    params,
                    intermediates,
                    action,
                } => {
                    let is = intermediates.as_slice();
                    if is == b"?" && (action == 'h' || action == 'l') {
                        let enable = action == 'h';
                        for p in params.iter() {
                            if p[0] == 2004 {
                                self.bracketed_paste = enable;
                            } else if p[0] == 1004 {
                                self.focus_reporting = enable;
                            } else if p[0] == 2026 {
                                // BSU refreshes the deadline; ESU clears it.
                                // Refreshing on a nested BSU matches the
                                // contour spec's "keep the window open" rule
                                // for apps that chain updates.
                                self.synchronized_update_since = enable.then(Instant::now);
                            } else if !apply_mouse_mode(
                                p[0],
                                enable,
                                &mut self.mouse_tracking,
                                &mut self.mouse_encoding,
                            ) {
                                set_private_mode(
                                    p[0],
                                    enable,
                                    &mut self.active,
                                    &mut self.stash,
                                    &self.viewport,
                                    &mut self.on_alt_screen,
                                );
                            }
                        }
                    } else if action == 'u' && matches!(is, b">" | b"<" | b"=" | b"?") {
                        handle_kitty_keyboard(
                            is[0],
                            &params,
                            &mut self.kitty_keyboard,
                            &mut self.pending_output,
                        );
                    } else if action == 'q' && is == b" " {
                        // DECSCUSR. The space intermediate is mandatory; the
                        // single param picks shape+blink (0/1=blink block,
                        // 2=block, 3/4=underline, 5/6=beam).
                        let ps = params
                            .iter()
                            .next()
                            .and_then(|g| g.first().copied())
                            .unwrap_or(0);
                        self.cursor_style.apply_decscusr(ps);
                    } else {
                        csi_dispatch(&mut self.active, &self.viewport, &params, is, action);
                    }
                }
                vte::Action::EscDispatch {
                    intermediates,
                    byte,
                } => {
                    let is = intermediates.as_slice();
                    if is.is_empty() && byte == b'7' {
                        save_cursor_slot(&mut self.active);
                    } else if is.is_empty() && byte == b'8' {
                        restore_cursor_slot(&mut self.active, &self.viewport);
                    } else {
                        esc_dispatch(&mut self.active, &self.viewport, is, byte);
                    }
                }
                vte::Action::OscDispatch(data) => {
                    let mut ctx = OscContext {
                        clipboard: &mut self.clipboard,
                        pending_output: &mut self.pending_output,
                        current_directory: &mut self.current_directory,
                        hyperlinks: &mut self.hyperlinks,
                        active_screen: &mut self.active,
                        viewport: &self.viewport,
                        current_title: &mut self.current_title,
                        current_prompt_row: &mut self.current_prompt_row,
                    };
                    handle_osc(&data, &mut ctx);
                }
                vte::Action::Hook { params, action } => {
                    self.hook_bytes.push(vec![]);
                    self.hook_params.push(params);
                    self.hook_action.push(action);
                }
                vte::Action::Put(bytes) => {
                    if let Some(last) = self.hook_bytes.last_mut() {
                        last.extend_from_slice(bytes);
                    }
                }
                vte::Action::Unhook => {
                    let bytes = self.hook_bytes.pop().unwrap();
                    let params = self.hook_params.pop().unwrap();
                    let action = self.hook_action.pop().unwrap();
                    if action == 'q' {
                        let image = parse_sixel(params, bytes);
                        let id = self.next_image_id;
                        self.next_image_id += 1;
                        let row = self
                            .active
                            .grid
                            .active_row_index(&self.active.cursor, &self.viewport);
                        let image_rows = image.height.div_ceil(self.cell_height);
                        // An app redrawing a sixel places the new image over
                        // the old one. Without this sweep the fresh `id`
                        // leaves the previous entry in the map, so each
                        // redraw adds a ghost at the old position.
                        crate::terminal::image::remove_overlapping(
                            &mut self.active.images,
                            row,
                            image_rows.max(1) as usize,
                            self.active.cursor.col,
                            self.cell_height,
                        );
                        self.active.images.insert(
                            id,
                            PlacedImage {
                                image,
                                id,
                                row,
                                col: self.active.cursor.col,
                            },
                        );

                        // Advance cursor past the image, scrolling as needed.
                        for _ in 0..image_rows {
                            self.active.cursor.row += 1;
                            if self.active.cursor.row >= self.viewport.rows {
                                self.active.grid.push_visible_row(&self.viewport);
                                self.active.cursor.row = self.viewport.rows - 1;
                            }
                        }
                        self.active.cursor.col = 0;
                        self.active.offset = 0;
                    }
                }
            }

            // Use saturating_sub: a screen swap during this iteration can
            // reset `total_popped` to the other grid's value, which would
            // underflow an unchecked subtraction.
            let newly_popped = self.active.grid.total_popped.saturating_sub(popped_before);
            if newly_popped > 0 {
                self.active.images.retain(|_, img| img.row >= newly_popped);
                for img in self.active.images.values_mut() {
                    img.row -= newly_popped;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible_text(term: &Terminal) -> String {
        let mut s = String::new();
        for r in 0..term.viewport.rows {
            let row = term.visible_row(r);
            for cell in &row.cells {
                s.push_str(cell);
            }
            s.push('\n');
        }
        s
    }

    /// Like [`visible_text`] but with row boundaries removed, so assertions
    /// can match logical content that crossed a soft-wrap.
    fn visible_text_flat(term: &Terminal) -> String {
        visible_text(term).replace('\n', "")
    }

    #[test]
    fn alt_screen_1049_hides_primary_and_restores() {
        let mut term = Terminal::new(8, 4, 100, 16);
        term.process(b"hello");
        term.process(b"\x1b[?1049h");

        // Alt is active, blank, cursor at (0,0).
        assert!(term.on_alt_screen);
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        assert!(
            !visible_text(&term).contains("hello"),
            "alt screen should be blank, got {:?}",
            visible_text(&term)
        );

        term.process(b"WORLD");
        assert!(visible_text(&term).contains("WORLD"));

        term.process(b"\x1b[?1049l");

        // Back on primary with saved cursor restored and original content visible.
        assert!(!term.on_alt_screen);
        assert!(visible_text(&term).contains("hello"));
        assert_eq!(term.active.cursor.col, 5);
        assert_eq!(term.active.cursor.row, 0);
    }

    #[test]
    fn alt_screen_1049_resize_preserves_primary() {
        let mut term = Terminal::new(10, 4, 100, 16);
        term.process(b"primary-content");
        term.process(b"\x1b[?1049h");
        term.process(b"ALT");

        // Resize while on alt — primary must survive with its content.
        term.resize(12, 5);
        term.process(b"\x1b[?1049l");

        // After reflow, the primary text may straddle a soft-wrap boundary.
        let flat = visible_text_flat(&term);
        assert!(
            flat.contains("primary-content"),
            "primary content lost through resize: {:?}",
            flat
        );
        assert_eq!(term.viewport.cols, 12);
        assert_eq!(term.viewport.rows, 5);
    }

    #[test]
    fn alt_screen_has_no_scrollback() {
        let mut term = Terminal::new(8, 3, 100, 16);
        term.process(b"\x1b[?1049h");

        // Fill enough rows on alt to normally produce scrollback on primary.
        for _ in 0..10 {
            term.process(b"line\n");
        }
        assert_eq!(term.active.grid.scrollback_len(&term.viewport), 0);
    }

    #[test]
    fn decsc_decrc_restores_cursor_and_colors() {
        let mut term = Terminal::new(10, 4, 100, 16);
        term.process(b"\x1b[3;5H"); // move to row 3 col 5
        term.process(b"\x1b[31m"); // red fg
        term.process(b"\x1b7"); // DECSC
        let saved_fg = term.active.fg;
        term.process(b"\x1b[1;1H\x1b[32m"); // move + change color
        term.process(b"\x1b8"); // DECRC

        assert_eq!(term.active.cursor.row, 2);
        assert_eq!(term.active.cursor.col, 4);
        assert_eq!(term.active.fg, saved_fg);
    }

    #[test]
    fn mode_47_does_not_save_cursor() {
        let mut term = Terminal::new(8, 3, 100, 16);
        term.process(b"\x1b[2;3H"); // row 2 col 3
        term.process(b"\x1b[?47h");
        term.process(b"\x1b[1;1H"); // move on alt
        term.process(b"\x1b[?47l");

        // ?47 doesn't save/restore cursor — we land wherever we left primary.
        // Primary's cursor before the switch was (row=1, col=2); ?47 preserves
        // the *primary screen's* cursor (untouched because we swapped away
        // before moving), so we should be back at (1,2).
        assert_eq!(term.active.cursor.row, 1);
        assert_eq!(term.active.cursor.col, 2);
    }

    #[test]
    fn decset_1006_switches_to_sgr_encoding() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?1006h");
        assert_eq!(term.mouse_encoding, MouseEncoding::Sgr);
        term.process(b"\x1b[?1006l");
        assert_eq!(term.mouse_encoding, MouseEncoding::Default);
    }

    #[test]
    fn decset_1002_enables_button_event_tracking() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?1002h");
        assert_eq!(term.mouse_tracking, MouseTracking::ButtonEvent);
        term.process(b"\x1b[?1002l");
        assert_eq!(term.mouse_tracking, MouseTracking::Off);
    }

    #[test]
    fn tracking_mode_is_replaced_not_layered() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?1000h");
        term.process(b"\x1b[?1003h");
        assert_eq!(term.mouse_tracking, MouseTracking::AnyEvent);
    }

    #[test]
    fn mouse_report_emits_into_pending_output() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?1000h\x1b[?1006h");
        let emitted = term.mouse_report(
            MouseEventKind::Press,
            MouseButton::Left,
            4,
            9,
            MouseModifiers::default(),
        );
        assert!(emitted);
        // Coordinates pushed are 1-based.
        assert_eq!(term.take_pending_output(), b"\x1b[<0;5;10M");
    }

    #[test]
    fn mouse_report_returns_false_when_tracking_off() {
        let mut term = Terminal::new(80, 24, 100, 16);
        let emitted = term.mouse_report(
            MouseEventKind::Press,
            MouseButton::Left,
            0,
            0,
            MouseModifiers::default(),
        );
        assert!(!emitted);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Bracketed paste (mode 2004) ----

    #[test]
    fn paste_default_is_raw() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.paste("hello\n");
        assert_eq!(term.take_pending_output(), b"hello\n");
    }

    #[test]
    fn paste_wraps_when_mode_2004_enabled() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?2004h");
        assert!(term.bracketed_paste);
        term.paste("hello\n");
        assert_eq!(term.take_pending_output(), b"\x1b[200~hello\n\x1b[201~");
    }

    #[test]
    fn decrst_2004_disables_bracketed_paste() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?2004h");
        term.process(b"\x1b[?2004l");
        assert!(!term.bracketed_paste);
        term.paste("hi");
        assert_eq!(term.take_pending_output(), b"hi");
    }

    #[test]
    fn paste_scrubs_embedded_end_marker() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?2004h");
        // The clipboard tries to break out of the bracket — the injected
        // `\x1b[201~` is stripped and everything else comes through.
        term.paste("evil\x1b[201~injection");
        assert_eq!(
            term.take_pending_output(),
            b"\x1b[200~evilinjection\x1b[201~"
        );
    }

    // ---- Sixel image placement ----

    fn place_image(
        term: &mut Terminal,
        row: usize,
        col: u32,
        height_px: u32,
    ) -> u64 {
        let id = term.next_image_id;
        term.next_image_id += 1;
        term.active.images.insert(
            id,
            PlacedImage {
                image: crate::sixel::SixelImage {
                    pixels: vec![],
                    width: 1,
                    height: height_px,
                },
                id,
                row,
                col,
            },
        );
        id
    }

    #[test]
    fn sixel_redraw_at_same_position_replaces_previous() {
        let mut term = Terminal::new(80, 24, 100, 16);
        // cell_height = 16, so 32px = 2 grid rows.
        let id_a = place_image(&mut term, 5, 0, 32);
        self::image::remove_overlapping(&mut term.active.images, 5, 2, 0, 16);
        // The manual sweep used by the Unhook handler — call it to verify
        // the behavior the handler relies on.
        assert!(!term.active.images.contains_key(&id_a));
    }

    #[test]
    fn sixel_different_columns_coexist() {
        let mut term = Terminal::new(80, 24, 100, 16);
        let id_a = place_image(&mut term, 5, 0, 32);
        let id_b = place_image(&mut term, 5, 10, 32);
        // Dedup sweep for a new image at col 0 must not touch col 10.
        self::image::remove_overlapping(&mut term.active.images, 5, 2, 0, 16);
        assert!(!term.active.images.contains_key(&id_a));
        assert!(term.active.images.contains_key(&id_b));
    }

    #[test]
    fn scroll_region_shifts_images_up() {
        let mut term = Terminal::new(10, 10, 0, 16);
        // Set scroll region rows 0..=9 (whole screen is the region when
        // we use DECSTBM with a custom bottom). Place image at absolute
        // row 5, then issue CSI M (delete line) from row 0 to scroll the
        // region up by 2.
        term.process(b"\x1b[1;8r"); // DECSTBM top=1, bottom=8 (0-indexed: 0..=7)
        let id = place_image(&mut term, 5, 0, 16);
        term.process(b"\x1b[H"); // cursor home
        term.process(b"\x1b[2M"); // delete 2 lines → scroll_up_in_region n=2
        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 3, "image should shift up by 2 rows");
    }

    #[test]
    fn scroll_region_drops_image_pushed_out_of_top() {
        let mut term = Terminal::new(10, 10, 0, 16);
        term.process(b"\x1b[1;8r");
        let id = place_image(&mut term, 2, 0, 16);
        term.process(b"\x1b[H");
        term.process(b"\x1b[5M"); // 5 > available space above → image goes past top
        assert!(
            !term.active.images.contains_key(&id),
            "image scrolled past region top should be dropped"
        );
    }

    #[test]
    fn scroll_region_preserves_images_outside_region() {
        let mut term = Terminal::new(10, 10, 0, 16);
        term.process(b"\x1b[2;5r"); // region rows 1..=4 (abs 1..=4 with no scrollback)
        let id = place_image(&mut term, 8, 0, 16); // below region
        term.process(b"\x1b[2H"); // move into region
        term.process(b"\x1b[2M"); // scroll up inside region
        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 8, "image outside region is unaffected");
    }

    #[test]
    fn ed_2_removes_visible_images() {
        let mut term = Terminal::new(10, 10, 0, 16);
        let id = place_image(&mut term, 3, 0, 16);
        term.process(b"\x1b[2J"); // ED 2 — clear entire screen
        assert!(
            !term.active.images.contains_key(&id),
            "ED 2 should drop images on the visible area"
        );
    }

    #[test]
    fn alt_screen_entry_clears_alt_images() {
        let mut term = Terminal::new(10, 10, 0, 16);
        // Enter alt once and place an image on the alt buffer.
        term.process(b"\x1b[?1049h");
        assert!(term.on_alt_screen);
        let id = place_image(&mut term, 3, 0, 16);
        // Leave alt — clear_visible should drop the alt's image.
        term.process(b"\x1b[?1049l");
        assert!(!term.on_alt_screen);
        // Re-enter alt; the alt buffer (now `active` again) must not
        // have the old image.
        term.process(b"\x1b[?1049h");
        assert!(!term.active.images.contains_key(&id));
    }

    // ---- Synchronized output (mode 2026) ----

    #[test]
    fn bsu_sets_synchronized_update_flag() {
        let mut term = Terminal::new(80, 24, 100, 16);
        assert!(!term.is_synchronized_update_active());
        term.process(b"\x1b[?2026h");
        assert!(term.is_synchronized_update_active());
    }

    #[test]
    fn esu_clears_synchronized_update_flag() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?2026h");
        term.process(b"\x1b[?2026l");
        assert!(!term.is_synchronized_update_active());
        assert!(term.synchronized_update_since.is_none());
    }

    #[test]
    fn synchronized_update_expires_after_timeout() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.process(b"\x1b[?2026h");
        // Back-date the start so the safety deadline has already passed —
        // avoids a real sleep in the test but exercises the timeout path.
        term.synchronized_update_since =
            Some(Instant::now() - SYNCHRONIZED_UPDATE_TIMEOUT - Duration::from_millis(1));
        assert!(!term.is_synchronized_update_active());
    }

    #[test]
    fn paste_from_clipboard_round_trips() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.clipboard = Clipboard::in_memory();
        term.clipboard.set(ClipboardKind::Clipboard, "hello");
        term.paste_from_clipboard(ClipboardKind::Clipboard);
        assert_eq!(term.take_pending_output(), b"hello");
    }

    #[test]
    fn paste_from_clipboard_ignores_empty_selection() {
        let mut term = Terminal::new(80, 24, 100, 16);
        term.clipboard = Clipboard::in_memory();
        term.paste_from_clipboard(ClipboardKind::Clipboard);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Selection ----

    fn write_row(
        term: &mut Terminal,
        screen_row: u32,
        text: &str,
    ) {
        term.process(format!("\x1b[{};1H", screen_row + 1).as_bytes());
        term.process(text.as_bytes());
    }

    #[test]
    fn start_selection_char_mode_is_empty_initially() {
        let mut term = Terminal::new(10, 3, 100, 16);
        term.start_selection(2, 1, SelectionMode::Char);
        assert!(term.selection.is_some());
        assert!(!term.has_selection()); // empty Char = not "has selection"
    }

    #[test]
    fn char_selection_extend_produces_text() {
        let mut term = Terminal::new(10, 3, 100, 16);
        write_row(&mut term, 0, "hello");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(4, 0);
        assert_eq!(term.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn word_selection_snaps_to_boundaries() {
        let mut term = Terminal::new(20, 3, 100, 16);
        write_row(&mut term, 0, "hello world");
        term.start_selection(2, 0, SelectionMode::Word); // in "hello"
        assert_eq!(term.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn line_selection_covers_full_row() {
        let mut term = Terminal::new(20, 3, 100, 16);
        write_row(&mut term, 0, "hello world");
        term.start_selection(5, 0, SelectionMode::Line);
        // Line selection trims trailing padding spaces.
        assert_eq!(term.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn selection_spans_rows_with_newline_separator() {
        let mut term = Terminal::new(10, 3, 100, 16);
        write_row(&mut term, 0, "abc");
        write_row(&mut term, 1, "def");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(2, 1);
        // Intermediate row trims trailing spaces, \n joins hard line breaks.
        assert_eq!(term.selection_text().as_deref(), Some("abc\ndef"));
    }

    #[test]
    fn selection_drags_backwards_flips_anchor_head() {
        let mut term = Terminal::new(20, 3, 100, 16);
        write_row(&mut term, 0, "hello world");
        term.start_selection(8, 0, SelectionMode::Word); // in "world"
        term.extend_selection(2, 0); // drag back into "hello"
        assert_eq!(term.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn is_cell_selected_matches_contains() {
        let mut term = Terminal::new(10, 3, 100, 16);
        write_row(&mut term, 0, "abcdefghij");
        term.start_selection(2, 0, SelectionMode::Char);
        term.extend_selection(5, 0);
        assert!(!term.is_cell_selected(0, 1));
        assert!(term.is_cell_selected(0, 2));
        assert!(term.is_cell_selected(0, 5));
        assert!(!term.is_cell_selected(0, 6));
        assert!(!term.is_cell_selected(1, 3));
    }

    #[test]
    fn search_finds_exact_case_sensitive_matches() {
        let mut term = Terminal::new(20, 4, 100, 16);
        write_row(&mut term, 0, "abc foo xyz FOO bar");
        term.open_search();
        assert!(term.search_active());
        term.search_append("foo");
        // Only the lowercase occurrence matches.
        assert_eq!(term.search.matches.len(), 1);
        let m = term.search.matches[0];
        assert_eq!((m.start_col, m.end_col), (4, 6));
        assert!(term.is_cell_match(0, 4));
        assert!(term.is_cell_match(0, 5));
        assert!(term.is_cell_match(0, 6));
        assert!(!term.is_cell_match(0, 3));
        assert!(!term.is_cell_match(0, 7));
        // The uppercase run must stay un-highlighted.
        assert!(!term.is_cell_match(0, 12));
    }

    #[test]
    fn search_close_clears_state() {
        let mut term = Terminal::new(20, 4, 100, 16);
        write_row(&mut term, 0, "hello");
        term.open_search();
        term.search_append("hello");
        assert_eq!(term.search.matches.len(), 1);
        term.close_search();
        assert!(!term.search_active());
        assert!(term.search.matches.is_empty());
        assert!(term.search.query.is_empty());
    }

    #[test]
    fn search_close_promotes_active_match_to_selection() {
        let mut term = Terminal::new(20, 4, 100, 16);
        write_row(&mut term, 0, "abc foo def");
        term.open_search();
        term.search_append("foo");
        term.close_search();
        // Selection now covers the match columns 4..=6.
        assert!(term.is_cell_selected(0, 4));
        assert!(term.is_cell_selected(0, 5));
        assert!(term.is_cell_selected(0, 6));
        assert!(!term.is_cell_selected(0, 3));
        assert!(!term.is_cell_selected(0, 7));
    }

    #[test]
    fn search_close_without_matches_leaves_prior_selection() {
        let mut term = Terminal::new(20, 4, 100, 16);
        write_row(&mut term, 0, "hello world");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(4, 0);
        assert!(term.has_selection());
        term.open_search();
        term.search_append("zzz"); // no match
        term.close_search();
        // Pre-existing selection must still be intact.
        assert!(term.is_cell_selected(0, 0));
        assert!(term.is_cell_selected(0, 4));
    }

    #[test]
    fn search_next_wraps_around() {
        let mut term = Terminal::new(20, 4, 100, 16);
        write_row(&mut term, 0, "foo");
        write_row(&mut term, 1, "foo");
        write_row(&mut term, 2, "foo");
        term.open_search();
        term.search_append("foo");
        assert_eq!(term.search.matches.len(), 3);
        let start_idx = term.search.active_idx;
        term.search_next();
        term.search_next();
        term.search_next();
        // Three steps from start returns to start.
        assert_eq!(term.search.active_idx, start_idx);
    }

    #[test]
    fn search_backspace_trims_query_and_rescans() {
        let mut term = Terminal::new(20, 4, 100, 16);
        write_row(&mut term, 0, "fox foxy fo");
        term.open_search();
        term.search_append("foxy");
        assert_eq!(term.search.matches.len(), 1);
        term.search_backspace(); // query is now "fox"
        // "fox" hits both "fox" and the start of "foxy".
        assert_eq!(term.search.matches.len(), 2);
    }

    #[test]
    fn copy_selection_writes_to_clipboard() {
        let mut term = Terminal::new(10, 3, 100, 16);
        term.clipboard = Clipboard::in_memory();
        write_row(&mut term, 0, "copy-me");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(6, 0);
        term.copy_selection(ClipboardKind::Clipboard);
        assert_eq!(
            term.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("copy-me")
        );
        // Selection survives copy (callers clear explicitly).
        assert!(term.has_selection());
    }

    #[test]
    fn clear_selection_drops_state() {
        let mut term = Terminal::new(10, 3, 100, 16);
        write_row(&mut term, 0, "hello");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(4, 0);
        term.clear_selection();
        assert!(term.selection.is_none());
        assert!(term.selection_text().is_none());
    }

    // ---- OSC 7 cwd ----

    #[test]
    fn osc_7_updates_terminal_cwd() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b]7;file://localhost/tmp/work\x1b\\");
        assert_eq!(
            term.current_directory.as_deref(),
            Some(std::path::Path::new("/tmp/work"))
        );
    }

    // ---- OSC 8 hyperlinks ----

    #[test]
    fn osc_8_attaches_link_to_subsequent_cells() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\after");
        assert_eq!(term.hyperlink_at(0, 0), Some("https://example.com"));
        assert_eq!(term.hyperlink_at(0, 3), Some("https://example.com"));
        // First cell after the closing OSC 8 carries no link.
        assert_eq!(term.hyperlink_at(0, 4), None);
    }

    #[test]
    fn osc_8_close_clears_current_link() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b]8;;https://example.com\x1b\\");
        assert!(term.active.current_hyperlink.is_some());
        term.process(b"\x1b]8;;\x1b\\");
        assert!(term.active.current_hyperlink.is_none());
    }

    // ---- Kitty keyboard protocol ----

    #[test]
    fn kitty_push_records_flags() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[>1u");
        assert_eq!(
            term.kitty_keyboard.current(),
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
        );
    }

    #[test]
    fn kitty_pop_default_unwinds_one_frame() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[>1u\x1b[<u");
        assert!(term.kitty_keyboard.current().is_empty());
    }

    #[test]
    fn kitty_query_writes_response_to_pending_output() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[>3u\x1b[?u");
        assert_eq!(term.take_pending_output(), b"\x1b[?3u");
    }

    // ---- Cursor style (DECSCUSR) ----

    #[test]
    fn decscusr_sets_steady_block() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[2 q");
        assert_eq!(
            term.cursor_style,
            CursorStyle {
                shape: CursorShape::Block,
                blink: false,
            }
        );
    }

    #[test]
    fn decscusr_sets_blinking_beam() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[5 q");
        assert_eq!(
            term.cursor_style,
            CursorStyle {
                shape: CursorShape::Beam,
                blink: true,
            }
        );
    }

    #[test]
    fn config_default_cursor_style_overrides_xterm_default() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.set_default_cursor_style(CursorStyle {
            shape: CursorShape::Underline,
            blink: false,
        });
        assert_eq!(term.cursor_style.shape, CursorShape::Underline);
        assert!(!term.cursor_style.blink);
    }

    // ---- Focus reporting (?1004) ----

    #[test]
    fn focus_change_silent_when_reporting_disabled() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.report_focus_change(true);
        assert!(term.take_pending_output().is_empty());
    }

    #[test]
    fn focus_change_emits_csi_i_o_when_enabled() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[?1004h");
        term.report_focus_change(true);
        term.report_focus_change(false);
        assert_eq!(term.take_pending_output(), b"\x1b[I\x1b[O");
    }

    #[test]
    fn decrst_1004_disables_focus_reporting() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b[?1004h\x1b[?1004l");
        term.report_focus_change(true);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Live config reload effects ----

    // ---- Title (OSC 0 / OSC 2) ----

    #[test]
    fn osc_2_updates_terminal_title() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b]2;build ok\x1b\\");
        assert_eq!(term.current_title.as_deref(), Some("build ok"));
    }

    #[test]
    fn osc_0_updates_terminal_title() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x1b]0;hi\x1b\\");
        assert_eq!(term.current_title.as_deref(), Some("hi"));
    }

    // ---- Bell ----

    #[test]
    fn bel_byte_sets_bell_pending() {
        let mut term = Terminal::new(20, 3, 100, 16);
        assert!(!term.take_bell_pending());
        term.process(b"\x07");
        assert!(term.take_bell_pending());
        // Take is destructive — second poll within the same frame returns false.
        assert!(!term.take_bell_pending());
    }

    #[test]
    fn bel_inside_text_is_caught() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"hi\x07there");
        assert!(term.take_bell_pending());
    }

    #[test]
    fn bel_does_not_advance_cursor() {
        let mut term = Terminal::new(20, 3, 100, 16);
        term.process(b"\x07");
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.cursor.row, 0);
    }

    // ---- Live config reload ----

    #[test]
    fn set_scrollback_limit_takes_effect_on_next_push() {
        let mut term = Terminal::new(8, 2, 100, 16);
        // Burn through enough lines to trigger trim-on-push later.
        for i in 0..50u32 {
            term.process(format!("line{i}\n").as_bytes());
        }
        term.set_scrollback_limit(5);
        // Push more lines so the per-row trim path runs against the new limit.
        for i in 0..20u32 {
            term.process(format!("after{i}\n").as_bytes());
        }
        // Visible rows + scrollback budget caps total grid rows.
        let max_expected = term.viewport.rows as usize + 5;
        assert!(
            term.active.grid.rows.len() <= max_expected,
            "grid kept {} rows after lowering limit to 5 (max {})",
            term.active.grid.rows.len(),
            max_expected,
        );
    }

    // ---- OSC 133 shell integration + prompt navigation ----

    /// Drive a scripted shell session that emits OSC 133 marks into the
    /// terminal, producing enough rows to land some prompts in scrollback.
    /// Each invocation simulates one prompt + one command.
    fn emit_prompt(
        term: &mut Terminal,
        label: &str,
        output_lines: u32,
        exit: i32,
    ) {
        term.process(b"\x1b]133;A\x1b\\");
        term.process(label.as_bytes());
        term.process(b"\x1b]133;B\x1b\\");
        term.process(b"\n\x1b]133;C\x1b\\");
        for i in 0..output_lines {
            term.process(format!("out{i}\n").as_bytes());
        }
        term.process(format!("\x1b]133;D;{exit}\x1b\\").as_bytes());
    }

    #[test]
    fn osc_133_stamps_exit_status_onto_prompt_row_through_process() {
        let mut term = Terminal::new(10, 6, 100, 16);
        emit_prompt(&mut term, "$ ls", 1, 0);
        // Prompt landed on row 0 (the first row written to). Exit status
        // should be stamped there, not on the D row further down.
        let prompt_row = &term.active.grid.rows[0];
        assert!(prompt_row.prompt_start);
        assert_eq!(prompt_row.exit_status, Some(0));
    }

    #[test]
    fn osc_133_exit_status_survives_scrollback_pop() {
        // Small viewport so prompts quickly move into scrollback.
        let mut term = Terminal::new(10, 3, 100, 16);
        emit_prompt(&mut term, "$ first", 2, 0);
        emit_prompt(&mut term, "$ second", 2, 1);
        // Both prompt rows are now somewhere in scrollback; find the
        // first one and verify its exit status.
        let first = term
            .active
            .grid
            .rows
            .iter()
            .find(|r| r.prompt_start)
            .expect("first prompt row survived");
        assert_eq!(first.exit_status, Some(0));
    }

    #[test]
    fn scroll_to_prev_prompt_moves_viewport() {
        let mut term = Terminal::new(10, 4, 200, 16);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        // Starts at live (offset = 0). Prev should scroll back to an
        // earlier prompt.
        let before = term.active.offset;
        term.scroll_to_prev_prompt();
        assert!(
            term.active.offset > before,
            "prev should scroll the viewport into history"
        );
    }

    #[test]
    fn scroll_to_prev_prompt_silent_with_no_marks() {
        let mut term = Terminal::new(10, 4, 100, 16);
        term.process(b"plain\noutput\nwithout\nshell integration\n");
        let before = term.active.offset;
        term.scroll_to_prev_prompt();
        assert_eq!(
            term.active.offset, before,
            "no marks → offset must not change"
        );
    }

    #[test]
    fn scroll_to_next_prompt_walks_forward() {
        let mut term = Terminal::new(10, 4, 200, 16);
        emit_prompt(&mut term, "$ a", 3, 0);
        emit_prompt(&mut term, "$ b", 3, 0);
        emit_prompt(&mut term, "$ c", 3, 0);
        // Scroll all the way back, then walk forward.
        term.active.offset = term.active.grid.scrollback_len(&term.viewport);
        let start = term.active.offset;
        term.scroll_to_next_prompt();
        assert!(
            term.active.offset < start,
            "next should move the viewport toward live"
        );
    }

    #[test]
    fn scroll_to_next_prompt_silent_at_last_prompt() {
        let mut term = Terminal::new(10, 4, 200, 16);
        emit_prompt(&mut term, "$ only", 3, 0);
        // At live there's no next prompt — repeated presses shouldn't
        // bounce the viewport.
        let before = term.active.offset;
        term.scroll_to_next_prompt();
        assert_eq!(term.active.offset, before);
    }

    #[test]
    fn prompt_marks_ride_reflow_shrink_then_grow() {
        // 20-col viewport, prompt + long command that will soft-wrap when
        // shrunk. After a shrink/grow round-trip the mark must end up
        // exactly once, on the head of the (re-merged) logical line.
        let mut term = Terminal::new(20, 6, 100, 16);
        term.process(b"\x1b]133;A\x1b\\");
        term.process(b"$ this is a long prompt line");
        term.process(b"\x1b]133;B\x1b\\\n");
        term.process(b"\x1b]133;D;0\x1b\\");

        term.resize(8, 6); // forces soft-wrap
        term.resize(20, 6); // re-merge

        let prompt_rows: Vec<_> = term
            .active
            .grid
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.prompt_start)
            .collect();
        assert_eq!(
            prompt_rows.len(),
            1,
            "exactly one prompt mark after reflow round-trip, got {}: {:#?}",
            prompt_rows.len(),
            prompt_rows
                .iter()
                .map(|(i, r)| (i, r.cells.iter().map(|c| c.as_str()).collect::<String>()))
                .collect::<Vec<_>>()
        );
        // Exit status rode along with the prompt mark.
        assert_eq!(prompt_rows[0].1.exit_status, Some(0));
    }

    #[test]
    fn prompt_marks_do_not_duplicate_on_continuation_rows() {
        // After a shrink, marks must live only on the *head* of each
        // logical line — the row that is either the first row or comes
        // right after a row whose `wrapped` flag is false. (`wrapped=true`
        // means "this row spills into the next one", so the head of a
        // soft-wrapped logical line is the one with `wrapped=true` whose
        // predecessor has `wrapped=false`.)
        let mut term = Terminal::new(20, 6, 100, 16);
        term.process(b"\x1b]133;A\x1b\\");
        term.process(b"$ a command that will definitely wrap");
        term.process(b"\x1b]133;B\x1b\\\n");

        term.resize(8, 6);

        for i in 0..term.active.grid.rows.len() {
            let is_head = i == 0 || !term.active.grid.rows[i - 1].wrapped;
            if !is_head {
                let row = &term.active.grid.rows[i];
                assert!(
                    !row.prompt_start,
                    "continuation row {i} unexpectedly carries prompt_start"
                );
                assert!(
                    !row.output_start,
                    "continuation row {i} unexpectedly carries output_start"
                );
            }
        }
    }

    #[test]
    fn row_clear_drops_marks() {
        let mut term = Terminal::new(10, 4, 100, 16);
        emit_prompt(&mut term, "$ cmd", 1, 0);
        // ED 2 wipes the entire visible area — including all rows' marks.
        term.process(b"\x1b[2J");
        let any_marks = term
            .active
            .grid
            .rows
            .iter()
            .rev()
            .take(term.viewport.rows as usize)
            .any(|r| r.prompt_start || r.output_start || r.exit_status.is_some());
        assert!(!any_marks, "ED 2 must drop marks on visible rows");
    }
}

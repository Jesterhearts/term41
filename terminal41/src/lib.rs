#![allow(clippy::too_many_arguments)]

mod charset;
mod color;
mod conformance;
mod cursor;
mod grid;
mod hyperlink;
mod image;
mod keyboard;
mod mode;
mod mouse;
mod osc;
mod parser;
mod row;
mod screen;
mod search;
pub mod selection;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use clip41::Clipboard;
use clip41::ClipboardKind;
use pty_pipe41::MAX_READ_CHUNK;
use pty_pipe41::PtyReader;
use vtepp::Action;

pub use self::color::ColorPalette;
pub use self::conformance::C1Mode;
pub use self::conformance::ConformanceLevel;
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
use self::mouse::encode_mouse_event;
use self::mouse::should_report;
use self::osc::OscContext;
use self::osc::handle_osc;
use self::parser::CsiContext;
use self::parser::EscContext;
use self::parser::csi_dispatch;
use self::parser::esc_dispatch;
use self::parser::execute;
use self::parser::put_ascii_run;
use self::parser::put_char;
use self::parser::put_text_run;
pub use self::row::LineAttr;
pub use self::row::Row;
pub use self::screen::Screen;
use self::screen::resize_screen;
use crate::search::MatchSpan;
use crate::search::SearchState;
use crate::selection::Selection;
use crate::selection::SelectionMode;
use crate::selection::SelectionPoint;
use crate::selection::expand_to_line;
use crate::selection::expand_to_word;

/// Per-prompt metadata recorded from OSC 133 B/C/D sequences. Keyed by
/// the absolute row of the prompt (`A` mark) in
/// [`Terminal::command_metas`]. Enables command selection, rerun, text
/// extraction, and duration display in the gutter popup.
#[derive(Debug)]
pub struct CommandMeta {
    /// Column where the user's command text begins (from OSC 133 `B`).
    /// `None` when the shell doesn't emit `B`.
    pub command_col: Option<u32>,
    /// Absolute row where OSC 133 `B` fired. Usually the same as the
    /// prompt row, but multi-line prompts can differ.
    pub command_row: Option<u64>,
    /// Absolute row where OSC 133 `C` fired (command output starts).
    pub output_row: Option<u64>,
    /// When execution started (timestamped at `C`).
    pub started_at: Option<Instant>,
    /// When the command finished (timestamped at `D`).
    pub finished_at: Option<Instant>,
}

impl CommandMeta {
    fn new() -> Self {
        Self {
            command_col: None,
            command_row: None,
            output_row: None,
            started_at: None,
            finished_at: None,
        }
    }
}

/// State machine for absorbing the two parameter bytes of a VT52
/// `ESC Y Pr Pc` direct cursor address. The bytes arrive as separate
/// parser actions after the `EscDispatch { byte: 'Y' }` is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vt52CursorAddr {
    /// Not inside a VT52 ESC Y sequence.
    Idle,
    /// Got `ESC Y`; the next byte(s) contain the row.
    AwaitingRow,
    /// Got the row byte; waiting for the column byte.
    AwaitingCol(u8),
}

/// Terminal-level modes toggled by escape sequences (DECSET/DECRST, mode
/// 2004, mode 2026, etc.) and reset together by RIS. Grouping them keeps
/// the `Terminal` struct focused and lets handler functions accept a single
/// `&mut TerminalModes` instead of five separate parameters.
#[derive(Debug)]
pub struct TerminalModes {
    /// Currently-active mouse tracking mode requested by the app via DECSET.
    pub mouse_tracking: MouseTracking,
    /// Wire encoding used for mouse events.
    pub mouse_encoding: MouseEncoding,
    /// Mode 2004 — when enabled, pasted text is wrapped in
    /// `\x1b[200~ ... \x1b[201~` so apps can distinguish it from typed input.
    pub bracketed_paste: bool,
    /// Mode `?1004` — when enabled, focus changes are reported to the
    /// foreground app as `\x1b[I` (focus in) and `\x1b[O` (focus out).
    pub focus_reporting: bool,
    /// Mode 2026 — Synchronized Output (BSU/ESU). `Some(t)` from the moment
    /// `CSI ? 2026 h` arrives until either `CSI ? 2026 l` clears it or the
    /// [`SYNCHRONIZED_UPDATE_TIMEOUT`] safety deadline passes; otherwise
    /// `None`.
    pub synchronized_update_since: Option<Instant>,
    /// IRM (ANSI mode 4) — Insert/Replace mode. When `true`, printing a
    /// character shifts existing text right before writing. Default is
    /// replace (overwrite) mode.
    pub insert_mode: bool,
    /// LNM (ANSI mode 20) — Line Feed/New Line mode. When `true`, LF, VT,
    /// and FF perform an implicit CR before the line feed. Default is off.
    pub newline_mode: bool,
    /// DECARM (`?8`) — auto-repeat. Always on at the OS level; tracked
    /// here only so DECRQM can report it. Default is `true`.
    pub decarm: bool,
    /// DECLRMM (`?69`) — when `true`, left/right margins (set by DECSLRM)
    /// are active and constrain cursor movement, scrolling, and
    /// insertion/deletion. Default is `false`.
    pub declrmm: bool,
    /// DECNCSM (`?95`) — when `true`, DECCOLM switching does not clear
    /// the screen. Default is `false`.
    pub decncsm: bool,
    /// DECSCNM (`?5`) — when `true`, the entire screen renders in reverse
    /// video: the default bg becomes fg and vice versa. Per-cell SGR 7
    /// (REVERSE) XORs with this, so reversed cells appear normal.
    pub screen_reverse: bool,
    /// Mode 40 — when `true`, DECCOLM (mode 3) is honoured. Default is
    /// `false`, matching xterm. Without this gate a malicious escape
    /// sequence stream can repeatedly toggle 80/132 columns, triggering
    /// expensive grid resizes.
    pub allow_deccolm: bool,
    /// DECNRCM (`?42`) — when `true`, national replacement character-set
    /// designations replace their ASCII positions and the terminal behaves
    /// as a 7-bit national terminal.
    pub decnrcm: bool,
    /// Saved column count from before DECCOLM switched to 132 columns.
    /// `None` when in normal (80-column) mode.
    pub deccolm_saved_cols: Option<u32>,
    /// Current DEC operating level selected by DECSCL.
    pub conformance_level: ConformanceLevel,
    /// How terminal-generated C1 controls are transmitted to the host.
    pub c1_mode: C1Mode,
    /// DECANM (`?2`) — when `true` the terminal operates in VT52 compatibility
    /// mode. Set via `CSI ? 2 l`, cleared by `CSI ? 2 h` or RIS. VT52 mode
    /// uses a completely different (non-CSI) escape sequence vocabulary.
    pub vt52_mode: bool,
}

impl TerminalModes {
    fn new() -> Self {
        Self {
            mouse_tracking: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Default,
            bracketed_paste: false,
            focus_reporting: false,
            synchronized_update_since: None,
            insert_mode: false,
            newline_mode: false,
            decarm: true,
            declrmm: false,
            decncsm: false,
            screen_reverse: false,
            allow_deccolm: false,
            decnrcm: false,
            deccolm_saved_cols: None,
            conformance_level: ConformanceLevel::Level4,
            c1_mode: C1Mode::SevenBit,
            vt52_mode: false,
        }
    }
}

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
    /// Cell width in pixels. Stored for kitty display-sizing (`c=`/`r=` keys)
    /// once that path is wired up.
    cell_width: u32,

    next_image_id: u64,

    /// System clipboard gateway. Shared between OSC 52 and mouse-driven
    /// copy/paste paths.
    clipboard: Clipboard,

    /// Bytes produced by the terminal itself that must be written back to
    /// the PTY — responses to queries like OSC 52 `?` reads. Drained by the
    /// event loop after each [`process`](Self::process) call.
    pending_output: Vec<u8>,

    /// Terminal-level modes toggled by escape sequences (DECSET/DECRST,
    /// mode 2004, mode 2026, etc.) and reset together by RIS.
    pub modes: TerminalModes,

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

    /// Title last reported by the foreground app via OSC 0 / OSC 2.
    /// `None` means no app has set a title (or one explicitly cleared
    /// it); the host applies its default ("term41") in that case.
    pub current_title: Option<String>,

    /// xterm title stack. CSI 22;0 t pushes, CSI 23;0 t pops. Capped at
    /// 16 entries to bound memory from a misbehaving app.
    title_stack: Vec<Option<String>>,

    /// Saved private mode states for XTSAVE/XTRESTORE (CSI ? Ps s / r).
    saved_private_modes: HashMap<u16, bool>,

    /// Latched true whenever the parser sees a BEL byte (0x07). The host
    /// drains this each frame via [`Self::take_bell_pending`] so it can
    /// flash the screen, ping the compositor, etc. Latched (not
    /// counted) because reacting once per frame is the right grain — a
    /// noisy app that bells in a tight loop should still get one
    /// per-frame response, not a queue that backs up forever.
    bell_pending: bool,

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

    /// Per-prompt metadata (command column, output row, timing). Keyed by
    /// the absolute row of the prompt's `A` mark. Stale entries are pruned
    /// when their rows fall off the front of scrollback.
    pub(crate) command_metas: HashMap<u64, CommandMeta>,

    /// Kitty graphics protocol image store. Images transmitted via `a=t`
    /// live here until placed or deleted.
    kitty_images: image41::kitty::KittyImageStore,

    /// Accumulates chunks for multi-part kitty graphics transmissions.
    kitty_chunked: image41::kitty::ChunkedTransmission,

    /// Accumulates chunks for multi-part iTerm2 graphics transmissions
    /// (`MultipartFile` → `FilePart*` → `FileEnd`).
    iterm_chunked: image41::iterm::ChunkedTransmission,

    /// Runtime color palette. Stored here so SGR resets, OSC color queries,
    /// and the renderer can all resolve themed colors.
    pub palette: ColorPalette,

    /// State machine for the VT52 `ESC Y Pr Pc` direct cursor address. After
    /// `ESC Y` is dispatched, the next 1–2 byte actions carry the row and
    /// column values. This field persists across `apply` calls so the state
    /// survives the per-action dispatch boundary.
    vt52_cursor_addr: Vt52CursorAddr,
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
        cell_width: u32,
        palette: ColorPalette,
    ) -> Self {
        Self {
            active: Screen::new(cols, rows, scrollback_limit, palette.fg, palette.bg),
            // Stash starts as a blank alt screen (no scrollback). When the
            // first ?1049h / ?47h arrives we simply swap `active` and
            // `stash` — no lazy construction needed.
            stash: Screen::new(cols, rows, 0, palette.fg, palette.bg),
            viewport: Viewport { rows, cols },
            on_alt_screen: false,
            cell_height,
            next_image_id: 0,
            clipboard: Clipboard::new(),
            pending_output: Vec::new(),
            modes: TerminalModes::new(),
            selection: None,
            search: SearchState::new(),
            current_directory: None,
            hyperlinks: HyperlinkRegistry::new(),
            kitty_keyboard: KittyKeyboardState::new(),
            cursor_style: CursorStyle::default(),
            current_title: None,
            title_stack: Vec::new(),
            saved_private_modes: HashMap::new(),
            bell_pending: false,
            current_prompt_row: None,
            command_metas: HashMap::new(),
            kitty_images: image41::kitty::KittyImageStore::new(),
            kitty_chunked: image41::kitty::ChunkedTransmission::new(),
            iterm_chunked: image41::iterm::ChunkedTransmission::new(),
            cell_width,
            palette,
            vt52_cursor_addr: Vt52CursorAddr::Idle,
        }
    }

    /// Returns `true` when the foreground app has opened a synchronized
    /// output window (mode 2026) that has not yet been closed or timed out.
    /// The host should skip rendering while this returns `true` so partial
    /// frames (e.g. mid-scroll, mid-reflow) are never presented.
    pub fn is_synchronized_update_active(&self) -> bool {
        self.modes
            .synchronized_update_since
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
    pub fn set_cell_dimensions(
        &mut self,
        cell_width: u32,
        cell_height: u32,
    ) {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
    }

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
        if !self.modes.focus_reporting {
            return;
        }
        // Per xterm: CSI I on focus gain, CSI O on focus loss.
        conformance::write_csi(
            &mut self.pending_output,
            self.modes.c1_mode,
            format_args!("{}", if focused { 'I' } else { 'O' }),
        );
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
        if self.modes.bracketed_paste {
            conformance::write_csi(
                &mut self.pending_output,
                self.modes.c1_mode,
                format_args!("200~"),
            );
            for chunk in text.split(PASTE_END) {
                self.pending_output.extend_from_slice(chunk.as_bytes());
            }
            conformance::write_csi(
                &mut self.pending_output,
                self.modes.c1_mode,
                format_args!("201~"),
            );
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
        !matches!(self.modes.mouse_tracking, MouseTracking::Off)
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
        if !should_report(self.modes.mouse_tracking, kind, button) {
            return false;
        }
        encode_mouse_event(
            self.modes.c1_mode,
            self.modes.mouse_encoding,
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

    // -- Gutter popup / command metadata queries ----------------------------

    /// Walk backward from `screen_row` (0 = viewport top) to find the
    /// nearest prompt-start row. Returns the absolute row of the prompt,
    /// or `None` if no prompt exists above (or at) this row.
    pub fn find_prompt_for_screen_row(
        &self,
        screen_row: u32,
    ) -> Option<u64> {
        let base =
            self.active.grid.rows.len() - self.viewport.rows as usize - self.active.offset as usize;
        let start = base + screen_row as usize;
        let popped = self.active.grid.total_popped as u64;
        for i in (0..=start).rev() {
            if self.active.grid.rows[i].prompt_start {
                return Some(popped + i as u64);
            }
        }
        None
    }

    /// Find the next prompt_start after `after_abs`.
    fn find_next_prompt_after(
        &self,
        after_abs: u64,
    ) -> Option<u64> {
        let popped = self.active.grid.total_popped as u64;
        let start = after_abs.checked_sub(popped)? as usize + 1;
        for i in start..self.active.grid.rows.len() {
            if self.active.grid.rows[i].prompt_start {
                return Some(popped + i as u64);
            }
        }
        None
    }

    /// Find the last row of the command's output — one before the next
    /// prompt, or the end of the grid if no subsequent prompt exists.
    fn command_end_abs(
        &self,
        prompt_abs: u64,
    ) -> u64 {
        if let Some(next) = self.find_next_prompt_after(prompt_abs) {
            next.saturating_sub(1)
        } else {
            (self.active.grid.total_popped + self.active.grid.rows.len() - 1) as u64
        }
    }

    /// Extract text spanning `[start_abs, start_col]..=[end_abs, EOL]`.
    /// Trailing spaces are trimmed on each non-wrapped row; hard line
    /// breaks emit `\n`.
    fn extract_rows_text(
        &self,
        start_abs: u64,
        start_col: u32,
        end_abs: u64,
    ) -> String {
        let popped = self.active.grid.total_popped as u64;
        let mut out = String::new();
        for abs in start_abs..=end_abs {
            let Some(local) = abs.checked_sub(popped).map(|l| l as usize) else {
                continue;
            };
            if local >= self.active.grid.rows.len() {
                break;
            }
            let row = &self.active.grid.rows[local];
            let cs = if abs == start_abs {
                start_col as usize
            } else {
                0
            };
            let ce = row.cells.len();
            if cs >= ce {
                if abs < end_abs && !row.wrapped {
                    out.push('\n');
                }
                continue;
            }
            let mut seg = String::new();
            for cell in &row.cells[cs..ce] {
                seg.push_str(cell);
            }
            out.push_str(seg.trim_end_matches(' '));
            if abs < end_abs && !row.wrapped {
                out.push('\n');
            }
        }
        out
    }

    /// The typed command text at `prompt_abs` (between B/prompt and C/next
    /// prompt). Returns `None` if the prompt has scrolled off or no
    /// command has been executed from this prompt yet.
    pub fn command_text_at(
        &self,
        prompt_abs: u64,
    ) -> Option<String> {
        let meta = self.command_metas.get(&prompt_abs);
        let start_col = meta.and_then(|m| m.command_col).unwrap_or(0);
        let start_row = meta.and_then(|m| m.command_row).unwrap_or(prompt_abs);
        let end_row = self.command_text_end(prompt_abs, meta);
        if start_row > end_row {
            return None;
        }
        let text = self.extract_rows_text(start_row, start_col, end_row);
        if text.is_empty() { None } else { Some(text) }
    }

    /// Resolve the last row that belongs to "command text" (not output).
    /// When the command has produced output (`C` was received), that row
    /// is the clear boundary. Otherwise fall back to the next prompt, and
    /// if there isn't one either (the user hasn't run anything yet) clamp
    /// to the prompt row so the selection doesn't span the whole screen.
    fn command_text_end(
        &self,
        prompt_abs: u64,
        meta: Option<&CommandMeta>,
    ) -> u64 {
        if let Some(meta) = meta
            && let Some(output) = meta.output_row
        {
            return output.saturating_sub(1);
        }
        if let Some(next) = self.find_next_prompt_after(prompt_abs) {
            return next.saturating_sub(1);
        }
        // No output boundary and no subsequent prompt — nothing has been
        // executed from this prompt yet.
        prompt_abs
    }

    /// The output text for the command at `prompt_abs` (between C and next
    /// prompt). `None` if no output boundary was recorded.
    pub fn output_text_at(
        &self,
        prompt_abs: u64,
    ) -> Option<String> {
        let output_row = self.command_metas.get(&prompt_abs)?.output_row?;
        let end_row = self.command_end_abs(prompt_abs);
        if output_row > end_row {
            return None;
        }
        let text = self.extract_rows_text(output_row, 0, end_row);
        if text.is_empty() { None } else { Some(text) }
    }

    /// Command text + output combined.
    pub fn command_and_output_text_at(
        &self,
        prompt_abs: u64,
    ) -> Option<String> {
        let meta = self.command_metas.get(&prompt_abs);
        let start_col = meta.and_then(|m| m.command_col).unwrap_or(0);
        let start_row = meta.and_then(|m| m.command_row).unwrap_or(prompt_abs);
        let end_row = self.command_end_abs(prompt_abs);
        if start_row > end_row {
            return None;
        }
        let text = self.extract_rows_text(start_row, start_col, end_row);
        if text.is_empty() { None } else { Some(text) }
    }

    /// How long the command at `prompt_abs` took, if both C and D were seen.
    pub fn command_duration_at(
        &self,
        prompt_abs: u64,
    ) -> Option<Duration> {
        let meta = self.command_metas.get(&prompt_abs)?;
        let start = meta.started_at?;
        let end = meta.finished_at?;
        Some(end.duration_since(start))
    }

    /// Select the command text for the prompt at `prompt_abs` so the user
    /// sees which command the gutter popup refers to. Skips creating a
    /// selection when there's no meaningful command text (e.g. a fresh
    /// prompt before any command has been typed).
    pub fn select_command_at(
        &mut self,
        prompt_abs: u64,
    ) {
        let meta = self.command_metas.get(&prompt_abs);
        let start_col = meta.and_then(|m| m.command_col).unwrap_or(0);
        let start_row = meta.and_then(|m| m.command_row).unwrap_or(prompt_abs);
        let end_row = self.command_text_end(prompt_abs, meta);
        if start_row > end_row {
            return;
        }
        // Verify there's actual text before creating the selection.
        let text = self.extract_rows_text(start_row, start_col, end_row);
        if text.trim().is_empty() {
            return;
        }
        let end_col = self
            .absolute_row_to_local(end_row)
            .map(|l| self.active.grid.rows[l].content_len().saturating_sub(1))
            .unwrap_or(0);
        let anchor = SelectionPoint {
            row: start_row,
            col: start_col,
        };
        let head = SelectionPoint {
            row: end_row,
            col: end_col,
        };
        self.selection = Some(Selection {
            anchor,
            head,
            mode: SelectionMode::Char,
            origin: anchor,
        });
    }

    /// Copy arbitrary text to the system clipboard.
    pub fn copy_to_clipboard(
        &mut self,
        text: &str,
    ) {
        self.clipboard.set(ClipboardKind::Clipboard, text);
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
    ///
    /// `now` anchors the animation clock: each animated image's frame index
    /// is chosen from `now - placed_at`. Passing the same `now` to every
    /// visible image in a render pass keeps the whole frame temporally
    /// consistent.
    pub fn visible_images(
        &self,
        now: Instant,
    ) -> impl Iterator<Item = VisibleImage<'_>> {
        let viewport_top =
            self.active.grid.rows.len() - self.viewport.rows as usize - self.active.offset as usize;
        let viewport_bottom = viewport_top + self.viewport.rows as usize;
        let cell_height = self.cell_height;

        self.active.images.values().filter_map(move |img| {
            let img_rows = img.display_height.div_ceil(cell_height).max(1) as usize;
            let img_bottom = img.row + img_rows;
            if img.row < viewport_bottom && img_bottom > viewport_top {
                let elapsed = now.saturating_duration_since(img.placed_at);
                Some(VisibleImage {
                    image: &img.image,
                    id: img.id,
                    screen_row: img.row as i32 - viewport_top as i32,
                    screen_col: img.col,
                    display_width: img.display_width,
                    display_height: img.display_height,
                    frame_index: img.image.frame_at(elapsed),
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

    /// Apply a single parsed VTE action to the terminal state. Called by the
    /// terminal thread with the lock held — the parser runs *outside* the lock
    /// so the SIMD byte-scanning path never blocks rendering.
    ///
    /// Hook/Put/Unhook (DCS accumulation) are handled by the terminal thread
    /// directly and should not be passed here.
    pub fn apply(
        &mut self,
        action: Action<'_>,
    ) {
        let popped_before: usize = self.active.grid.total_popped;

        // VT52 ESC Y direct cursor address: absorb the two parameter bytes that
        // follow the EscDispatch. They arrive as PrintAscii or Execute actions
        // because vtepp is still in ANSI ground state; we intercept them here
        // before the normal dispatch so they are not printed as characters.
        if self.vt52_cursor_addr != Vt52CursorAddr::Idle {
            let byte_opt: Option<u8> = match &action {
                Action::PrintAscii(run) => run.first().copied(),
                Action::Execute(b) => Some(*b),
                _ => None,
            };

            match (self.vt52_cursor_addr, byte_opt) {
                (Vt52CursorAddr::AwaitingRow, Some(b)) => {
                    self.vt52_cursor_addr = Vt52CursorAddr::AwaitingCol(b.saturating_sub(0x20));

                    // The two bytes may arrive batched in one PrintAscii run.
                    // If so, consume the second byte (col) immediately and
                    // then fall through to process any remaining bytes normally.
                    if let Action::PrintAscii(run) = &action
                        && run.len() >= 2
                    {
                        let col = run[1].saturating_sub(0x20) as u32;
                        let row = b.saturating_sub(0x20) as u32;
                        self.active.cursor.row = row.min(self.viewport.rows.saturating_sub(1));
                        self.active.cursor.col = col.min(self.viewport.cols.saturating_sub(1));
                        self.vt52_cursor_addr = Vt52CursorAddr::Idle;
                        // Any bytes after the two position bytes are normal text.
                        if run.len() > 2 {
                            put_ascii_run(
                                &mut self.active,
                                &self.viewport,
                                &run[2..],
                                self.modes.insert_mode,
                            );
                        }
                        self.track_scroll(popped_before);
                        return;
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                (Vt52CursorAddr::AwaitingCol(row), Some(b)) => {
                    let col = b.saturating_sub(0x20) as u32;
                    self.active.cursor.row = (row as u32).min(self.viewport.rows.saturating_sub(1));
                    self.active.cursor.col = col.min(self.viewport.cols.saturating_sub(1));
                    self.vt52_cursor_addr = Vt52CursorAddr::Idle;

                    // If more bytes follow in the same PrintAscii run, print them.
                    if let Action::PrintAscii(run) = &action
                        && run.len() > 1
                    {
                        put_ascii_run(
                            &mut self.active,
                            &self.viewport,
                            &run[1..],
                            self.modes.insert_mode,
                        );
                    }
                    self.track_scroll(popped_before);
                    return;
                }
                _ => {
                    // Unexpected action type: abort the ESC Y sequence and
                    // fall through to process this action normally.
                    self.vt52_cursor_addr = Vt52CursorAddr::Idle;
                }
            }
        }

        // In VT52 mode, CSI sequences are not valid and should be silently
        // dropped — vtepp still parses them because it doesn't know the
        // terminal mode, but executing them would be wrong.
        if self.modes.vt52_mode && matches!(action, Action::CsiDispatch { .. }) {
            self.track_scroll(popped_before);
            return;
        }

        match action {
            Action::PrintAscii(run) => put_ascii_run(
                &mut self.active,
                &self.viewport,
                run,
                self.modes.insert_mode,
            ),
            Action::PrintText(run) => put_text_run(
                &mut self.active,
                &self.viewport,
                run,
                self.modes.insert_mode,
            ),
            Action::Print(c) => {
                put_char(&mut self.active, &self.viewport, c, self.modes.insert_mode)
            }
            Action::Execute(byte) => {
                execute(
                    &mut self.active,
                    &self.viewport,
                    byte,
                    &mut self.bell_pending,
                    self.modes.newline_mode,
                );
            }
            Action::CsiDispatch {
                params,
                intermediates,
                action,
            } => {
                let mut ctx = CsiContext {
                    screen: &mut self.active,
                    stash: &mut self.stash,
                    viewport: &mut self.viewport,
                    on_alt_screen: &mut self.on_alt_screen,
                    modes: &mut self.modes,
                    kitty_keyboard: &mut self.kitty_keyboard,
                    pending_output: &mut self.pending_output,
                    cursor_style: &mut self.cursor_style,
                    cell_width: self.cell_width,
                    cell_height: self.cell_height,
                    palette: &self.palette,
                    title_stack: &mut self.title_stack,
                    current_title: &mut self.current_title,
                    saved_modes: &mut self.saved_private_modes,
                    current_prompt_row: &mut self.current_prompt_row,
                    bell_pending: &mut self.bell_pending,
                    vt52_cursor_addr: &mut self.vt52_cursor_addr,
                };
                csi_dispatch(&mut ctx, &params, intermediates.as_slice(), action);
            }
            Action::EscDispatch {
                intermediates,
                byte,
            } => {
                let mut ctx = EscContext {
                    screen: &mut self.active,
                    stash: &mut self.stash,
                    viewport: &self.viewport,
                    on_alt_screen: &mut self.on_alt_screen,
                    modes: &mut self.modes,
                    kitty_keyboard: &mut self.kitty_keyboard,
                    cursor_style: &mut self.cursor_style,
                    current_title: &mut self.current_title,
                    title_stack: &mut self.title_stack,
                    saved_modes: &mut self.saved_private_modes,
                    current_prompt_row: &mut self.current_prompt_row,
                    bell_pending: &mut self.bell_pending,
                    palette: &self.palette,
                    pending_output: &mut self.pending_output,
                    vt52_cursor_addr: &mut self.vt52_cursor_addr,
                };
                esc_dispatch(&mut ctx, intermediates.as_slice(), byte);
            }
            Action::OscDispatch(data) => {
                // iTerm2 image protocol rides on OSC 1337. Route it next
                // to the other graphics protocols (kitty on APC, sixel
                // on DCS) rather than through the text-OSC dispatcher,
                // which doesn't carry cursor / cell-size state.
                if let Some(rest) = data.strip_prefix(b"1337;")
                    && is_iterm_image_cmd(rest)
                {
                    handle_iterm_graphics(
                        rest,
                        &mut self.iterm_chunked,
                        &mut self.active,
                        &self.viewport,
                        &mut self.next_image_id,
                        self.cell_height,
                        self.cell_width,
                    );
                } else {
                    let mut ctx = OscContext {
                        clipboard: &mut self.clipboard,
                        pending_output: &mut self.pending_output,
                        c1_mode: self.modes.c1_mode,
                        current_directory: &mut self.current_directory,
                        hyperlinks: &mut self.hyperlinks,
                        active_screen: &mut self.active,
                        viewport: &self.viewport,
                        current_title: &mut self.current_title,
                        current_prompt_row: &mut self.current_prompt_row,
                        command_metas: &mut self.command_metas,
                        palette: &self.palette,
                        cell_width: self.cell_width,
                        cell_height: self.cell_height,
                    };
                    handle_osc(&data, &mut ctx);
                }
            }
            Action::ApcDispatch(data) => {
                handle_kitty_graphics(
                    &data,
                    &mut self.kitty_images,
                    &mut self.kitty_chunked,
                    &mut self.active,
                    &self.viewport,
                    &mut self.next_image_id,
                    self.cell_height,
                    self.cell_width,
                    self.modes.c1_mode,
                    &mut self.pending_output,
                );
            }
            // Hook/Put/Unhook are accumulated by the terminal thread and
            // dispatched via place_sixel_image — they never reach here.
            Action::Hook { .. } | Action::Put(_) | Action::Unhook => {}
        }

        self.track_scroll(popped_before);
    }

    /// Place a fully-decoded sixel image at the current cursor position.
    /// Called by the terminal thread *after* parsing the sixel data outside
    /// the lock, so the CPU-intensive decode doesn't block rendering.
    pub fn place_sixel_image(
        &mut self,
        image: image41::DecodedImage,
    ) {
        let popped_before: usize = self.active.grid.total_popped;

        let id = self.next_image_id;
        self.next_image_id += 1;
        let row = self
            .active
            .grid
            .active_row_index(&self.active.cursor, &self.viewport);
        let image_rows = image.height.div_ceil(self.cell_height);
        crate::image::remove_overlapping(
            &mut self.active.images,
            row,
            image_rows.max(1) as usize,
            self.active.cursor.col,
            self.cell_height,
        );
        let display_width = image.width;
        let display_height = image.height;
        self.active.images.insert(
            id,
            PlacedImage {
                image,
                id,
                row,
                col: self.active.cursor.col,
                display_width,
                display_height,
                placed_at: Instant::now(),
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

        self.track_scroll(popped_before);
    }

    /// Adjust image positions and prune stale command metadata after rows
    /// have been scrolled off the top of the grid.
    fn track_scroll(
        &mut self,
        popped_before: usize,
    ) {
        // Use saturating_sub: a screen swap during this iteration can
        // reset `total_popped` to the other grid's value, which would
        // underflow an unchecked subtraction.
        let newly_popped = self.active.grid.total_popped.saturating_sub(popped_before);
        if newly_popped > 0 {
            self.active.images.retain(|_, img| img.row >= newly_popped);
            for img in self.active.images.values_mut() {
                img.row -= newly_popped;
            }
            let min_abs = self.active.grid.total_popped as u64;
            self.command_metas.retain(|&abs, _| abs >= min_abs);
        }
    }
}

/// Handle to a running terminal thread. Signals the thread to stop on drop.
pub struct TerminalThread {
    stop: Arc<AtomicBool>,
    pub thread_handle: Arc<OnceLock<Thread>>,
}

impl Default for TerminalThread {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalThread {
    /// Create a fresh `OnceLock` that the terminal thread will populate with
    /// its `Thread` handle. Pass a clone to `Pty::spawn` so the PTY reader
    /// can unpark the terminal thread.
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread_handle: Arc::new(OnceLock::new()),
        }
    }

    /// Spawn the terminal thread. `thread_handle` must be the same `OnceLock`
    /// that was passed to `Pty::spawn` for this tab.
    pub fn spawn(
        &self,
        name: String,
        terminal: Arc<Mutex<Terminal>>,
        pty_reader: PtyReader,
        render_thread: Arc<OnceLock<Thread>>,
        startup_redraw: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_ = stop.clone();
        let handle_ = self.thread_handle.clone();

        thread::Builder::new()
            .name(name)
            .spawn(move || {
                handle_
                    .set(thread::current())
                    .expect("set terminal thread handle");
                run_terminal_thread(terminal, pty_reader, stop_, render_thread, startup_redraw);
            })
            .expect("spawn terminal thread");
    }
}

impl Drop for TerminalThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread_handle.get() {
            t.unpark();
        }
    }
}

/// Handle XTGETTCAP (DCS + q). The payload is a semicolon-separated list of
/// hex-encoded terminfo capability names. For each recognized name, reply
/// `DCS 1 + r <hex_name>=<hex_value> ST`; for unknown names reply
/// `DCS 0 + r <hex_name> ST`.
///
/// DECRQSS — Request Selection or Setting (`DCS $ q <selector> ST`).
/// The selector identifies which setting to report. We reply with
/// `DCS 1 $ r <value> ST` for recognised settings, or
/// `DCS 0 $ r ST` for unrecognised ones.
fn handle_decrqss(
    selector: &[u8],
    terminal: &mut Terminal,
) {
    let out = &mut terminal.pending_output;
    let c1_mode = terminal.modes.c1_mode;

    match selector {
        // SGR — report current graphic rendition.
        b"m" => {
            let screen = &terminal.active;
            let mut parts: Vec<String> = Vec::new();
            let attrs = screen.attrs;
            if attrs.contains(font41::attrs::CellAttrs::BOLD) {
                parts.push("1".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::DIM) {
                parts.push("2".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::ITALIC) {
                parts.push("3".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::REVERSE) {
                parts.push("7".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::HIDDEN) {
                parts.push("8".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::STRIKETHROUGH) {
                parts.push("9".into());
            }
            if attrs.contains(font41::attrs::CellAttrs::OVERLINE) {
                parts.push("53".into());
            }
            if parts.is_empty() {
                parts.push("0".into());
            }
            let sgr = parts.join(";");
            conformance::write_dcs(out, c1_mode, format_args!("1$r{sgr}m"));
        }
        // DECSTBM — report scroll region.
        b"r" => {
            let top = terminal.active.scroll_top + 1;
            let bottom = terminal.active.scroll_bottom + 1;
            conformance::write_dcs(out, c1_mode, format_args!("1$r{top};{bottom}r"));
        }
        // DECSLRM — report left/right margins.
        b"s" => {
            let left = terminal.active.left_margin + 1;
            let right = terminal.active.right_margin + 1;
            conformance::write_dcs(out, c1_mode, format_args!("1$r{left};{right}s"));
        }
        // DECSCL — report operating level / C1 transmission mode.
        b"\"p" => {
            let level = terminal.modes.conformance_level.da1_code();
            if terminal.modes.conformance_level == ConformanceLevel::Level1 {
                conformance::write_dcs(out, c1_mode, format_args!("1$r{level}\"p"));
            } else {
                conformance::write_dcs(
                    out,
                    c1_mode,
                    format_args!("1$r{level};{}\"p", terminal.modes.c1_mode.decscl_param()),
                );
            }
        }
        // DECSCUSR — report cursor style.
        b" q" => {
            let ps = match (terminal.cursor_style.shape, terminal.cursor_style.blink) {
                (cursor::CursorShape::Block, true) => 1,
                (cursor::CursorShape::Block, false) => 2,
                (cursor::CursorShape::Underline, true) => 3,
                (cursor::CursorShape::Underline, false) => 4,
                (cursor::CursorShape::Beam, true) => 5,
                (cursor::CursorShape::Beam, false) => 6,
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps} q"));
        }
        // DECSCA — report character protection attribute.
        b"\"q" => {
            let ps = if terminal
                .active
                .attrs
                .contains(font41::attrs::CellAttrs::PROTECTED)
            {
                1
            } else {
                0
            };
            conformance::write_dcs(out, c1_mode, format_args!("1$r{ps}\"q"));
        }
        // Unrecognised — invalid response.
        _ => {
            conformance::write_dcs(out, c1_mode, format_args!("0$r"));
        }
    }
}

/// This lets tmux and neovim discover features like truecolor, styled
/// underlines, and cursor shapes without relying on a terminfo entry.
fn handle_xtgettcap(
    payload: &[u8],
    c1_mode: C1Mode,
    output: &mut Vec<u8>,
) {
    for cap_hex in payload.split(|&b| b == b';') {
        if cap_hex.is_empty() {
            continue;
        }
        let cap_name = hex_decode(cap_hex);
        let cap_str = std::str::from_utf8(&cap_name).unwrap_or("");
        if let Some(value) = xtgettcap_value(cap_str) {
            let value_hex = hex_encode(value.as_bytes());
            conformance::push_dcs_prefix(output, c1_mode);
            output.extend_from_slice(b"1+r");
            output.extend_from_slice(cap_hex);
            output.push(b'=');
            output.extend_from_slice(value_hex.as_bytes());
            conformance::push_st(output, c1_mode);
        } else {
            conformance::push_dcs_prefix(output, c1_mode);
            output.extend_from_slice(b"0+r");
            output.extend_from_slice(cap_hex);
            conformance::push_st(output, c1_mode);
        }
    }
}

/// Map a terminfo capability name to its value string. Returns `None` for
/// unrecognized capabilities.
fn xtgettcap_value(name: &str) -> Option<&'static str> {
    match name {
        // Truecolor support (tmux, neovim gate on this).
        "RGB" => Some(""),
        // Number of colors.
        "colors" => Some("256"),
        // Set cursor shape: CSI Ps SP q (DECSCUSR).
        "Ss" => Some("\x1b[%p1%d q"),
        // Reset cursor shape.
        "Se" => Some("\x1b[2 q"),
        // Styled underlines (kitty extension). CSI 4:Pm m.
        "Smulx" => Some("\x1b[4:%p1%dm"),
        // Set underline color. CSI 58:2::%p1%d:%p2%d:%p3%d m.
        "Setulc" => Some("\x1b[58:2::%p1%{65536}%*%p2%{256}%*%+%p3%+m"),
        // Set RGB foreground. CSI 38:2::R:G:B m.
        "setrgbf" => Some("\x1b[38:2:%p1%d:%p2%d:%p3%dm"),
        // Set RGB background. CSI 48:2::R:G:B m.
        "setrgbb" => Some("\x1b[48:2:%p1%d:%p2%d:%p3%dm"),
        // Terminal name.
        "TN" => Some("xterm-256color"),
        _ => None,
    }
}

fn hex_decode(hex: &[u8]) -> Vec<u8> {
    hex.chunks(2)
        .filter_map(|pair| {
            if pair.len() == 2 {
                u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()
            } else {
                None
            }
        })
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn handle_dcs(
    params: vtepp::Params,
    intermediates: &[u8],
    action: char,
    payload: &[u8],
    terminal: &mut Terminal,
) {
    if action == 'q' && intermediates == b"+" {
        let c1_mode = terminal.modes.c1_mode;
        handle_xtgettcap(payload, c1_mode, &mut terminal.pending_output);
    } else if action == 'q' && intermediates == b"$" {
        handle_decrqss(payload, terminal);
    } else if action == 'u' && intermediates == b"!" {
        let ps = params
            .iter()
            .next()
            .and_then(|group| group.first().copied())
            .unwrap_or(0);
        if let Some(upss) = charset::parse_upss_assignment(ps, payload) {
            for screen in [&mut terminal.active, &mut terminal.stash] {
                screen.upss = upss;
            }
        }
    }
}

/// Main loop for the per-tab terminal thread. Drains PTY data, parses it
/// (without the terminal lock), and applies each action under a brief lock.
pub fn run_terminal_thread(
    terminal: Arc<Mutex<Terminal>>,
    mut pty_reader: PtyReader,
    stop: Arc<AtomicBool>,
    render_thread: Arc<OnceLock<Thread>>,
    startup_redraw: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let mut parser = vtepp::Parser::new();
    let mut hook_bytes: Vec<Vec<u8>> = vec![];
    let mut hook_params: Vec<vtepp::Params> = vec![];
    let mut hook_intermediates: Vec<vtepp::Intermediates> = vec![];
    let mut hook_action: Vec<char> = vec![];
    let mut buf = [0u8; MAX_READ_CHUNK];

    loop {
        // Drain all available PTY data. On the first iteration this catches
        // bytes that arrived before the OnceLock was set (PTY reader couldn't
        // unpark us yet).
        pty_reader.clear_pending();
        let mut did_work = false;
        let mut hit_budget = false;
        let batch_start = std::time::Instant::now();
        loop {
            let n = pty_reader.read(&mut buf);
            if n == 0 {
                break;
            }
            did_work = true;
            for action in parser.parse(&buf[..n]) {
                match action {
                    vtepp::Action::Hook {
                        params,
                        intermediates,
                        action,
                    } => {
                        hook_bytes.push(vec![]);
                        hook_params.push(params);
                        hook_intermediates.push(intermediates);
                        hook_action.push(action);
                    }
                    vtepp::Action::Put(bytes) => {
                        if let Some(last) = hook_bytes.last_mut() {
                            last.extend_from_slice(bytes);
                        }
                    }
                    vtepp::Action::Unhook => {
                        let bytes = hook_bytes.pop().unwrap();
                        let params = hook_params.pop().unwrap();
                        let intermediates = hook_intermediates.pop().unwrap();
                        let act = hook_action.pop().unwrap();
                        if act == 'q' && intermediates.as_slice() == b"+" {
                            // XTGETTCAP — terminal capability query. tmux and
                            // neovim use this to discover features like
                            // truecolor, cursor shapes, styled underlines.
                            let mut t = terminal.lock().unwrap();
                            let c1_mode = t.modes.c1_mode;
                            handle_xtgettcap(&bytes, c1_mode, &mut t.pending_output);
                        } else if act == 'q' && intermediates.as_slice() == b"$" {
                            // DECRQSS — Request Selection or Setting.
                            // The payload identifies the setting being queried;
                            // we reply with `DCS 1 $ r <value> ST` for
                            // recognised settings, `DCS 0 $ r ST` otherwise.
                            let mut t = terminal.lock().unwrap();
                            handle_decrqss(&bytes, &mut t);
                        } else if act == 'q' && intermediates.as_slice().is_empty() {
                            // Sixel parsing is CPU-heavy — done outside the
                            // lock so rendering isn't blocked.
                            let image = image41::sixel::parse_sixel(params, bytes);
                            terminal.lock().unwrap().place_sixel_image(image);
                        } else {
                            handle_dcs(
                                params,
                                intermediates.as_slice(),
                                act,
                                &bytes,
                                &mut terminal.lock().unwrap(),
                            );
                        }
                    }
                    action => {
                        terminal.lock().unwrap().apply(action);
                    }
                }
            }
            if terminal_batch_budget_exhausted(batch_start) {
                hit_budget = true;
                break;
            }
        }

        if did_work && let Some(t) = render_thread.get() {
            t.unpark();
        }
        if did_work && let Some(request_redraw) = startup_redraw.as_ref() {
            request_redraw();
        }

        if stop.load(Ordering::Acquire) {
            break;
        }

        if hit_budget {
            thread::yield_now();
            continue;
        }

        thread::park();
        if stop.load(Ordering::Acquire) {
            break;
        }
    }
}

const TERMINAL_BATCH_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

fn terminal_batch_budget_exhausted(batch_start: std::time::Instant) -> bool {
    batch_start.elapsed() >= TERMINAL_BATCH_TIME_BUDGET
}

// ---------------------------------------------------------------------------
// Kitty graphics protocol handler
// ---------------------------------------------------------------------------

fn handle_kitty_graphics(
    data: &[u8],
    store: &mut image41::kitty::KittyImageStore,
    chunked: &mut image41::kitty::ChunkedTransmission,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    // APC payload must start with 'G'.
    if data.first() != Some(&b'G') {
        return;
    }

    let cmd = image41::kitty::parse_command(&data[1..]);

    // Feed into chunked accumulator. If more chunks expected, return early.
    let cmd = match chunked.feed(cmd) {
        Some(cmd) => cmd,
        None => return,
    };

    match cmd.action {
        b'q' => handle_kitty_query(&cmd, store, c1_mode, pending_output),
        b'T' => handle_kitty_transmit_display(
            &cmd,
            store,
            screen,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
            c1_mode,
            pending_output,
        ),
        b't' => handle_kitty_transmit(&cmd, store, c1_mode, pending_output),
        b'p' => handle_kitty_place(
            &cmd,
            store,
            screen,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
            c1_mode,
            pending_output,
        ),
        b'd' => handle_kitty_delete(&cmd, screen, store, cell_height),
        _ => {}
    }
}

/// Decode image data from a command's payload, handling direct, file, and
/// temp-file transmission mediums.
fn decode_kitty_image(cmd: &image41::kitty::KittyCommand) -> Option<image41::DecodedImage> {
    match cmd.transmission {
        b'f' => image41::kitty::decode_file_payload(cmd, &cmd.payload, false),
        b't' => image41::kitty::decode_file_payload(cmd, &cmd.payload, true),
        _ => image41::kitty::decode_payload(cmd, &cmd.payload),
    }
}

/// Place an image on the grid at the cursor position.
fn place_kitty_image(
    image: image41::DecodedImage,
    cmd: &image41::kitty::KittyCommand,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) {
    // Apply source rectangle cropping.
    let image = image41::kitty::crop_source_rect(image, cmd);

    let id = *next_image_id;
    *next_image_id += 1;

    let row = screen.grid.active_row_index(&screen.cursor, viewport);

    // Compute display size in pixels. `c=`/`r=` take precedence over the
    // image's native pixel dimensions and drive both cursor advancement
    // and the render-time quad scaling.
    //
    // If only one of `c` and `r` is given, preserve the source aspect ratio
    // in the other dimension — this matches kitty's documented behaviour
    // and is what `viu -w N` relies on for fitted previews.
    let (display_width, display_height) = match (cmd.columns > 0, cmd.rows > 0) {
        (true, true) => (cmd.columns * cell_width, cmd.rows * cell_height),
        (true, false) => {
            let w = cmd.columns * cell_width;
            let h = if image.width > 0 {
                (image.height as u64 * w as u64 / image.width as u64) as u32
            } else {
                image.height
            };
            (w, h)
        }
        (false, true) => {
            let h = cmd.rows * cell_height;
            let w = if image.height > 0 {
                (image.width as u64 * h as u64 / image.height as u64) as u32
            } else {
                image.width
            };
            (w, h)
        }
        (false, false) => (image.width, image.height),
    };

    let image_rows = display_height.div_ceil(cell_height);

    crate::image::remove_overlapping(
        &mut screen.images,
        row,
        image_rows.max(1) as usize,
        screen.cursor.col,
        cell_height,
    );

    screen.images.insert(
        id,
        PlacedImage {
            image,
            id,
            row,
            col: screen.cursor.col,
            display_width,
            display_height,
            placed_at: Instant::now(),
        },
    );

    // Advance cursor past the image unless C=1.
    if !cmd.no_move_cursor {
        let advance_rows = image_rows;
        for _ in 0..advance_rows {
            screen.cursor.row += 1;
            if screen.cursor.row >= viewport.rows {
                screen.grid.push_visible_row(viewport);
                screen.cursor.row = viewport.rows - 1;
            }
        }
        screen.cursor.col = 0;
    }
}

fn send_kitty_response(
    cmd: &image41::kitty::KittyCommand,
    image_id: u32,
    ok: bool,
    message: &str,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    // q=1 suppresses OK responses, q=2 suppresses all.
    if cmd.quiet >= 2 {
        return;
    }
    if cmd.quiet >= 1 && ok {
        return;
    }
    let status = if ok { "OK" } else { message };
    conformance::write_apc(
        pending_output,
        c1_mode,
        format_args!("Gi={image_id};{status}"),
    );
}

fn handle_kitty_query(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = store.resolve_id(cmd);
    // Query just tests if the protocol is supported. Respond OK without
    // storing anything.
    send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
}

fn handle_kitty_transmit(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = store.resolve_id(cmd);
    match decode_kitty_image(cmd) {
        Some(image) => {
            store.store(id, image);
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
        }
        None => {
            send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output);
        }
    }
}

fn handle_kitty_transmit_display(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = store.resolve_id(cmd);
    match decode_kitty_image(cmd) {
        Some(image) => {
            // Store for potential later re-placement.
            store.store(id, image.clone());
            place_kitty_image(
                image,
                cmd,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
        }
        None => {
            send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output);
        }
    }
}

fn handle_kitty_place(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = store.resolve_id(cmd);
    match store.get(id) {
        Some(image) => {
            let image = image.clone();
            place_kitty_image(
                image,
                cmd,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
        }
        None => {
            send_kitty_response(cmd, id, false, "ENOENT", c1_mode, pending_output);
        }
    }
}

fn handle_kitty_delete(
    cmd: &image41::kitty::KittyCommand,
    screen: &mut Screen,
    store: &mut image41::kitty::KittyImageStore,
    cell_height: u32,
) {
    let uppercase = cmd.delete.is_ascii_uppercase();
    match cmd.delete.to_ascii_lowercase() {
        b'a' | 0 => {
            // Delete all visible placements.
            screen.images.clear();
            if uppercase {
                store.clear();
            }
        }
        b'i' => {
            // Delete by image id.
            let id = cmd.image_id;
            if cmd.placement_id != 0 {
                // Delete specific placement — we don't track placement ids on
                // PlacedImage yet, so remove the first image with matching
                // stored-image dimensions. For now, remove all with that
                // stored-image id.
                if let Some(stored) = store.get(id) {
                    let (sw, sh) = (stored.width, stored.height);
                    screen
                        .images
                        .retain(|_, img| img.image.width != sw || img.image.height != sh);
                }
            } else {
                // Remove all placements of this image.
                if let Some(stored) = store.get(id) {
                    let (sw, sh) = (stored.width, stored.height);
                    screen
                        .images
                        .retain(|_, img| img.image.width != sw || img.image.height != sh);
                }
            }
            if uppercase {
                store.remove(id);
            }
        }
        b'c' => {
            // Delete at cursor position.
            let cursor_row = screen.grid.active_row_index(
                &screen.cursor,
                &Viewport {
                    rows: screen.grid.rows.len() as u32,
                    cols: 0,
                },
            );
            let cursor_col = screen.cursor.col;
            screen.images.retain(|_, img| {
                if img.col != cursor_col {
                    return true;
                }
                let img_rows = img.image.height.div_ceil(cell_height).max(1) as usize;
                let img_bottom = img.row + img_rows;
                !(img.row <= cursor_row && cursor_row < img_bottom)
            });
        }
        b'r' => {
            // Delete by id range.
            let lo = cmd.src_x; // x= is the lower bound
            let hi = cmd.src_y; // y= is the upper bound
            if let Some(lo_stored) = store.get(lo) {
                let _ = lo_stored; // just checking existence
            }
            // Remove placements for images in the range (approximate: we
            // don't track which stored image id each PlacedImage came from).
            if uppercase {
                store.remove_range(lo, hi);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// iTerm2 graphics protocol handler (OSC 1337 ; File= / MultipartFile= / …)
// ---------------------------------------------------------------------------

/// True when an OSC 1337 subcommand (after the `1337;` prefix) is part of
/// the iTerm2 inline-image protocol. OSC 1337 is also used by iTerm2 for
/// shell integration, notifications, and other extensions — routing only
/// the image subcommands keeps those out of this handler.
fn is_iterm_image_cmd(rest: &[u8]) -> bool {
    rest.starts_with(b"File=")
        || rest.starts_with(b"MultipartFile=")
        || rest.starts_with(b"FilePart=")
        || rest == b"FileEnd"
}

/// Dispatch an iTerm2 graphics OSC 1337 subcommand. `rest` is the OSC
/// payload with the leading `1337;` already consumed.
fn handle_iterm_graphics(
    rest: &[u8],
    chunked: &mut image41::iterm::ChunkedTransmission,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) {
    if let Some(cmd) = image41::iterm::parse_file(rest) {
        if let Some(image) = image41::iterm::decode_payload(&cmd.payload) {
            place_iterm_image(
                cmd,
                image,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
        }
        return;
    }
    if let Some(header) = image41::iterm::parse_multipart_start(rest) {
        chunked.begin(header);
        return;
    }
    if let Some(chunk) = image41::iterm::parse_file_part(rest) {
        chunked.push(chunk);
        return;
    }
    if image41::iterm::is_file_end(rest)
        && let Some(cmd) = chunked.finish()
        && let Some(image) = image41::iterm::decode_payload(&cmd.payload)
    {
        place_iterm_image(
            cmd,
            image,
            screen,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
        );
    }
}

/// Place an iTerm2 image on the grid at the cursor position, resolving
/// `width`/`height` (cells, pixels, percent, or auto) into final display
/// pixels and advancing the cursor past the image unless `doNotMoveCursor`
/// is set.
fn place_iterm_image(
    cmd: image41::iterm::ItermCommand,
    image: image41::DecodedImage,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) {
    // The spec default for `inline` is 0 ("download silently"). A terminal
    // that can't offer a download UI has nothing useful to do with a hidden
    // image — drop rather than silently render what the sender said not to.
    if !cmd.inline {
        return;
    }

    let viewport_px_w = viewport.cols * cell_width;
    let viewport_px_h = viewport.rows * cell_height;

    let w_given = !matches!(cmd.width, image41::iterm::Dimension::Auto);
    let h_given = !matches!(cmd.height, image41::iterm::Dimension::Auto);

    let mut display_width = cmd.width.to_pixels(cell_width, viewport_px_w, image.width);
    let mut display_height = cmd
        .height
        .to_pixels(cell_height, viewport_px_h, image.height);

    // When only one axis is specified and preserveAspectRatio is on,
    // derive the other from the source image's aspect ratio. With both
    // axes given, honour the sender verbatim (they asked for a stretch).
    if cmd.preserve_aspect_ratio && w_given != h_given && image.width > 0 && image.height > 0 {
        if w_given {
            display_height =
                (display_width as u64 * image.height as u64 / image.width as u64) as u32;
        } else {
            display_width =
                (display_height as u64 * image.width as u64 / image.height as u64) as u32;
        }
    }

    if display_width == 0 || display_height == 0 {
        return;
    }

    let id = *next_image_id;
    *next_image_id += 1;

    let row = screen.grid.active_row_index(&screen.cursor, viewport);
    let image_rows = display_height.div_ceil(cell_height);

    crate::image::remove_overlapping(
        &mut screen.images,
        row,
        image_rows.max(1) as usize,
        screen.cursor.col,
        cell_height,
    );

    screen.images.insert(
        id,
        PlacedImage {
            image,
            id,
            row,
            col: screen.cursor.col,
            display_width,
            display_height,
            placed_at: Instant::now(),
        },
    );

    if !cmd.do_not_move_cursor {
        for _ in 0..image_rows {
            screen.cursor.row += 1;
            if screen.cursor.row >= viewport.rows {
                screen.grid.push_visible_row(viewport);
                screen.cursor.row = viewport.rows - 1;
            }
        }
        screen.cursor.col = 0;
    }
}

#[cfg(test)]
mod tests {
    use vtepp::Parser;

    use super::*;

    /// Test wrapper that bundles a `Terminal` with its own `Parser` so tests
    /// can call `.process()` the same way as before the parser was extracted.
    /// Deref/DerefMut coerce to `&Terminal`/`&mut Terminal` transparently.
    struct TestTerm {
        inner: Terminal,
        parser: Parser,
    }

    impl TestTerm {
        fn new(
            cols: u32,
            rows: u32,
            scrollback: u32,
            cell_h: u32,
            cell_w: u32,
        ) -> Self {
            Self {
                inner: Terminal::new(
                    cols,
                    rows,
                    scrollback,
                    cell_h,
                    cell_w,
                    ColorPalette::default(),
                ),
                parser: Parser::new(),
            }
        }

        fn process(
            &mut self,
            data: &[u8],
        ) {
            for action in self.parser.parse(data) {
                self.inner.apply(action);
            }
        }
    }

    impl std::ops::Deref for TestTerm {
        type Target = Terminal;

        fn deref(&self) -> &Terminal {
            &self.inner
        }
    }

    impl std::ops::DerefMut for TestTerm {
        fn deref_mut(&mut self) -> &mut Terminal {
            &mut self.inner
        }
    }

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
        let mut term = TestTerm::new(8, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(8, 3, 100, 16, 8);
        term.process(b"\x1b[?1049h");

        // Fill enough rows on alt to normally produce scrollback on primary.
        for _ in 0..10 {
            term.process(b"line\n");
        }
        assert_eq!(term.active.grid.scrollback_len(&term.viewport), 0);
    }

    #[test]
    fn decsc_decrc_restores_cursor_and_colors() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(8, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1006h");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::Sgr);
        term.process(b"\x1b[?1006l");
        assert_eq!(term.modes.mouse_encoding, MouseEncoding::Default);
    }

    #[test]
    fn decset_1002_enables_button_event_tracking() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1002h");
        assert_eq!(term.modes.mouse_tracking, MouseTracking::ButtonEvent);
        term.process(b"\x1b[?1002l");
        assert_eq!(term.modes.mouse_tracking, MouseTracking::Off);
    }

    #[test]
    fn tracking_mode_is_replaced_not_layered() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1000h");
        term.process(b"\x1b[?1003h");
        assert_eq!(term.modes.mouse_tracking, MouseTracking::AnyEvent);
    }

    #[test]
    fn mouse_report_emits_into_pending_output() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
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
    fn mouse_report_uses_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?1000h\x1b[?1006h\x1b G");
        let emitted = term.mouse_report(
            MouseEventKind::Press,
            MouseButton::Left,
            4,
            9,
            MouseModifiers::default(),
        );
        assert!(emitted);
        assert_eq!(term.take_pending_output(), b"\x9b<0;5;10M");
    }

    #[test]
    fn mouse_report_returns_false_when_tracking_off() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
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
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.paste("hello\n");
        assert_eq!(term.take_pending_output(), b"hello\n");
    }

    #[test]
    fn paste_wraps_when_mode_2004_enabled() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        assert!(term.modes.bracketed_paste);
        term.paste("hello\n");
        assert_eq!(term.take_pending_output(), b"\x1b[200~hello\n\x1b[201~");
    }

    #[test]
    fn paste_wraps_with_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h\x1b G");
        term.paste("hello\n");
        assert_eq!(term.take_pending_output(), b"\x9b200~hello\n\x9b201~");
    }

    #[test]
    fn decrst_2004_disables_bracketed_paste() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2004h");
        term.process(b"\x1b[?2004l");
        assert!(!term.modes.bracketed_paste);
        term.paste("hi");
        assert_eq!(term.take_pending_output(), b"hi");
    }

    #[test]
    fn paste_scrubs_embedded_end_marker() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
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
                image: image41::DecodedImage::single_frame(1, height_px, vec![]),
                id,
                row,
                col,
                display_width: 1,
                display_height: height_px,
                placed_at: Instant::now(),
            },
        );
        id
    }

    #[test]
    fn sixel_redraw_at_same_position_replaces_previous() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        // cell_height = 16, so 32px = 2 grid rows.
        let id_a = place_image(&mut term, 5, 0, 32);
        self::image::remove_overlapping(&mut term.active.images, 5, 2, 0, 16);
        // The manual sweep used by the Unhook handler — call it to verify
        // the behavior the handler relies on.
        assert!(!term.active.images.contains_key(&id_a));
    }

    #[test]
    fn sixel_different_columns_coexist() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        let id_a = place_image(&mut term, 5, 0, 32);
        let id_b = place_image(&mut term, 5, 10, 32);
        // Dedup sweep for a new image at col 0 must not touch col 10.
        self::image::remove_overlapping(&mut term.active.images, 5, 2, 0, 16);
        assert!(!term.active.images.contains_key(&id_a));
        assert!(term.active.images.contains_key(&id_b));
    }

    #[test]
    fn scroll_region_shifts_images_up() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
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
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
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
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        term.process(b"\x1b[2;5r"); // region rows 1..=4 (abs 1..=4 with no scrollback)
        let id = place_image(&mut term, 8, 0, 16); // below region
        term.process(b"\x1b[2H"); // move into region
        term.process(b"\x1b[2M"); // scroll up inside region
        let img = term.active.images.get(&id).expect("image retained");
        assert_eq!(img.row, 8, "image outside region is unaffected");
    }

    #[test]
    fn ed_2_removes_visible_images() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
        let id = place_image(&mut term, 3, 0, 16);
        term.process(b"\x1b[2J"); // ED 2 — clear entire screen
        assert!(
            !term.active.images.contains_key(&id),
            "ED 2 should drop images on the visible area"
        );
    }

    #[test]
    fn alt_screen_entry_clears_alt_images() {
        let mut term = TestTerm::new(10, 10, 0, 16, 8);
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
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        assert!(!term.is_synchronized_update_active());
        term.process(b"\x1b[?2026h");
        assert!(term.is_synchronized_update_active());
    }

    #[test]
    fn esu_clears_synchronized_update_flag() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2026h");
        term.process(b"\x1b[?2026l");
        assert!(!term.is_synchronized_update_active());
        assert!(term.modes.synchronized_update_since.is_none());
    }

    #[test]
    fn synchronized_update_expires_after_timeout() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?2026h");
        // Back-date the start so the safety deadline has already passed —
        // avoids a real sleep in the test but exercises the timeout path.
        term.modes.synchronized_update_since =
            Some(Instant::now() - SYNCHRONIZED_UPDATE_TIMEOUT - Duration::from_millis(1));
        assert!(!term.is_synchronized_update_active());
    }

    #[test]
    fn paste_from_clipboard_round_trips() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        term.clipboard.set(ClipboardKind::Clipboard, "hello");
        term.paste_from_clipboard(ClipboardKind::Clipboard);
        assert_eq!(term.take_pending_output(), b"hello");
    }

    #[test]
    fn paste_from_clipboard_ignores_empty_selection() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        term.paste_from_clipboard(ClipboardKind::Clipboard);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Selection ----

    fn write_row(
        term: &mut TestTerm,
        screen_row: u32,
        text: &str,
    ) {
        term.process(format!("\x1b[{};1H", screen_row + 1).as_bytes());
        term.process(text.as_bytes());
    }

    #[test]
    fn start_selection_char_mode_is_empty_initially() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.start_selection(2, 1, SelectionMode::Char);
        assert!(term.selection.is_some());
        assert!(!term.has_selection()); // empty Char = not "has selection"
    }

    #[test]
    fn char_selection_extend_produces_text() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(4, 0);
        assert_eq!(term.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn word_selection_snaps_to_boundaries() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.start_selection(2, 0, SelectionMode::Word); // in "hello"
        assert_eq!(term.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn line_selection_covers_full_row() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.start_selection(5, 0, SelectionMode::Line);
        // Line selection trims trailing padding spaces.
        assert_eq!(term.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn selection_spans_rows_with_newline_separator() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abc");
        write_row(&mut term, 1, "def");
        term.start_selection(0, 0, SelectionMode::Char);
        term.extend_selection(2, 1);
        // Intermediate row trims trailing spaces, \n joins hard line breaks.
        assert_eq!(term.selection_text().as_deref(), Some("abc\ndef"));
    }

    #[test]
    fn selection_drags_backwards_flips_anchor_head() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.start_selection(8, 0, SelectionMode::Word); // in "world"
        term.extend_selection(2, 0); // drag back into "hello"
        assert_eq!(term.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn is_cell_selected_matches_contains() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]7;file://localhost/tmp/work\x1b\\");
        assert_eq!(
            term.current_directory.as_deref(),
            Some(std::path::Path::new("/tmp/work"))
        );
    }

    // ---- OSC 8 hyperlinks ----

    #[test]
    fn osc_8_attaches_link_to_subsequent_cells() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\after");
        assert_eq!(term.hyperlink_at(0, 0), Some("https://example.com"));
        assert_eq!(term.hyperlink_at(0, 3), Some("https://example.com"));
        // First cell after the closing OSC 8 carries no link.
        assert_eq!(term.hyperlink_at(0, 4), None);
    }

    #[test]
    fn osc_8_close_clears_current_link() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]8;;https://example.com\x1b\\");
        assert!(term.active.current_hyperlink.is_some());
        term.process(b"\x1b]8;;\x1b\\");
        assert!(term.active.current_hyperlink.is_none());
    }

    // ---- Kitty keyboard protocol ----

    #[test]
    fn kitty_push_records_flags() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>1u");
        assert_eq!(
            term.kitty_keyboard.current(),
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
        );
    }

    #[test]
    fn kitty_pop_default_unwinds_one_frame() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>1u\x1b[<u");
        assert!(term.kitty_keyboard.current().is_empty());
    }

    #[test]
    fn kitty_query_writes_response_to_pending_output() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>3u\x1b[?u");
        assert_eq!(term.take_pending_output(), b"\x1b[?3u");
    }

    // ---- Cursor style (DECSCUSR) ----

    #[test]
    fn decscusr_sets_steady_block() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.report_focus_change(true);
        assert!(term.take_pending_output().is_empty());
    }

    #[test]
    fn focus_change_emits_csi_i_o_when_enabled() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h");
        term.report_focus_change(true);
        term.report_focus_change(false);
        assert_eq!(term.take_pending_output(), b"\x1b[I\x1b[O");
    }

    #[test]
    fn focus_change_uses_8bit_csi_after_s8c1t() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b G");
        term.report_focus_change(true);
        term.report_focus_change(false);
        assert_eq!(term.take_pending_output(), b"\x9bI\x9bO");
    }

    #[test]
    fn decrst_1004_disables_focus_reporting() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?1004h\x1b[?1004l");
        term.report_focus_change(true);
        assert!(term.take_pending_output().is_empty());
    }

    // ---- Live config reload effects ----

    // ---- Title (OSC 0 / OSC 2) ----

    #[test]
    fn osc_2_updates_terminal_title() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]2;build ok\x1b\\");
        assert_eq!(term.current_title.as_deref(), Some("build ok"));
    }

    #[test]
    fn osc_0_updates_terminal_title() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]0;hi\x1b\\");
        assert_eq!(term.current_title.as_deref(), Some("hi"));
    }

    // ---- Bell ----

    #[test]
    fn bel_byte_sets_bell_pending() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        assert!(!term.take_bell_pending());
        term.process(b"\x07");
        assert!(term.take_bell_pending());
        // Take is destructive — second poll within the same frame returns false.
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

    // ---- Live config reload ----

    #[test]
    fn set_scrollback_limit_takes_effect_on_next_push() {
        let mut term = TestTerm::new(8, 2, 100, 16, 8);
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
        term: &mut TestTerm,
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
        let mut term = TestTerm::new(10, 6, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
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
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
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
        let mut term = TestTerm::new(10, 4, 200, 16, 8);
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
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
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
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
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
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
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

    // ---- DECTCEM cursor visibility (?25) ----

    #[test]
    fn dectcem_hides_and_shows_cursor() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        assert!(term.active.cursor_visible, "default must be visible");
        term.process(b"\x1b[?25l");
        assert!(!term.active.cursor_visible);
        term.process(b"\x1b[?25h");
        assert!(term.active.cursor_visible);
    }

    #[test]
    fn dectcem_state_is_per_screen() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[?25l"); // hide on primary
        term.process(b"\x1b[?1049h"); // switch to alt
        // Alt starts with its own default (visible) — hiding the cursor on
        // the primary screen must not bleed through to alt.
        assert!(term.active.cursor_visible);
        term.process(b"\x1b[?1049l"); // back to primary
        // Primary's hidden state survives the round trip.
        assert!(!term.active.cursor_visible);
    }

    // ---- Device Attribute queries ----

    #[test]
    fn da1_replies_vt420() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?64;7;21;22;28;29c");
    }

    #[test]
    fn da1_with_zero_param_also_replies() {
        // Apps sometimes send `CSI 0 c` explicitly; the reply is the same.
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[0c");
        assert_eq!(term.take_pending_output(), b"\x1b[?64;7;21;22;28;29c");
    }

    #[test]
    fn da2_replies_as_vt420_compatible() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>c");
        assert_eq!(term.take_pending_output(), b"\x1b[>41;0;0c");
    }

    #[test]
    fn decscl_level1_changes_da1_prefix_and_resets_screen() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"hello\x1b[?1004h\x1b[61\"p");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level1);
        assert_eq!(term.modes.c1_mode, C1Mode::SevenBit);
        assert!(!term.modes.focus_reporting);
        term.process(b"\x1b[c");
        assert_eq!(term.take_pending_output(), b"\x1b[?61;7;21;22;28;29c");
        for r in term.active.grid.rows.iter().rev().take(3) {
            assert_eq!(r.content_len(), 0);
        }
    }

    #[test]
    fn decscl_with_8bit_controls_switches_reply_encoding() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[64;2\"p\x1b[>c");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level4);
        assert_eq!(term.modes.c1_mode, C1Mode::EightBit);
        assert_eq!(term.take_pending_output(), b"\x9b>41;0;0c");
    }

    #[test]
    fn s8c1t_is_ignored_in_level1_mode() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[61\"p\x1b G\x1b[>c");
        assert_eq!(term.modes.conformance_level, ConformanceLevel::Level1);
        assert_eq!(term.modes.c1_mode, C1Mode::SevenBit);
        assert_eq!(term.take_pending_output(), b"\x1b[>41;0;0c");
    }

    #[test]
    fn xtversion_replies_with_name_and_version() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b[>0q");
        let expected = format!("\x1bP>|term41 {}\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(term.take_pending_output(), expected.as_bytes());
    }

    // ---- RIS (ESC c) ----

    #[test]
    fn ris_clears_visible_and_resets_cursor() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"hello\x1b[5;5H"); // print + move cursor
        term.process(b"\x1bc");
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        // Visible content is gone.
        for r in term.active.grid.rows.iter().rev().take(3) {
            assert_eq!(r.content_len(), 0);
        }
    }

    #[test]
    fn ris_returns_to_primary_screen() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"\x1b[?1049h");
        assert!(term.on_alt_screen);
        term.process(b"\x1bc");
        assert!(!term.on_alt_screen);
    }

    #[test]
    fn ris_resets_modes_the_app_flipped() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.process(b"\x1b[?2004h"); // bracketed paste
        term.process(b"\x1b[?1004h"); // focus reporting
        term.process(b"\x1b[?1000h"); // mouse tracking
        term.process(b"\x1b[?25l"); // hide cursor
        term.process(b"\x1bc");
        assert!(!term.modes.bracketed_paste);
        assert!(!term.modes.focus_reporting);
        assert_eq!(term.modes.mouse_tracking, MouseTracking::Off);
        assert!(term.active.cursor_visible);
    }

    #[test]
    fn ris_preserves_scrollback() {
        // A misbehaving app's reset shouldn't nuke the user's history.
        let mut term = TestTerm::new(4, 2, 100, 16, 8);
        for _ in 0..5 {
            term.process(b"x\r\n");
        }
        let rows_before = term.active.grid.rows.len();
        assert!(rows_before > 2, "setup should have produced scrollback");
        term.process(b"\x1bc");
        // Rows count stays the same — visible area cleared in place, history kept.
        assert_eq!(term.active.grid.rows.len(), rows_before);
    }

    #[test]
    fn deccolm_ignored_without_mode_40() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"\x1b[?3h");
        // DECCOLM is gated by mode 40 — without it, the resize is ignored.
        assert_eq!(term.viewport.cols, 80);
    }

    #[test]
    fn deccolm_set_resizes_to_132_and_clears() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        term.process(b"hello");
        assert_eq!(term.viewport.cols, 80);
        // Enable mode 40 (allow DECCOLM), then set DECCOLM.
        term.process(b"\x1b[?40h\x1b[?3h");
        assert_eq!(term.viewport.cols, 132);
        assert_eq!(term.active.cursor.row, 0);
        assert_eq!(term.active.cursor.col, 0);
        assert_eq!(term.active.scroll_top, 0);
        // First visible row should be blank (cleared).
        let first_vis = term.active.grid.rows.len() - 24;
        assert_eq!(term.active.grid.rows[first_vis].cells[0].as_str(), " ");
    }

    #[test]
    fn deccolm_reset_restores_original_width() {
        let mut term = TestTerm::new(80, 24, 100, 16, 8);
        // Enable mode 40 first.
        term.process(b"\x1b[?40h");
        term.process(b"\x1b[?3h"); // 132 cols
        assert_eq!(term.viewport.cols, 132);
        term.process(b"\x1b[?3l"); // back to 80
        assert_eq!(term.viewport.cols, 80);
        assert_eq!(term.active.cursor.row, 0);
    }

    #[test]
    fn terminal_batch_budget_trips_on_time_limit() {
        let start = std::time::Instant::now() - TERMINAL_BATCH_TIME_BUDGET;
        assert!(terminal_batch_budget_exhausted(start));
    }
}

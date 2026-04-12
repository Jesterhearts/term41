use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::ops::RangeBounds;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use palette::Srgb;

use crate::clipboard::Clipboard;
use crate::clipboard::ClipboardKind;
use crate::selection::Selection;
use crate::selection::SelectionMode;
use crate::selection::SelectionPoint;
use crate::selection::expand_to_line;
use crate::selection::expand_to_word;
use crate::sixel::SixelImage;
use crate::sixel::parse_sixel;
use crate::vte;
use crate::vte::Params;

pub const fn default_fg() -> Srgb<u8> {
    Srgb::new(204, 204, 204)
}

pub const fn default_bg() -> Srgb<u8> {
    Srgb::new(0, 0, 0)
}

/// The standard 256-color palette.
const fn ansi_color(index: u8) -> Srgb<u8> {
    match index {
        0 => Srgb::new(0, 0, 0),
        1 => Srgb::new(205, 0, 0),
        2 => Srgb::new(0, 205, 0),
        3 => Srgb::new(205, 205, 0),
        4 => Srgb::new(0, 0, 238),
        5 => Srgb::new(205, 0, 205),
        6 => Srgb::new(0, 205, 205),
        7 => Srgb::new(229, 229, 229),
        8 => Srgb::new(127, 127, 127),
        9 => Srgb::new(255, 0, 0),
        10 => Srgb::new(0, 255, 0),
        11 => Srgb::new(255, 255, 0),
        12 => Srgb::new(92, 92, 255),
        13 => Srgb::new(255, 0, 255),
        14 => Srgb::new(0, 255, 255),
        15 => Srgb::new(255, 255, 255),
        16..=231 => {
            const fn to_val(c: u8) -> u8 {
                if c == 0 { 0 } else { 55 + 40 * c }
            }

            let idx = index - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            Srgb::new(to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Srgb::new(v, v, v)
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub image: SixelImage,
    pub id: u64,
    /// Absolute row index in `grid.rows` where the image top-left is placed.
    pub row: usize,
    /// Column position of the image top-left.
    pub col: u32,
}

/// A reference to an image visible in the current viewport.
pub struct VisibleImage<'a> {
    pub image: &'a SixelImage,
    pub id: u64,
    /// Row relative to the top of the viewport (0 = top).
    pub screen_row: u32,
    /// Column position.
    pub screen_col: u32,
}

/// A terminal row stored as struct-of-arrays for cache-friendly access.
/// The renderer can borrow `&[char]` directly for shaping without copying.
#[derive(Debug, Default)]
pub struct Row {
    pub chars: Vec<char>,
    pub fg: Vec<Srgb<u8>>,
    pub bg: Vec<Srgb<u8>>,
    /// True if this row is a continuation of the previous row (soft wrap).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: u32) -> Self {
        let n = cols as usize;
        Self {
            chars: vec![' '; n],
            fg: vec![default_fg(); n],
            bg: vec![default_bg(); n],
            wrapped: false,
        }
    }

    fn len(&self) -> u32 {
        self.chars.len() as u32
    }

    fn content_len(&self) -> u32 {
        if self.wrapped {
            self.len()
        } else {
            self.chars
                .iter()
                .rposition(|c| *c != ' ')
                .map_or(0, |p| p + 1) as u32
        }
    }

    fn resize(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.chars.resize(new_len, ' ');
        self.fg.resize(new_len, default_fg());
        self.bg.resize(new_len, default_bg());
    }

    fn truncate(
        &mut self,
        new_len: u32,
    ) {
        let new_len = new_len as usize;
        self.chars.truncate(new_len);
        self.fg.truncate(new_len);
        self.bg.truncate(new_len);
    }

    fn clear(&mut self) {
        self.clear_range(0..self.chars.len())
    }

    fn clear_range(
        &mut self,
        range: std::ops::Range<usize>,
    ) {
        self.chars[range.clone()].fill(' ');
        self.fg[range.clone()].fill(default_fg());
        self.bg[range].fill(default_bg());
    }

    fn copy_within<R>(
        &mut self,
        src: R,
        dest: usize,
    ) where
        R: RangeBounds<usize> + Clone,
    {
        self.chars.copy_within(src.clone(), dest);
        self.fg.copy_within(src.clone(), dest);
        self.bg.copy_within(src, dest);
    }

    fn copy_from(
        &mut self,
        other: &Self,
        src: std::ops::Range<usize>,
        dest_offset: usize,
    ) -> usize {
        let copy_len = ((other.content_len() as usize).saturating_sub(src.start))
            .min((self.len() as usize).saturating_sub(dest_offset))
            .min(src.len());
        self.chars[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.chars[src.start..src.start + copy_len]);
        self.fg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.fg[src.start..src.start + copy_len]);
        self.bg[dest_offset..dest_offset + copy_len]
            .copy_from_slice(&other.bg[src.start..src.start + copy_len]);

        copy_len
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub col: u32,
    pub row: u32,
}

/// Snapshot of cursor position and active colors, used by DECSC/DECRC
/// (ESC 7 / ESC 8) and the `?1048`/`?1049` private modes.
#[derive(Debug, Clone, Copy)]
pub struct SavedCursor {
    pub cursor: Cursor,
    pub fg: Srgb<u8>,
    pub bg: Srgb<u8>,
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
}

impl Screen {
    fn new(
        cols: u32,
        rows: u32,
        scrollback_limit: u32,
    ) -> Self {
        let mut grid_rows = VecDeque::with_capacity(rows as usize + scrollback_limit as usize);
        for _ in 0..rows {
            grid_rows.push_back(Row::new(cols));
        }
        Self {
            grid: Grid {
                rows: grid_rows,
                scrollback_limit,
                total_popped: 0,
            },
            cursor: Cursor::default(),
            fg: default_fg(),
            bg: default_bg(),
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            offset: 0,
            images: BTreeMap::new(),
            saved_cursor: None,
        }
    }
}

#[derive(Debug)]
pub struct Grid {
    pub rows: VecDeque<Row>,
    pub scrollback_limit: u32,
    /// Running count of rows popped from the front (for image position
    /// tracking).
    pub total_popped: usize,
}

impl Grid {
    pub fn scrollback_len(
        &self,
        viewport: &Viewport,
    ) -> u32 {
        (self.rows.len() as u32).saturating_sub(viewport.rows)
    }

    pub fn push_visible_row(
        &mut self,
        viewport: &Viewport,
    ) {
        self.rows.push_back(Row::new(viewport.cols));

        let max_rows = viewport.rows as usize + self.scrollback_limit as usize;
        if self.rows.len() > max_rows {
            self.rows.pop_front();
            self.total_popped += 1;
        }
    }

    pub fn erase_in_display(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let first_visible = self.rows.len() - viewport.rows as usize;
        let col = cursor.col as usize;

        match mode {
            // Erase from cursor to end of screen.
            0 => {
                let cols = self.rows[active].chars.len();
                self.rows[active].clear_range(col..cols);
                for r in (active + 1)..self.rows.len() {
                    self.rows[r].clear();
                }
            }
            // Erase from start of screen to cursor (inclusive).
            1 => {
                for r in first_visible..active {
                    self.rows[r].clear();
                }
                self.rows[active].clear_range(0..col + 1);
            }
            // Erase entire screen.
            2 => {
                for r in first_visible..self.rows.len() {
                    self.rows[r].clear();
                }
            }
            // Erase scrollback buffer.
            3 => {
                self.total_popped += first_visible;
                self.rows.drain(0..first_visible);
            }
            _ => {}
        }
    }

    pub fn erase_in_line(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        mode: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;

        match mode {
            // Erase from cursor to end of line.
            0 => self.rows[active].clear_range(col..cols),
            // Erase from start of line to cursor (inclusive).
            1 => self.rows[active].clear_range(0..col + 1),
            // Erase entire line.
            2 => self.rows[active].clear(),
            _ => {}
        }
    }

    pub fn delete_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;
        let count = (n as usize).min(cols - col);

        self.rows[active].copy_within(col + count..cols, col);
        self.rows[active].clear_range(cols - count..cols);
    }

    pub fn insert_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;
        let count = (n as usize).min(cols - col);

        self.rows[active].copy_within(col..cols - count, col + count);
        self.rows[active].clear_range(col..col + count);
    }

    pub fn erase_chars(
        &mut self,
        cursor: &Cursor,
        viewport: &Viewport,
        n: u16,
    ) {
        let active = self.active_row_index(cursor, viewport);
        let cols = self.rows[active].chars.len();
        let col = cursor.col as usize;
        let end = (col + n as usize).min(cols);

        self.rows[active].clear_range(col..end);
    }

    /// Scroll content up within a region: remove line at `top`, insert blank
    /// at `bottom`. Both are viewport-relative row indices.
    fn scroll_up_in_region(
        &mut self,
        viewport: &Viewport,
        top: u32,
        bottom: u32,
        n: u32,
    ) {
        let first_visible = self.rows.len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        for _ in 0..n {
            self.rows.remove(abs_top);
            self.rows.insert(abs_bottom, Row::new(viewport.cols));
        }
    }

    /// Scroll content down within a region: insert blank at `top`, remove
    /// line at `bottom`. Both are viewport-relative row indices.
    fn scroll_down_in_region(
        &mut self,
        viewport: &Viewport,
        top: u32,
        bottom: u32,
        n: u32,
    ) {
        let first_visible = self.rows.len() - viewport.rows as usize;
        let abs_top = first_visible + top as usize;
        let abs_bottom = first_visible + bottom as usize;
        let n = (n as usize).min(abs_bottom - abs_top + 1);
        for _ in 0..n {
            self.rows.remove(abs_bottom);
            self.rows.insert(abs_top, Row::new(viewport.cols));
        }
    }

    pub fn active_row_index(
        &self,
        cursor: &Cursor,
        viewport: &Viewport,
    ) -> usize {
        self.rows.len() - viewport.rows as usize + cursor.row as usize
    }

    fn reflow(
        &mut self,
        new_width: u32,
    ) {
        if self.rows.is_empty() {
            return;
        }

        if self.rows[0].len() == new_width {
            return;
        }

        if new_width > self.rows[0].len() {
            let new_width = new_width as usize;
            let mut dst = 0;
            let mut dst_col = self.rows[0].content_len() as usize;
            let mut src = 1;
            let mut src_col: usize = 0;

            while dst < self.rows.len() && src < self.rows.len() {
                self.rows[dst].resize(new_width as u32);

                if !self.rows[dst].wrapped {
                    dst += 1;
                    dst_col = if dst == src && self.rows[dst].wrapped {
                        self.rows[dst].content_len() as usize
                    } else {
                        0
                    };
                    if dst == src {
                        src += 1;
                    }
                    continue;
                }

                // Pull one chunk from src into dst.
                let (d, s) = self.split_current_next(dst, src);
                let s_content = s.content_len() as usize;
                let n = d.copy_from(s, src_col..s_content, dst_col);
                dst_col += n;
                src_col += n;

                // If src exhausted: inherit its wrap state, clear it, advance.
                if src_col >= s_content {
                    d.wrapped = s.wrapped;
                    s.clear();
                    s.wrapped = true;
                    src += 1;
                    src_col = 0;
                }

                // If dst full: advance to next row.
                if dst_col >= new_width {
                    if src_col > 0 {
                        self.rows[dst].wrapped = true;
                    }
                    dst += 1;
                    dst_col = 0;
                    if dst == src {
                        // Collision: dst caught up to partially-consumed src.
                        // Shift remaining content to front and advance src.
                        self.rows[dst].copy_within(src_col.., 0);
                        let len = self.rows[dst].len() as usize;
                        self.rows[dst].clear_range(len - src_col..len);
                        dst_col = len - src_col;
                        src += 1;
                        src_col = 0;
                    }
                }
            }

            self.rows[dst].resize(new_width as u32);
            self.rows
                .truncate(dst + if self.rows[dst].wrapped { 0 } else { 1 });
        } else {
            let mut row = 0;
            while row < self.rows.len() {
                if self.rows[row].len() > new_width {
                    if self.rows[row].content_len() > new_width {
                        let overflow = Row {
                            chars: self.rows[row].chars.split_off(new_width as usize),
                            fg: self.rows[row].fg.split_off(new_width as usize),
                            bg: self.rows[row].bg.split_off(new_width as usize),
                            wrapped: self.rows[row].wrapped,
                        };

                        self.rows[row].wrapped = true;
                        self.rows.insert(row + 1, overflow);
                    } else {
                        self.rows[row].wrapped = false;
                        self.rows[row].truncate(new_width);
                    }
                } else {
                    let mut content = self.rows[row].len() as usize;
                    self.rows[row].resize(new_width);

                    // Pull content from continuation rows to fill space left
                    // by a short overflow. This maintains the invariant that
                    // only the last row in a wrapped sequence is partially
                    // filled.
                    while self.rows[row].wrapped && row + 1 < self.rows.len() {
                        let room = new_width as usize - content;
                        if room == 0 {
                            break;
                        }

                        let next = row + 1;
                        let next_content = self.rows[next].content_len() as usize;
                        let to_copy = room.min(next_content);

                        if to_copy > 0 {
                            let (dst, src) = self.split_current_next(row, next);
                            dst.chars[content..content + to_copy]
                                .copy_from_slice(&src.chars[..to_copy]);
                            dst.fg[content..content + to_copy].copy_from_slice(&src.fg[..to_copy]);
                            dst.bg[content..content + to_copy].copy_from_slice(&src.bg[..to_copy]);
                        }

                        if to_copy >= next_content {
                            // Fully consumed the next row — inherit its wrap
                            // state and remove it.
                            let next_wrapped = self.rows[next].wrapped;
                            self.rows.remove(next);
                            self.rows[row].wrapped = next_wrapped;
                            content += to_copy;
                        } else {
                            // Partially consumed — shift remaining content left
                            // and trim to its new length so the main loop can
                            // process it correctly.
                            self.rows[next].copy_within(to_copy.., 0);
                            let remaining = self.rows[next].len() as usize - to_copy;
                            self.rows[next].truncate(remaining as u32);
                            break;
                        }
                    }
                }
                row += 1;
            }
        }
    }

    fn split_current_next(
        &mut self,
        row: usize,
        next: usize,
    ) -> (&mut Row, &mut Row) {
        let (front, back) = self.rows.as_mut_slices();

        if row < front.len() && next >= front.len() {
            let next = next - front.len();
            (&mut front[row], &mut back[next])
        } else if next < front.len() && row >= front.len() {
            (&mut back[row - front.len()], &mut front[next])
        } else if next < front.len() {
            let (first, second) = front.split_at_mut(next);
            (&mut first[row], &mut second[0])
        } else {
            let (first, second) = back.split_at_mut(next - front.len());
            (&mut first[row - front.len()], &mut second[0])
        }
    }
}

/// Dimensions of the rendered terminal window, shared by both screens.
/// Per-screen state (scroll region, scrollback offset) lives on [`Screen`].
#[derive(Debug, Default)]
pub struct Viewport {
    pub rows: u32,
    pub cols: u32,
}

/// Terminal state.
///
/// Holds two [`Screen`] buffers — the `active` one receives output and is
/// rendered; the `stash` holds whichever screen isn't currently live.
/// DECSET ?47 / ?1047 / ?1049 swap the two with a single [`std::mem::swap`].
/// DEC mouse-tracking mode currently requested by the foreground app.
///
/// Layered in the order the spec describes — each higher variant is a
/// superset of the one above, though we model them as distinct states so the
/// reporter can filter motion appropriately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseTracking {
    /// No mouse events are forwarded.
    Off,
    /// Mode 9. Press events only.
    X10,
    /// Mode 1000. Press and release, no motion.
    Normal,
    /// Mode 1002. Press, release, and motion while a button is held.
    ButtonEvent,
    /// Mode 1003. Press, release, and all motion regardless of buttons.
    AnyEvent,
}

/// On-the-wire encoding for mouse events. The app selects these with
/// DECSET ?1005/?1006/?1015.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEncoding {
    /// Legacy xterm `CSI M Cb Cx Cy` with each byte offset by 32. Cells
    /// beyond column/row 223 saturate, so modern apps prefer SGR.
    Default,
    /// Mode 1005. Same shape as Default but each field is UTF-8 encoded.
    Utf8,
    /// Mode 1006. `CSI < Pb ; Px ; Py M|m` — trailing `m` signals release.
    Sgr,
    /// Mode 1015. `CSI Pb ; Px ; Py M` — decimal, no angle bracket, release
    /// encoded with button code 3.
    Urxvt,
}

/// Kind of event the app is being told about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Motion,
}

/// Physical button that originated the event. `None` is used for motion
/// reports when no button is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    None,
}

/// Keyboard modifiers captured alongside a mouse event.
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
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
}

/// Save image positions as logical-line anchors that survive reflow.
///
/// Each image is mapped to (id, logical_lines_below, row_offset_in_line).
/// The count of hard line boundaries between the image and the grid end is
/// invariant through reflow, so it can be used to relocate the image after.
fn anchor_images(
    rows: &VecDeque<Row>,
    images: &BTreeMap<u64, PlacedImage>,
) -> Vec<(u64, usize, usize)> {
    images
        .values()
        .map(|img| {
            let lines_below = (img.row + 1..rows.len())
                .filter(|&r| !rows[r].wrapped)
                .count();

            let mut row_offset = 0;
            let mut r = img.row;
            while r > 0 && rows[r].wrapped {
                row_offset += 1;
                r -= 1;
            }

            (img.id, lines_below, row_offset)
        })
        .collect()
}

/// Restore image row positions from logical-line anchors produced by
/// [`anchor_images`]. Images whose logical line was trimmed away are removed.
fn restore_images(
    rows: &VecDeque<Row>,
    anchors: &[(u64, usize, usize)],
    images: &mut BTreeMap<u64, PlacedImage>,
) {
    for &(id, lines_below, row_offset) in anchors {
        let mut count = 0;
        let mut found = None;
        for r in (0..rows.len()).rev() {
            if r == 0 || !rows[r].wrapped {
                if count == lines_below {
                    found = Some(r);
                    break;
                }
                count += 1;
            }
        }

        match found {
            Some(start) => {
                let mut end = start + 1;
                while end < rows.len() && rows[end].wrapped {
                    end += 1;
                }
                let new_row = start + row_offset.min(end - start - 1);
                if let Some(img) = images.get_mut(&id) {
                    img.row = new_row;
                }
            }
            None => {
                images.remove(&id);
            }
        }
    }
}

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
        }
    }

    // ---- Selection ------------------------------------------------------

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
            let row_len_cols = row.chars.len() as u32;
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

            let segment: String = row.chars[col_start as usize..=col_end as usize]
                .iter()
                .collect();
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

    /// Return images whose top-left falls within the current viewport,
    /// with screen-relative row/col positions.
    pub fn visible_images(&self) -> impl Iterator<Item = VisibleImage<'_>> {
        let viewport_top =
            self.active.grid.rows.len() - self.viewport.rows as usize - self.active.offset as usize;
        let viewport_bottom = viewport_top + self.viewport.rows as usize;

        self.active.images.values().filter_map(move |img| {
            if img.row >= viewport_top && img.row < viewport_bottom {
                Some(VisibleImage {
                    image: &img.image,
                    id: img.id,
                    screen_row: (img.row - viewport_top) as u32,
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
                vte::Action::Print(c) => put_char(&mut self.active, &self.viewport, c),
                vte::Action::Execute(byte) => execute(&mut self.active, &self.viewport, byte),
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
                    handle_osc(&data, &mut self.clipboard, &mut self.pending_output)
                }
                vte::Action::Hook { params, action } => {
                    self.hook_bytes.push(vec![]);
                    self.hook_params.push(params);
                    self.hook_action.push(action);
                }
                vte::Action::Put(byte) => {
                    if let Some(last) = self.hook_bytes.last_mut() {
                        last.push(byte);
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

/// Resize a single screen to new dimensions.
///
/// Reflows soft-wrapped lines when the column count changes, preserves
/// image positions through the reflow via logical-line anchors, clamps
/// the cursor into the new bounds, and resets the scroll region / offset
/// to fit the new viewport.
fn resize_screen(
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
            grid.rows.push_back(Row::new(new_cols));
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
            grid.rows.push_back(Row::new(new_cols));
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
    let scrollback = screen.grid.scrollback_len(&Viewport {
        rows: new_rows,
        cols: new_cols,
    });
    screen.offset = screen.offset.min(scrollback);
}

fn put_char(
    screen: &mut Screen,
    viewport: &Viewport,
    ch: char,
) {
    let fg = screen.fg;
    let bg = screen.bg;

    if screen.cursor.col >= viewport.cols {
        // Soft wrap: mark the current row as a continuation.
        screen.cursor.col = 0;
        let r = screen.grid.active_row_index(&screen.cursor, viewport);
        screen.grid.rows[r].wrapped = true;
        if screen.cursor.row == screen.scroll_bottom {
            if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                screen.grid.push_visible_row(viewport);
            } else {
                screen.grid.scroll_up_in_region(
                    viewport,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    1,
                );
            }
        } else if screen.cursor.row < viewport.rows - 1 {
            screen.cursor.row += 1;
        }
    }

    // New output resets the viewport to the live edge.
    screen.offset = 0;

    let r = screen.grid.active_row_index(&screen.cursor, viewport);
    let c = screen.cursor.col as usize;
    screen.grid.rows[r].chars[c] = ch;
    screen.grid.rows[r].fg[c] = fg;
    screen.grid.rows[r].bg[c] = bg;
    screen.cursor.col += 1;
}

fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
) {
    match byte {
        b'\n' => {
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    screen.grid.scroll_up_in_region(
                        viewport,
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
        0x08 => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        b'\t' => {
            let next = (screen.cursor.col / 8 + 1) * 8;
            screen.cursor.col = next.min(viewport.cols - 1);
        }
        0x07 | 0x00 => {}
        _ => {}
    }
}

fn csi_dispatch(
    screen: &mut Screen,
    viewport: &Viewport,
    params: &vte::Params,
    intermediates: &[u8],
    action: char,
) {
    if !intermediates.is_empty() {
        return;
    }

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
            screen.grid.erase_in_display(&screen.cursor, viewport, mode);
        }
        'K' => {
            let mode = p.first().copied().unwrap_or(0);
            screen.grid.erase_in_line(&screen.cursor, viewport, mode);
        }
        'm' => apply_sgr(&mut screen.fg, &mut screen.bg, params),
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
                screen
                    .grid
                    .scroll_down_in_region(viewport, cursor.row, screen.scroll_bottom, n);
            }
        }
        'M' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            if cursor.row >= screen.scroll_top && cursor.row <= screen.scroll_bottom {
                screen
                    .grid
                    .scroll_up_in_region(viewport, cursor.row, screen.scroll_bottom, n);
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
                    screen.scroll_top,
                    screen.scroll_bottom,
                    n,
                );
            }
        }
        'T' => {
            let n = p.first().copied().unwrap_or(1).max(1) as u32;
            screen
                .grid
                .scroll_down_in_region(viewport, screen.scroll_top, screen.scroll_bottom, n);
        }
        'r' => {
            let top = p.first().copied().unwrap_or(1).max(1) as u32 - 1;
            let bottom = p.get(1).copied().unwrap_or(viewport.rows as u16).max(1) as u32 - 1;
            screen.scroll_top = top.min(viewport.rows - 1);
            screen.scroll_bottom = bottom.min(viewport.rows - 1).max(screen.scroll_top);
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
        'n' | 'c' => {}
        _ => {}
    }
}

fn esc_dispatch(
    screen: &mut Screen,
    viewport: &Viewport,
    intermediates: &[u8],
    byte: u8,
) {
    if intermediates.first().is_some_and(|&b| b"()*+".contains(&b)) {
        return;
    }

    match byte {
        b'c' => {
            todo!()
        }
        b'M' => {
            if screen.cursor.row == screen.scroll_top {
                screen.grid.scroll_down_in_region(
                    viewport,
                    screen.scroll_top,
                    screen.scroll_bottom,
                    1,
                );
            } else if screen.cursor.row > 0 {
                screen.cursor.row -= 1;
            }
        }
        b'=' | b'>' => {}
        _ => {}
    }
}

/// Split an OSC payload into its numeric command prefix and the remainder.
///
/// OSC commands have the shape `cmd;args`; when no semicolon is present the
/// whole payload is the command and `args` is empty.
fn split_osc(payload: &[u8]) -> (&[u8], &[u8]) {
    match payload.iter().position(|&b| b == b';') {
        Some(i) => (&payload[..i], &payload[i + 1..]),
        None => (payload, &[]),
    }
}

/// Resolve xterm OSC 52 selector characters into concrete clipboard kinds.
///
/// Selectors: `c` and digits `0`..`7` target the clipboard; `p`, `s`, `q`
/// target the primary selection. An empty selector defaults to the clipboard
/// (matches how most apps use OSC 52 in practice).
fn resolve_selectors(pc: &[u8]) -> Vec<ClipboardKind> {
    let mut seen_clipboard = false;
    let mut seen_primary = false;
    for &b in pc {
        match b {
            b'c' | b'0'..=b'7' => seen_clipboard = true,
            b'p' | b's' | b'q' => seen_primary = true,
            _ => {}
        }
    }
    let mut out = Vec::new();
    if pc.is_empty() || seen_clipboard {
        out.push(ClipboardKind::Clipboard);
    }
    if seen_primary {
        out.push(ClipboardKind::Primary);
    }
    out
}

/// Base64 decode with whitespace stripping — some apps fold long payloads
/// with embedded newlines, and xterm tolerates that.
fn decode_osc52(data: &[u8]) -> Option<Vec<u8>> {
    let filtered: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    BASE64.decode(&filtered).ok()
}

/// Dispatch an OSC payload to the appropriate handler. Unrecognised commands
/// are silently dropped — that's the standard behavior and avoids spurious
/// noise from apps probing for terminal features.
fn handle_osc(
    payload: &[u8],
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
) {
    let (cmd, rest) = split_osc(payload);
    match cmd {
        b"52" => handle_osc_52(rest, clipboard, pending_output),
        _ => {}
    }
}

/// Implements OSC 52 clipboard read/write as used by vim, tmux, etc.
///
/// Format: `OSC 52 ; Pc ; Pd ST` — Pc is one or more selector characters and
/// Pd is either base64-encoded text to copy, or `?` to query the clipboard
/// and have the terminal echo the result back over the PTY.
fn handle_osc_52(
    rest: &[u8],
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
) {
    let (pc, pd) = split_osc(rest);
    let kinds = resolve_selectors(pc);

    if pd == b"?" {
        // Only one response is meaningful even when multiple selectors are
        // requested — pick the first resolved kind.
        let Some(&kind) = kinds.first() else { return };
        let Some(text) = clipboard.get(kind) else {
            return;
        };
        let encoded = BASE64.encode(text.as_bytes());
        let pc_resp: &[u8] = if pc.is_empty() { b"c" } else { pc };
        pending_output.extend_from_slice(b"\x1b]52;");
        pending_output.extend_from_slice(pc_resp);
        pending_output.push(b';');
        pending_output.extend_from_slice(encoded.as_bytes());
        pending_output.extend_from_slice(b"\x1b\\");
        return;
    }

    let Some(decoded) = decode_osc52(pd) else {
        return;
    };
    let Ok(text) = std::str::from_utf8(&decoded) else {
        return;
    };
    for kind in kinds {
        clipboard.set(kind, text);
    }
}

/// Handle DECSET/DECRST bits that drive mouse tracking. Returns true when
/// the mode was a mouse-related one, so the caller knows not to fall
/// through to the generic private-mode handler.
///
/// Tracking modes are modeled as a single state (enabling a new tracking
/// mode replaces the prior one; disabling the tracking bit turns it fully
/// off). That matches how xterm-compatible apps actually use these flags.
fn apply_mouse_mode(
    mode: u16,
    enable: bool,
    tracking: &mut MouseTracking,
    encoding: &mut MouseEncoding,
) -> bool {
    let tracking_target = match mode {
        9 => Some(MouseTracking::X10),
        1000 => Some(MouseTracking::Normal),
        1002 => Some(MouseTracking::ButtonEvent),
        1003 => Some(MouseTracking::AnyEvent),
        _ => None,
    };
    if let Some(target) = tracking_target {
        *tracking = if enable { target } else { MouseTracking::Off };
        return true;
    }

    let encoding_target = match mode {
        1005 => Some(MouseEncoding::Utf8),
        1006 => Some(MouseEncoding::Sgr),
        1015 => Some(MouseEncoding::Urxvt),
        _ => None,
    };
    if let Some(target) = encoding_target {
        *encoding = if enable {
            target
        } else {
            MouseEncoding::Default
        };
        return true;
    }

    false
}

/// Decide whether the given event should be forwarded under the current
/// tracking mode. Release + motion reports under X10, motion reports under
/// Normal, and motion-without-button under ButtonEvent are all suppressed.
fn should_report(
    tracking: MouseTracking,
    kind: MouseEventKind,
    button: MouseButton,
) -> bool {
    match tracking {
        MouseTracking::Off => false,
        MouseTracking::X10 => matches!(kind, MouseEventKind::Press),
        MouseTracking::Normal => matches!(kind, MouseEventKind::Press | MouseEventKind::Release),
        MouseTracking::ButtonEvent => match kind {
            MouseEventKind::Press | MouseEventKind::Release => true,
            MouseEventKind::Motion => !matches!(button, MouseButton::None),
        },
        MouseTracking::AnyEvent => true,
    }
}

/// Numeric button code for the xterm mouse protocol (before adding motion
/// or modifier bits).
fn button_number(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::WheelLeft => 66,
        MouseButton::WheelRight => 67,
    }
}

/// Encode the button/modifier/motion byte (`Cb`) that's common to every
/// protocol. For non-SGR encodings a release collapses to button code 3
/// because there's no other way to distinguish it on the wire.
fn build_mouse_cb(
    encoding: MouseEncoding,
    kind: MouseEventKind,
    button: MouseButton,
    mods: MouseModifiers,
) -> u16 {
    let base = if matches!(kind, MouseEventKind::Release) && !matches!(encoding, MouseEncoding::Sgr)
    {
        3
    } else {
        button_number(button)
    };
    let motion = if matches!(kind, MouseEventKind::Motion) {
        32
    } else {
        0
    };
    let mods = (if mods.shift { 4 } else { 0 })
        | (if mods.alt { 8 } else { 0 })
        | (if mods.ctrl { 16 } else { 0 });
    base + motion + mods
}

/// Append one coordinate byte for the legacy encoding, saturating at 0xFF
/// so we never split a byte range the caller didn't ask for.
fn push_legacy_coord(
    out: &mut Vec<u8>,
    value: u32,
) {
    out.push((value + 32).min(255) as u8);
}

/// Append one coordinate as a UTF-8 code point. Values above U+10FFFF fall
/// back to `?` — realistically unreachable for terminal sizes.
fn push_utf8_coord(
    out: &mut Vec<u8>,
    value: u32,
) {
    match char::from_u32(value + 32) {
        Some(c) => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        None => out.push(b'?'),
    }
}

/// Encode a mouse event using the active protocol and push it into `out`.
fn encode_mouse_event(
    encoding: MouseEncoding,
    kind: MouseEventKind,
    button: MouseButton,
    col_1based: u32,
    row_1based: u32,
    mods: MouseModifiers,
    out: &mut Vec<u8>,
) {
    use std::io::Write as _;

    let cb = build_mouse_cb(encoding, kind, button, mods);

    match encoding {
        MouseEncoding::Sgr => {
            let release = matches!(kind, MouseEventKind::Release);
            let _ = write!(out, "\x1b[<{cb};{col_1based};{row_1based}");
            out.push(if release { b'm' } else { b'M' });
        }
        MouseEncoding::Urxvt => {
            // URXVT adds 32 to Cb just like legacy — the `32` is the xterm
            // legacy bias, not the motion bit, so we apply it here.
            let _ = write!(out, "\x1b[{};{};{}M", cb + 32, col_1based, row_1based);
        }
        MouseEncoding::Default => {
            out.extend_from_slice(b"\x1b[M");
            out.push((cb + 32).min(255) as u8);
            push_legacy_coord(out, col_1based);
            push_legacy_coord(out, row_1based);
        }
        MouseEncoding::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            push_utf8_coord(out, cb as u32);
            push_utf8_coord(out, col_1based);
            push_utf8_coord(out, row_1based);
        }
    }
}

/// Save the active screen's cursor and colors into its DECSC slot
/// (ESC 7 / `?1048h`).
fn save_cursor_slot(screen: &mut Screen) {
    screen.saved_cursor = Some(SavedCursor {
        cursor: screen.cursor,
        fg: screen.fg,
        bg: screen.bg,
    });
}

/// Restore the active screen's cursor and colors from its DECSC slot
/// (ESC 8 / `?1048l`). If the slot is empty the cursor homes to (0, 0)
/// without touching colors — DEC-terminal behavior for an un-saved state.
fn restore_cursor_slot(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    match screen.saved_cursor {
        Some(saved) => {
            screen.cursor.row = saved.cursor.row.min(viewport.rows.saturating_sub(1));
            screen.cursor.col = saved.cursor.col.min(viewport.cols.saturating_sub(1));
            screen.fg = saved.fg;
            screen.bg = saved.bg;
        }
        None => {
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        }
    }
}

/// Clear every cell of the visible area. Leaves any scrollback untouched.
fn clear_visible(
    screen: &mut Screen,
    viewport: &Viewport,
) {
    let first_visible = screen
        .grid
        .rows
        .len()
        .saturating_sub(viewport.rows as usize);
    for r in first_visible..screen.grid.rows.len() {
        screen.grid.rows[r].clear();
    }
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
fn set_private_mode(
    mode: u16,
    enable: bool,
    active: &mut Screen,
    stash: &mut Screen,
    viewport: &Viewport,
    on_alt: &mut bool,
) {
    match mode {
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

/// Apply SGR (Select Graphic Rendition) parameters to the current fg/bg colors.
fn apply_sgr(
    fg: &mut Srgb<u8>,
    bg: &mut Srgb<u8>,
    params: &vte::Params,
) {
    let params: Vec<u16> = params.iter().map(|p| p[0]).collect();

    if params.is_empty() {
        *fg = default_fg();
        *bg = default_bg();
        return;
    }

    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => {
                *fg = default_fg();
                *bg = default_bg();
            }
            30..=37 => *fg = ansi_color((params[i] - 30) as u8),
            39 => *fg = default_fg(),
            40..=47 => *bg = ansi_color((params[i] - 40) as u8),
            49 => *bg = default_bg(),
            90..=97 => *fg = ansi_color((params[i] - 90 + 8) as u8),
            100..=107 => *bg = ansi_color((params[i] - 100 + 8) as u8),
            38 => {
                if let Some(color) = parse_extended_color(&params, &mut i) {
                    *fg = color;
                }
            }
            48 => {
                if let Some(color) = parse_extended_color(&params, &mut i) {
                    *bg = color;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn parse_extended_color(
    params: &[u16],
    i: &mut usize,
) -> Option<Srgb<u8>> {
    if *i + 1 >= params.len() {
        return None;
    }
    match params[*i + 1] {
        5 => {
            if *i + 2 < params.len() {
                *i += 2;
                Some(ansi_color(params[*i] as u8))
            } else {
                None
            }
        }
        2 => {
            if *i + 4 < params.len() {
                *i += 4;
                Some(Srgb::new(
                    params[*i - 2] as u8,
                    params[*i - 1] as u8,
                    params[*i] as u8,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a grid from `(text, wrapped)` pairs. Each row is padded to `width`
    /// with spaces.
    fn make_grid(
        width: u32,
        rows: &[(&str, bool)],
    ) -> Grid {
        let mut grid_rows = VecDeque::new();
        for &(text, wrapped) in rows {
            let mut row = Row::new(width);
            for (i, ch) in text.chars().enumerate() {
                if i < width as usize {
                    row.chars[i] = ch;
                }
            }
            row.wrapped = wrapped;
            grid_rows.push_back(row);
        }
        Grid {
            rows: grid_rows,
            scrollback_limit: 1000,
            total_popped: 0,
        }
    }

    fn row_chars(row: &Row) -> String {
        row.chars.iter().collect()
    }

    // ── Row unit tests ──────────────────────────────────────────────

    #[test]
    fn row_new_filled_with_spaces() {
        let row = Row::new(4);
        assert_eq!(row.chars, vec![' '; 4]);
        assert_eq!(row.fg, vec![default_fg(); 4]);
        assert_eq!(row.bg, vec![default_bg(); 4]);
        assert!(!row.wrapped);
    }

    #[test]
    fn row_len() {
        let row = Row::new(5);
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_grow() {
        let mut row = Row::new(3);
        row.chars[0] = 'a';
        row.chars[1] = 'b';
        row.chars[2] = 'c';
        row.resize(5);
        assert_eq!(row_chars(&row), "abc  ");
        assert_eq!(row.len(), 5);
    }

    #[test]
    fn row_resize_shrink() {
        let mut row = Row::new(5);
        row.chars[0] = 'a';
        row.chars[1] = 'b';
        row.chars[2] = 'c';
        row.resize(2);
        assert_eq!(row_chars(&row), "ab");
    }

    #[test]
    fn row_clear() {
        let mut row = Row::new(3);
        row.chars[0] = 'x';
        row.chars[1] = 'y';
        row.fg[0] = Srgb::new(255, 0, 0);
        row.clear();
        assert_eq!(row.chars, vec![' '; 3]);
        assert_eq!(row.fg, vec![default_fg(); 3]);
    }

    #[test]
    fn row_clear_range() {
        let mut row = Row::new(5);
        for (i, ch) in "abcde".chars().enumerate() {
            row.chars[i] = ch;
        }
        row.clear_range(1..4);
        assert_eq!(row_chars(&row), "a   e");
    }

    #[test]
    fn row_copy_within() {
        let mut row = Row::new(6);
        for (i, ch) in "abcdef".chars().enumerate() {
            row.chars[i] = ch;
        }
        row.copy_within(0..3, 3);
        assert_eq!(row_chars(&row), "abcabc");
    }

    #[test]
    fn row_copy_from() {
        let mut dst = Row::new(6);
        let mut src = Row::new(3);
        for (i, ch) in "xyz".chars().enumerate() {
            src.chars[i] = ch;
        }
        dst.copy_from(&src, 0..3, 2);
        assert_eq!(row_chars(&dst), "  xyz ");
    }

    #[test]
    fn row_copy_from_with_offset() {
        let mut dst = Row::new(5);
        let mut src = Row::new(4);
        for (i, ch) in "abcd".chars().enumerate() {
            src.chars[i] = ch;
        }
        // Copy from src offset 2 to dst offset 0 → copies "cd" (length min(2,5)=2)
        dst.copy_from(&src, 2..4, 0);
        assert_eq!(row_chars(&dst), "cd   ");
    }

    // ── Reflow: grow with no wrapping ───────────────────────────────

    #[test]
    fn reflow_grow_no_wrapping() {
        let mut grid = make_grid(3, &[("abc", false), ("def", false)]);
        grid.reflow(5);
        assert_eq!(row_chars(&grid.rows[0]), "abc  ");
        assert_eq!(row_chars(&grid.rows[1]), "def  ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_same_width_is_noop() {
        let mut grid = make_grid(4, &[("abcd", false), ("efgh", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert_eq!(grid.rows.len(), 2);
    }

    // ── Reflow: grow merges wrapped rows ────────────────────────────

    #[test]
    fn reflow_grow_merges_two_wrapped_rows() {
        // "abcdef" soft-wrapped at width 3 into two rows.
        let mut grid = make_grid(
            3,
            &[
                ("abc", true),
                ("def", false), // continuation
            ],
        );
        // Growing to 6 should merge them into one row.
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_merges_three_wrapped_rows() {
        // "abcdefghi" soft-wrapped at width 3.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(9);
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghi");
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_partial_merge() {
        // "abcdefghi" at width 3, grow to 5.
        // Should become two rows: "abcde" / "fghi_".
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(5);
        assert_eq!(row_chars(&grid.rows[0]), "abcde");
        assert_eq!(row_chars(&grid.rows[1]), "fghi ");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_grow_mixed_wrapped_and_unwrapped() {
        // Two logical lines: "abcdef" (wrapped) then "ghi" (not wrapped).
        let mut grid = make_grid(3, &[("abc", true), ("def", false), ("ghi", false)]);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert_eq!(row_chars(&grid.rows[1]), "ghi   ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_grow_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), then standalone "xx", then "ghijkl" (wrapped).
        let mut grid = make_grid(
            3,
            &[
                ("abc", true),
                ("def", false),
                ("xx ", false),
                ("ghi", true),
                ("jkl", false),
            ],
        );
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert_eq!(row_chars(&grid.rows[1]), "xx    ");
        assert_eq!(row_chars(&grid.rows[2]), "ghijkl");
        assert_eq!(grid.rows.len(), 3);
    }

    // ── Reflow: single row ──────────────────────────────────────────

    #[test]
    fn reflow_single_row_grow() {
        let mut grid = make_grid(3, &[("abc", false)]);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abc   ");
        assert_eq!(grid.rows.len(), 1);
    }

    // ── Reflow: grow collision ────────────────────────────────────

    #[test]
    fn reflow_grow_collision_preserves_line_boundary() {
        // "abcdef" (wrapped at width 3) then "ghi" (unwrapped). Grow to 4.
        // The collision on "def" must not merge content from "ghi".
        let mut grid = make_grid(3, &[("abc", true), ("def", false), ("ghi", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ef  ");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi ");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_grow_collision_continues_when_wrapped() {
        // "abcdefghi" at width 3, grow to 4. Collision on row 1 which IS
        // wrapped — merging should continue through the chain.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "i   ");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    // ── Reflow: shrink splits rows ─────────────────────────────────

    #[test]
    fn reflow_shrink_no_content_overflow() {
        // "abc" and "def" padded to width 6; trailing spaces discarded.
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_full_row() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_splits_into_three() {
        let mut grid = make_grid(9, &[("abcdefghi", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_two_logical_lines() {
        let mut grid = make_grid(6, &[("abcdef", false), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert!(grid.rows[2].wrapped);
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_already_wrapped() {
        // "abcdefghijkl" soft-wrapped at width 6, shrink to 3.
        let mut grid = make_grid(6, &[("abcdef", true), ("ghijkl", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(grid.rows[2].wrapped);
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_uneven_split() {
        // 5 chars into width 3: "abcde" -> "abc" + "de "
        let mut grid = make_grid(5, &[("abcde", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "de ");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_preserves_unwrapped_between_wrapped() {
        // "abcdef" (wrapped), standalone "xx", "ghijkl" (wrapped).
        let mut grid = make_grid(
            6,
            &[("abcdef", false), ("xx    ", false), ("ghijkl", false)],
        );
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert_eq!(row_chars(&grid.rows[2]), "xx ");
        assert_eq!(row_chars(&grid.rows[3]), "ghi");
        assert_eq!(row_chars(&grid.rows[4]), "jkl");
        assert!(grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert!(grid.rows[3].wrapped);
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);
    }

    #[test]
    fn reflow_shrink_pulls_from_continuation() {
        // "abcde" wrapped into "fg" — overflow "de" (len 2) should pull "f"
        // from the continuation row to produce "def".
        let mut grid = make_grid(5, &[("abcde", true), ("fg   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "g  ");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_pull_fully_consumes_next() {
        // Overflow "de" (len 2) pulls "f" from a single-char continuation,
        // fully consuming it.
        let mut grid = make_grid(5, &[("abcde", true), ("f    ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_pull_chains_through_main_loop() {
        // Multiple overflow rows each pull from the next continuation,
        // cascading through the main loop.
        let mut grid = make_grid(4, &[("abcd", true), ("efgh", true), ("ij  ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "j  ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    #[test]
    fn reflow_shrink_pull_preserves_colors() {
        // Color on the next row should land at the right position after pull.
        let mut grid = make_grid(5, &[("abcde", true), ("fg   ", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // 'f' is red
        grid.reflow(3);
        // "def" in row 1 — 'f' is at col 2.
        assert_eq!(grid.rows[1].chars[2], 'f');
        assert_eq!(grid.rows[1].fg[2], red);
    }

    // ── Reflow: trailing space stripping ────────────────────────────

    #[test]
    fn reflow_grow_strips_trailing_spaces() {
        // "ab" with trailing padding on a wrapped row, then "cd".
        let mut grid = make_grid(5, &[("ab   ", true), ("cd   ", false)]);
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "ab   cd   ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_shrink_drops_trailing_space_overflow() {
        // Wrapped row where overflow portion is all spaces — no split needed.
        let mut grid = make_grid(6, &[("abc   ", true), ("def   ", false)]);
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert_eq!(row_chars(&grid.rows[1]), "   ");
        assert_eq!(row_chars(&grid.rows[2]), "def");
        assert!(grid.rows[0].wrapped);
        assert!(grid.rows[1].wrapped);
        assert!(!grid.rows[2].wrapped);
        assert_eq!(grid.rows.len(), 3);
    }

    #[test]
    fn reflow_shrink_grow_maintains_space() {
        let mut grid = make_grid(6, &[("abc   ", false), ("def   ", false)]);
        grid.reflow(3);
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abc   ");
        assert_eq!(row_chars(&grid.rows[1]), "def   ");
        assert!(!grid.rows[0].wrapped);
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_grow_roundtrip_with_trailing_spaces() {
        // Shrink then grow should recover original content, modulo trailing spaces.
        let mut grid = make_grid(10, &[("hello     ", true), ("world     ", false)]);
        grid.reflow(5);
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "hello     ");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "world     ");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    // ── Helpers for scroll region / push_visible_row tests ──────────

    fn make_viewport(
        rows: u32,
        cols: u32,
    ) -> Viewport {
        Viewport { rows, cols }
    }

    /// Build a grid with `scrollback` history rows + `visible` visible rows.
    /// Each row is labeled with a single char repeated to fill the width.
    fn make_grid_with_scrollback(
        width: u32,
        visible: u32,
        labels: &[char],
    ) -> (Grid, Viewport) {
        let vp = make_viewport(visible, width);
        let mut rows = VecDeque::new();
        for &ch in labels {
            let mut row = Row::new(width);
            for c in row.chars.iter_mut() {
                *c = ch;
            }
            rows.push_back(row);
        }
        let grid = Grid {
            rows,
            scrollback_limit: 1000,
            total_popped: 0,
        };
        (grid, vp)
    }

    fn all_chars(grid: &Grid) -> Vec<String> {
        grid.rows.iter().map(row_chars).collect()
    }

    // ── 1. Scroll region tests ──────────────────────────────────────

    #[test]
    fn scroll_up_region_full_viewport() {
        // Scroll up the full viewport: top row removed, blank inserted at bottom.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["BBB", "CCC", "   "]);
    }

    #[test]
    fn scroll_up_region_partial() {
        // Scroll region covers only rows 1-2 of a 4-row viewport.
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_up_in_region(&vp, 1, 2, 1);
        // Row 0 and 3 unchanged; row 1 (B) removed, blank at row 2.
        assert_eq!(all_chars(&grid), vec!["AAA", "CCC", "   ", "DDD"]);
    }

    #[test]
    fn scroll_up_region_n_greater_than_1() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_up_in_region(&vp, 0, 3, 2);
        assert_eq!(all_chars(&grid), vec!["CCC", "DDD", "   ", "   "]);
    }

    #[test]
    fn scroll_up_region_n_clamped_to_region_size() {
        // n=100 but region is only 3 rows, should clamp.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 0, 2, 100);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "   "]);
    }

    #[test]
    fn scroll_down_region_full_viewport() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_down_region_partial() {
        // Scroll region covers only rows 1-2 of a 4-row viewport.
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_down_in_region(&vp, 1, 2, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "BBB", "DDD"]);
    }

    #[test]
    fn scroll_down_region_n_greater_than_1() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 4, &['A', 'B', 'C', 'D']);
        grid.scroll_down_in_region(&vp, 0, 3, 2);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_down_region_n_clamped() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 0, 2, 100);
        assert_eq!(all_chars(&grid), vec!["   ", "   ", "   "]);
    }

    #[test]
    fn scroll_up_region_with_scrollback() {
        // 2 scrollback rows + 3 visible. Scroll region is rows 0-2 of the
        // viewport. Scrollback should be untouched.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['S', 'T', 'A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["SSS", "TTT", "BBB", "CCC", "   "]);
    }

    #[test]
    fn scroll_down_region_with_scrollback() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['S', 'T', 'A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 0, 2, 1);
        assert_eq!(all_chars(&grid), vec!["SSS", "TTT", "   ", "AAA", "BBB"]);
    }

    #[test]
    fn scroll_up_preserves_colors() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // row B, first cell
        grid.scroll_up_in_region(&vp, 0, 2, 1);
        // B is now row 0; its color should survive.
        assert_eq!(grid.rows[0].fg[0], red);
        // New blank row at bottom should have default colors.
        assert_eq!(grid.rows[2].fg[0], default_fg());
    }

    #[test]
    fn scroll_down_preserves_colors() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        let blue = Srgb::new(0, 0, 255);
        grid.rows[1].fg[0] = blue; // row B
        grid.scroll_down_in_region(&vp, 0, 2, 1);
        // B moved from row 1 to row 2.
        assert_eq!(grid.rows[2].fg[0], blue);
        // New blank row at top should have default colors.
        assert_eq!(grid.rows[0].fg[0], default_fg());
    }

    #[test]
    fn scroll_up_single_row_region() {
        // A 1-row region: scrolling should just blank it.
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_up_in_region(&vp, 1, 1, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "CCC"]);
    }

    #[test]
    fn scroll_down_single_row_region() {
        let (mut grid, vp) = make_grid_with_scrollback(3, 3, &['A', 'B', 'C']);
        grid.scroll_down_in_region(&vp, 1, 1, 1);
        assert_eq!(all_chars(&grid), vec!["AAA", "   ", "CCC"]);
    }

    // ── 2. Reflow with scrollback ───────────────────────────────────

    #[test]
    fn reflow_grow_with_scrollback_unwrapped() {
        // Scrollback rows should be resized but not merged with visible rows.
        let mut grid = make_grid(
            5,
            &[
                ("SSSSS", false), // scrollback
                ("AAAAA", false), // visible
                ("BBBBB", false),
            ],
        );
        grid.reflow(8);
        assert_eq!(grid.rows.len(), 3);
        assert_eq!(row_chars(&grid.rows[0]), "SSSSS   ");
        assert_eq!(row_chars(&grid.rows[1]), "AAAAA   ");
    }

    #[test]
    fn reflow_grow_with_scrollback_wrapped() {
        // Wrapped rows in the scrollback should merge just like visible ones.
        let mut grid = make_grid(
            5,
            &[
                ("hello", true),  // scrollback, wraps into next
                ("world", false), // scrollback
                ("AAAAA", false), // visible
            ],
        );
        grid.reflow(10);
        assert_eq!(row_chars(&grid.rows[0]), "helloworld");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_with_scrollback() {
        let mut grid = make_grid(
            6,
            &[
                ("abcdef", false), // scrollback
                ("ghijkl", false), // visible
            ],
        );
        grid.reflow(3);
        // Both rows should split.
        assert_eq!(grid.rows.len(), 4);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
    }

    #[test]
    fn reflow_mixed_wrapping_shrink_then_grow() {
        // Three logical lines at width 8:
        //   "Hi"                 — short unwrapped
        //   "ABCDEFGHIJKLMNOP"   — 16-char wrapped across two rows
        //   "Bye"                — short unwrapped
        let mut grid = make_grid(
            8,
            &[
                ("Hi      ", false),
                ("ABCDEFGH", true),
                ("IJKLMNOP", false),
                ("Bye     ", false),
            ],
        );

        // Shrink to width 4: "Hi" fits, "ABCD"/"EFGH"/"IJKL"/"MNOP", "Bye" fits.
        grid.reflow(4);
        assert_eq!(row_chars(&grid.rows[0]), "Hi  ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ABCD");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "EFGH");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "IJKL");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "MNOP");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(row_chars(&grid.rows[5]), "Bye ");
        assert!(!grid.rows[5].wrapped);
        assert_eq!(grid.rows.len(), 6);

        // Grow 4 → 6: wrapped chains partially re-merge.
        // 16 chars at width 6 = three rows: 6 + 6 + 4.
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "Hi    ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ABCDEF");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "GHIJKL");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "MNOP  ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "Bye   ");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);
    }

    #[test]
    fn reflow_multiple_wrapped_shrink_then_grow() {
        // Two logical lines, each wrapped across two rows at width 6.
        let mut grid = make_grid(
            6,
            &[
                ("abcdef", true),
                ("ghijkl", true),
                ("mnopqr", false),
                ("stuvwx", true),
                ("yz0123", false),
                ("      ", false),
            ],
        );

        // Shrink to width 3: each wrapped line splits into two.
        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "jkl");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "mno");
        assert!(grid.rows[4].wrapped);
        assert_eq!(row_chars(&grid.rows[5]), "pqr");
        assert!(!grid.rows[5].wrapped);
        assert_eq!(row_chars(&grid.rows[6]), "stu");
        assert!(grid.rows[6].wrapped);
        assert_eq!(row_chars(&grid.rows[7]), "vwx");
        assert!(grid.rows[7].wrapped);
        assert_eq!(row_chars(&grid.rows[8]), "yz0");
        assert!(grid.rows[8].wrapped);
        assert_eq!(row_chars(&grid.rows[9]), "123");
        assert!(!grid.rows[9].wrapped);
        assert_eq!(row_chars(&grid.rows[10]), "   ");
        assert!(!grid.rows[10].wrapped);
        assert_eq!(grid.rows.len(), 11);

        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ghijkl");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "mnopqr");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "stuvwx");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "yz0123");
        assert!(!grid.rows[4].wrapped);
    }

    #[test]
    fn reflow_mixed_wrapping_roundtrip() {
        // Shrink then grow back to original width with mixed lines.
        //   "Hi"             — short unwrapped
        //   "abcdefghijkl"   — 12-char wrapped across two rows
        //   "Lo"             — short unwrapped

        let mut grid = make_grid(
            6,
            &[
                ("Hi    ", false),
                ("abcdef", true),
                ("ghijkl", false),
                ("Lo    ", false),
            ],
        );

        grid.reflow(3);
        assert_eq!(row_chars(&grid.rows[0]), "Hi ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "abc");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "def");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "ghi");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "jkl");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(row_chars(&grid.rows[5]), "Lo ");
        assert!(!grid.rows[5].wrapped);
        assert_eq!(grid.rows.len(), 6);

        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "Hi    ");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "abcdef");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghijkl");
        assert!(!grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "Lo    ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);
    }

    // ── 3. Reflow edge cases ────────────────────────────────────────

    #[test]
    fn reflow_empty_grid() {
        let mut grid = Grid {
            rows: VecDeque::new(),
            scrollback_limit: 1000,
            total_popped: 0,
        };
        grid.reflow(10); // should not panic
        assert_eq!(grid.rows.len(), 0);
    }

    #[test]
    fn reflow_single_row_shrink() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        grid.reflow(3);
        assert_eq!(grid.rows.len(), 2);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(!grid.rows[1].wrapped);
    }

    #[test]
    fn reflow_shrink_exact_fit_no_overflow() {
        // Content exactly fills the new width — no split needed.
        let mut grid = make_grid(6, &[("abc   ", false)]);
        grid.reflow(3);
        // "abc" fits in 3 cols, trailing spaces are not content.
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(row_chars(&grid.rows[0]), "abc");
    }

    #[test]
    fn reflow_shrink_preserves_colors() {
        let mut grid = make_grid(6, &[("abcdef", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[0].fg[3] = red; // 'd' is red
        grid.reflow(3);
        // 'd' is now at row 1, col 0.
        assert_eq!(grid.rows[1].fg[0], red);
    }

    #[test]
    fn reflow_grow_preserves_colors() {
        let mut grid = make_grid(3, &[("abc", true), ("def", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[1].fg[0] = red; // 'd' is red
        grid.reflow(6);
        // Merged into one row: "abcdef". 'd' is at col 3.
        assert_eq!(grid.rows[0].fg[3], red);
    }

    // ── 4. push_visible_row ─────────────────────────────────────────

    #[test]
    fn push_visible_row_adds_blank() {
        let vp = make_viewport(3, 4);
        let (mut grid, _) = make_grid_with_scrollback(4, 3, &['A', 'B', 'C']);
        grid.push_visible_row(&vp);
        assert_eq!(grid.rows.len(), 4);
        assert_eq!(row_chars(grid.rows.back().unwrap()), "    ");
    }

    #[test]
    fn push_visible_row_trims_scrollback() {
        let vp = make_viewport(3, 4);
        let mut grid = Grid {
            rows: VecDeque::new(),
            scrollback_limit: 2,
            total_popped: 0,
        };
        // Fill 3 visible + 2 scrollback = 5 rows (at the limit).
        for ch in ['S', 'T', 'A', 'B', 'C'] {
            let mut row = Row::new(4);
            row.chars.fill(ch);
            grid.rows.push_back(row);
        }
        assert_eq!(grid.rows.len(), 5); // at limit
        grid.push_visible_row(&vp);
        // Should have trimmed the oldest scrollback row.
        assert_eq!(grid.rows.len(), 5);
        assert_eq!(grid.total_popped, 1);
        assert_eq!(row_chars(&grid.rows[0]), "TTTT"); // 'S' row was removed
    }

    #[test]
    fn push_visible_row_total_popped_accumulates() {
        let vp = make_viewport(2, 3);
        let mut grid = Grid {
            rows: VecDeque::new(),
            scrollback_limit: 0,
            total_popped: 0,
        };
        // Start with 2 visible rows.
        for ch in ['A', 'B'] {
            let mut row = Row::new(3);
            row.chars.fill(ch);
            grid.rows.push_back(row);
        }
        // Push 3 more rows — each should pop one.
        grid.push_visible_row(&vp);
        grid.push_visible_row(&vp);
        grid.push_visible_row(&vp);
        assert_eq!(grid.total_popped, 3);
        assert_eq!(grid.rows.len(), 2);
    }

    // ── 5. reflow_soft_grow across VecDeque split ───────────────────

    #[test]
    fn reflow_grow_across_deque_boundary() {
        // Force wrapped rows to straddle the VecDeque's internal ring buffer
        // boundary. Rotating by exactly `len` preserves logical order while
        // advancing the internal head pointer. With 3 rows and typical
        // capacity 4, head lands at position 3 and elements wrap around.
        let mut grid = make_grid(3, &[("abc", true), ("def", true), ("ghi", false)]);
        let n = grid.rows.len();
        let cap = grid.rows.capacity();
        if cap > n {
            // Rotate by len to preserve order but shift the head pointer.
            for _ in 0..n {
                let row = grid.rows.pop_front().unwrap();
                grid.rows.push_back(row);
            }
        }
        grid.reflow(9);
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghi");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_grow_across_deque_boundary_partial_merge() {
        // 4 rows where only the first 2 are wrapped — merge should stop at
        // the unwrapped boundary. Rotation forces ring buffer wrap-around.
        let mut grid = make_grid(
            3,
            &[("abc", true), ("def", false), ("ghi", true), ("jkl", false)],
        );
        let n = grid.rows.len();
        let cap = grid.rows.capacity();
        if cap > n {
            for _ in 0..n {
                let row = grid.rows.pop_front().unwrap();
                grid.rows.push_back(row);
            }
        }
        grid.reflow(6);
        assert_eq!(row_chars(&grid.rows[0]), "abcdef");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "ghijkl");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    // ── Reflow: shrink-then-grow with long lines ───────────────────
    //
    // These tests exercise the merge path where the grow width is more
    // than double the shrunk width, requiring multiple source rows to
    // be pulled into a single destination row. This is the common case
    // for long log lines: a wide terminal shrinks narrow, creating many
    // wrapped rows, then grows back.

    #[test]
    fn reflow_shrink_grow_roundtrip_long_line() {
        // "abcdefghij" at width 10, shrink to 3 then grow back to 10.
        // Ratio 10:3 means each destination row consumes ~3 source rows.
        let mut grid = make_grid(10, &[("abcdefghij", false)]);

        grid.reflow(3);
        // "abc"W "def"W "ghi"W "j  "U
        assert_eq!(row_chars(&grid.rows[0]), "abc");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "def");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ghi");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "j  ");
        assert!(!grid.rows[3].wrapped);
        assert_eq!(grid.rows.len(), 4);

        grid.reflow(10);
        // Should recover the original single row.
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghij");
        assert!(!grid.rows[0].wrapped);
        assert_eq!(grid.rows.len(), 1);
    }

    #[test]
    fn reflow_shrink_grow_long_line_partial_grow() {
        // 20-char line shrunk to 4, then grown to 10 (not back to original).
        // Content should reflow into two correctly packed rows.
        let mut grid = make_grid(20, &[("abcdefghijklmnopqrst", false)]);

        grid.reflow(4);
        // "abcd"W "efgh"W "ijkl"W "mnop"W "qrst"U
        assert_eq!(grid.rows.len(), 5);
        assert_eq!(row_chars(&grid.rows[0]), "abcd");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "efgh");
        assert!(grid.rows[1].wrapped);
        assert_eq!(row_chars(&grid.rows[2]), "ijkl");
        assert!(grid.rows[2].wrapped);
        assert_eq!(row_chars(&grid.rows[3]), "mnop");
        assert!(grid.rows[3].wrapped);
        assert_eq!(row_chars(&grid.rows[4]), "qrst");
        assert!(!grid.rows[4].wrapped);
        assert_eq!(grid.rows.len(), 5);

        grid.reflow(10);
        // 20 chars at width 10 = two rows.
        assert_eq!(row_chars(&grid.rows[0]), "abcdefghij");
        assert!(grid.rows[0].wrapped);
        assert_eq!(row_chars(&grid.rows[1]), "klmnopqrst");
        assert!(!grid.rows[1].wrapped);
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn reflow_shrink_grow_long_line_colors_roundtrip() {
        // Per-cell colors must survive a shrink-then-grow roundtrip even
        // when the grow width is more than double the shrunk width.
        let mut grid = make_grid(10, &[("abcdefghij", false)]);
        let red = Srgb::new(255, 0, 0);
        grid.rows[0].fg[6] = red; // 'g' is red

        grid.reflow(3);
        // After shrink: "abc"W "def"W "ghi"W "j  "U — 'g' at row 2 col 0.
        assert_eq!(grid.rows[2].chars[0], 'g');
        assert_eq!(grid.rows[2].fg[0], red);

        grid.reflow(10);
        // After roundtrip: 'g' should be back at col 6 with its red color.
        assert_eq!(grid.rows[0].chars[6], 'g');
        assert_eq!(grid.rows[0].fg[6], red);
    }

    // ── Alt-screen tests ────────────────────────────────────────────

    fn visible_text(term: &Terminal) -> String {
        let mut s = String::new();
        for r in 0..term.viewport.rows {
            let row = term.visible_row(r);
            s.extend(row.chars.iter());
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

    // ---- OSC 52 clipboard ----

    fn call_osc(payload: &[u8]) -> (Clipboard, Vec<u8>) {
        let mut clipboard = Clipboard::in_memory();
        let mut pending = Vec::new();
        handle_osc(payload, &mut clipboard, &mut pending);
        (clipboard, pending)
    }

    #[test]
    fn osc_52_writes_clipboard_with_c_selector() {
        let (mut cb, pending) = call_osc(b"52;c;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
        assert!(pending.is_empty());
    }

    #[test]
    fn osc_52_writes_primary_with_p_selector() {
        let (mut cb, _) = call_osc(b"52;p;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Primary).as_deref(), Some("hello"));
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
    }

    #[test]
    fn osc_52_empty_selector_defaults_to_clipboard() {
        let (mut cb, _) = call_osc(b"52;;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
    }

    #[test]
    fn osc_52_multi_selector_sets_both() {
        let (mut cb, _) = call_osc(b"52;cp;aGVsbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
        assert_eq!(cb.get(ClipboardKind::Primary).as_deref(), Some("hello"));
    }

    #[test]
    fn osc_52_tolerates_embedded_whitespace_in_base64() {
        let (mut cb, _) = call_osc(b"52;c;aGVs\nbG8=");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some("hello"));
    }

    #[test]
    fn osc_52_rejects_invalid_base64() {
        let (mut cb, _) = call_osc(b"52;c;!!not-base64!!");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
    }

    #[test]
    fn osc_52_query_emits_base64_response() {
        let mut clipboard = Clipboard::in_memory();
        clipboard.set(ClipboardKind::Clipboard, "hi");
        let mut pending = Vec::new();
        handle_osc(b"52;c;?", &mut clipboard, &mut pending);
        assert_eq!(pending, b"\x1b]52;c;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_query_echoes_original_selector() {
        let mut clipboard = Clipboard::in_memory();
        clipboard.set(ClipboardKind::Primary, "hi");
        let mut pending = Vec::new();
        handle_osc(b"52;p;?", &mut clipboard, &mut pending);
        assert_eq!(pending, b"\x1b]52;p;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_ignored_for_unknown_command() {
        let (mut cb, pending) = call_osc(b"0;some-title");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
        assert!(pending.is_empty());
    }

    #[test]
    fn osc_52_ignored_when_non_utf8() {
        // \xFF\xFE is valid base64 of 0xF5 0xFD 0xBF which is invalid UTF-8.
        let (mut cb, _) = call_osc(b"52;c;//2/");
        assert_eq!(cb.get(ClipboardKind::Clipboard).as_deref(), Some(""));
    }

    // ---- Mouse encoding + mode handling ----

    fn encode(
        encoding: MouseEncoding,
        kind: MouseEventKind,
        button: MouseButton,
        col: u32,
        row: u32,
        mods: MouseModifiers,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        encode_mouse_event(encoding, kind, button, col, row, mods, &mut out);
        out
    }

    #[test]
    fn sgr_encodes_press_and_release_with_mcase() {
        let mods = MouseModifiers::default();
        let press = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Press,
            MouseButton::Left,
            3,
            5,
            mods,
        );
        let release = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Release,
            MouseButton::Left,
            3,
            5,
            mods,
        );
        assert_eq!(press, b"\x1b[<0;3;5M");
        assert_eq!(release, b"\x1b[<0;3;5m");
    }

    #[test]
    fn sgr_motion_adds_bit_32() {
        let out = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Motion,
            MouseButton::Left,
            10,
            12,
            MouseModifiers::default(),
        );
        assert_eq!(out, b"\x1b[<32;10;12M");
    }

    #[test]
    fn sgr_modifiers_combine() {
        let mods = MouseModifiers {
            shift: true,
            alt: true,
            ctrl: true,
        };
        let out = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Press,
            MouseButton::Right,
            1,
            1,
            mods,
        );
        // button 2 + shift 4 + alt 8 + ctrl 16 = 30
        assert_eq!(out, b"\x1b[<30;1;1M");
    }

    #[test]
    fn sgr_wheel_encodes_button_64() {
        let out = encode(
            MouseEncoding::Sgr,
            MouseEventKind::Press,
            MouseButton::WheelUp,
            4,
            2,
            MouseModifiers::default(),
        );
        assert_eq!(out, b"\x1b[<64;4;2M");
    }

    #[test]
    fn legacy_encoding_offsets_bytes_by_32() {
        let out = encode(
            MouseEncoding::Default,
            MouseEventKind::Press,
            MouseButton::Left,
            3,
            5,
            MouseModifiers::default(),
        );
        assert_eq!(out, &[0x1B, b'[', b'M', 32, 35, 37]);
    }

    #[test]
    fn legacy_release_collapses_button_to_three() {
        let out = encode(
            MouseEncoding::Default,
            MouseEventKind::Release,
            MouseButton::Right,
            3,
            5,
            MouseModifiers::default(),
        );
        // 3 (release) + 32 = 35, coords +32
        assert_eq!(out, &[0x1B, b'[', b'M', 35, 35, 37]);
    }

    #[test]
    fn utf8_encoding_handles_large_coords() {
        let out = encode(
            MouseEncoding::Utf8,
            MouseEventKind::Press,
            MouseButton::Left,
            300,
            1,
            MouseModifiers::default(),
        );
        // Button byte: 0 + 32 = 32 (single byte ' ')
        assert_eq!(&out[..4], b"\x1b[M ");
        // Col 300 + 32 = 332, which is 0xC5 0x8C in UTF-8
        assert_eq!(&out[4..6], &[0xC5, 0x8C]);
        // Row 1 + 32 = 33 '!'
        assert_eq!(out[6], b'!');
    }

    #[test]
    fn urxvt_encoding_uses_decimal_with_32_bias() {
        let out = encode(
            MouseEncoding::Urxvt,
            MouseEventKind::Press,
            MouseButton::Left,
            3,
            5,
            MouseModifiers::default(),
        );
        // Cb 0 + 32 = 32
        assert_eq!(out, b"\x1b[32;3;5M");
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
    fn should_report_filters_by_tracking_mode() {
        assert!(!should_report(
            MouseTracking::Off,
            MouseEventKind::Press,
            MouseButton::Left
        ));
        assert!(should_report(
            MouseTracking::X10,
            MouseEventKind::Press,
            MouseButton::Left
        ));
        assert!(!should_report(
            MouseTracking::X10,
            MouseEventKind::Release,
            MouseButton::Left
        ));
        assert!(should_report(
            MouseTracking::Normal,
            MouseEventKind::Release,
            MouseButton::Left
        ));
        assert!(!should_report(
            MouseTracking::Normal,
            MouseEventKind::Motion,
            MouseButton::Left
        ));
        assert!(should_report(
            MouseTracking::ButtonEvent,
            MouseEventKind::Motion,
            MouseButton::Left
        ));
        assert!(!should_report(
            MouseTracking::ButtonEvent,
            MouseEventKind::Motion,
            MouseButton::None
        ));
        assert!(should_report(
            MouseTracking::AnyEvent,
            MouseEventKind::Motion,
            MouseButton::None
        ));
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
}

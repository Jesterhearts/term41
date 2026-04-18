use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clip41::Clipboard;
use clip41::ClipboardKind;
use percent_encoding::percent_decode;

use crate::C1Mode;
use crate::CommandMeta;
use crate::color;
use crate::conformance;
use crate::grid::Viewport;
use crate::hyperlink::HyperlinkId;
use crate::hyperlink::HyperlinkRegistry;
use crate::screen::Screen;

// -- OSC command numbers ------------------------------------------------------

const OSC_SET_ICON_AND_TITLE: u16 = 0;
const OSC_SET_TITLE: u16 = 2;
const OSC_PALETTE_COLOR: u16 = 4;
const OSC_SET_DIRECTORY: u16 = 7;
const OSC_HYPERLINK: u16 = 8;
const OSC_FG_COLOR: u16 = 10;
const OSC_BG_COLOR: u16 = 11;
const OSC_CURSOR_COLOR: u16 = 12;
const OSC_CLIPBOARD: u16 = 52;
const OSC_RESET_PALETTE: u16 = 104;
const OSC_RESET_FG: u16 = 110;
const OSC_RESET_BG: u16 = 111;
const OSC_RESET_CURSOR_COLOR: u16 = 112;
const OSC_SHELL_INTEGRATION: u16 = 133;
const OSC_ITERM2: u16 = 1337;

/// Bundles the bits of [`Terminal`](super::Terminal) state that OSC handlers
/// are allowed to read or mutate. Passing a single context keeps the call
/// signature stable as new OSC commands (8 hyperlinks, 7 cwd, 0/2 title, 4
/// palette, …) get wired in.
///
/// `active_screen` is handed in whole (rather than borrowing its individual
/// fields) so handlers that need both the grid and the cursor — like OSC 133
/// shell integration — don't have to juggle multiple simultaneous borrows.
pub(super) struct OscContext<'a> {
    pub clipboard: &'a mut Clipboard,
    pub pending_output: &'a mut Vec<u8>,
    pub c1_mode: C1Mode,
    pub current_directory: &'a mut Option<PathBuf>,
    pub hyperlinks: &'a mut HyperlinkRegistry,
    pub active_screen: &'a mut Screen,
    pub viewport: &'a Viewport,
    pub current_title: &'a mut Option<String>,
    /// Absolute row index of the most recent OSC 133 `A` (prompt start).
    /// An `OSC 133 D` stamps its exit code onto this row's exit_status so
    /// the mark sits next to the prompt, not the end-of-output. `None`
    /// before the first prompt and after the prompt row scrolls off the
    /// front of scrollback.
    pub current_prompt_row: &'a mut Option<u64>,
    /// Per-prompt metadata: command column (from B), output row (from C),
    /// and timestamps for duration calculation.
    pub command_metas: &'a mut HashMap<u64, CommandMeta>,
    pub palette: &'a color::ColorPalette,
    pub cell_width: u32,
    pub cell_height: u32,
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
pub(super) fn handle_osc(
    payload: &[u8],
    ctx: &mut OscContext<'_>,
) {
    let (cmd_bytes, rest) = split_osc(payload);

    // Parse the numeric command prefix. Non-numeric or empty prefixes
    // produce None and fall through to the default (silently dropped).
    let cmd: Option<u16> = std::str::from_utf8(cmd_bytes)
        .ok()
        .and_then(|s| s.parse().ok());

    match cmd {
        // OSC 0 sets icon name + window title; OSC 2 sets just the window
        // title. We don't have a separate icon-name surface, so both feed
        // the same field. OSC 1 (icon name only) is intentionally ignored.
        Some(OSC_SET_ICON_AND_TITLE) | Some(OSC_SET_TITLE) => {
            handle_osc_title(rest, ctx.current_title)
        }
        Some(OSC_SET_DIRECTORY) => handle_osc_7(rest, ctx.current_directory),
        Some(OSC_HYPERLINK) => handle_osc_8(
            rest,
            ctx.hyperlinks,
            &mut ctx.active_screen.current_hyperlink,
        ),
        Some(OSC_PALETTE_COLOR) => handle_osc_4(rest, ctx.palette, ctx.c1_mode, ctx.pending_output),
        Some(OSC_FG_COLOR) => handle_osc_color_query(
            rest,
            OSC_FG_COLOR as u8,
            ctx.palette.fg,
            ctx.c1_mode,
            ctx.pending_output,
        ),
        Some(OSC_BG_COLOR) => handle_osc_color_query(
            rest,
            OSC_BG_COLOR as u8,
            ctx.palette.bg,
            ctx.c1_mode,
            ctx.pending_output,
        ),
        Some(OSC_CURSOR_COLOR) => handle_osc_cursor_color_query(rest, ctx),
        Some(OSC_CLIPBOARD) => handle_osc_52(rest, ctx.clipboard, ctx.c1_mode, ctx.pending_output),
        // Reset palette/fg/bg/cursor color. Accepted but currently no-op —
        // the palette is immutable at this level.
        Some(OSC_RESET_PALETTE | OSC_RESET_FG | OSC_RESET_BG | OSC_RESET_CURSOR_COLOR) => {}
        Some(OSC_SHELL_INTEGRATION) => handle_osc_133(
            rest,
            ctx.active_screen,
            ctx.viewport,
            ctx.current_prompt_row,
            ctx.command_metas,
        ),
        // iTerm2 proprietary commands. Image commands (File=, MultipartFile=,
        // etc.) are routed separately in terminal.rs. ReportCellSize gets a
        // reply; other non-image commands are silently accepted as no-ops.
        Some(OSC_ITERM2) if rest.starts_with(b"ReportCellSize") => {
            conformance::write_osc(
                ctx.pending_output,
                ctx.c1_mode,
                format_args!("1337;ReportCellSize={};{}", ctx.cell_height, ctx.cell_width),
            );
        }
        Some(OSC_ITERM2) => {}
        _ => {}
    }
}

/// OSC 133 — semantic prompt marks (a.k.a. "shell integration"). A
/// cooperating shell (bash, zsh, fish, …) brackets its prompt and command
/// output with:
///
/// ```text
/// OSC 133 ; A ST   — prompt start
/// OSC 133 ; B ST   — command start (user's typing begins)
/// OSC 133 ; C ST   — command output start
/// OSC 133 ; D [; exit_code] ST   — command finished
/// ```
///
/// Terminals use these to offer prompt-to-prompt navigation, last-command
/// selection, success/failure gutter markers, and similar. `A`, `B`, `C`,
/// and `D` all produce observable state: `A` marks the prompt row, `B`
/// records the column where the typed command begins, `C` marks the output
/// start and timestamps execution, and `D` stamps exit status and finish
/// time.
///
/// Payloads with extra `;key=value` args (iTerm2-style `aid=…`, `cl=…`,
/// etc.) are ignored — we only honour the single-letter kind.
fn handle_osc_133(
    rest: &[u8],
    screen: &mut Screen,
    viewport: &Viewport,
    current_prompt_row: &mut Option<u64>,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    let (kind, args) = split_osc(rest);
    match kind {
        b"A" => {
            let abs = mark_current_row(screen, viewport, |row| {
                row.prompt_start = true;
                // A fresh prompt invalidates any lingering exit_status from
                // a prior occupant of this row (e.g. a recycled scrollback
                // slot). The shell hasn't even shown the prompt yet.
                row.exit_status = None;
            });
            *current_prompt_row = Some(abs);
            // Seed the metadata entry so B/C/D can fill it in.
            command_metas.insert(abs, CommandMeta::new());
        }
        b"B" => {
            // Prompt end / command start. Record the cursor column so
            // "select command" can skip the prompt decoration.
            if let Some(prompt_abs) = *current_prompt_row {
                let abs = current_absolute_row(screen, viewport);
                if let Some(meta) = command_metas.get_mut(&prompt_abs) {
                    meta.command_col = Some(screen.cursor.col);
                    meta.command_row = Some(abs);
                }
            }
        }
        b"C" => {
            let abs = mark_current_row(screen, viewport, |row| {
                row.output_start = true;
            });
            if let Some(prompt_abs) = *current_prompt_row
                && let Some(meta) = command_metas.get_mut(&prompt_abs)
            {
                meta.output_row = Some(abs);
                meta.started_at = Some(Instant::now());
            }
        }
        b"D" => {
            let exit = parse_osc_133_d_exit(args);
            if let Some(abs) = *current_prompt_row
                && let Some(local) = absolute_to_local(screen, abs)
            {
                screen.grid.rows[local].exit_status = Some(exit);
            }
            if let Some(prompt_abs) = *current_prompt_row
                && let Some(meta) = command_metas.get_mut(&prompt_abs)
            {
                meta.finished_at = Some(Instant::now());
            }
        }
        _ => {}
    }
}

/// Run `apply` on the row the cursor currently occupies and return that
/// row's absolute index (stable under scrollback trimming). Factored out
/// because every OSC 133 kind that stores a mark does the same lookup.
fn mark_current_row(
    screen: &mut Screen,
    viewport: &Viewport,
    apply: impl FnOnce(&mut crate::row::Row),
) -> u64 {
    let local = screen.grid.active_row_index(&screen.cursor, viewport);
    apply(&mut screen.grid.rows[local]);
    (screen.grid.total_popped + local) as u64
}

/// Return the absolute row index the cursor currently sits on, without
/// mutating the row. Used by OSC 133 B to record the command start row.
fn current_absolute_row(
    screen: &Screen,
    viewport: &Viewport,
) -> u64 {
    let local = screen.grid.active_row_index(&screen.cursor, viewport);
    (screen.grid.total_popped + local) as u64
}

/// Translate an absolute row index into a live grid offset, or `None` if
/// the row has already fallen off the front of scrollback.
fn absolute_to_local(
    screen: &Screen,
    abs: u64,
) -> Option<usize> {
    let popped = screen.grid.total_popped as u64;
    let local = abs.checked_sub(popped)? as usize;
    (local < screen.grid.rows.len()).then_some(local)
}

/// Parse the exit code from an OSC 133 `D` payload. Per the spec the first
/// argument is the exit status; non-numeric or missing values are treated
/// as success (`0`) so a shell that merely reports "command finished"
/// without the numeric status doesn't accidentally paint every prompt red.
fn parse_osc_133_d_exit(args: &[u8]) -> i32 {
    let (first, _) = split_osc(args);
    std::str::from_utf8(first)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0)
}

/// OSC 0 / OSC 2 — set the window title. Empty payloads clear the title
/// (matches xterm) so apps can hand back default behaviour. Non-UTF-8
/// payloads are dropped rather than mojibaked into the title bar.
fn handle_osc_title(
    rest: &[u8],
    current_title: &mut Option<String>,
) {
    if rest.is_empty() {
        *current_title = None;
        return;
    }
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    *current_title = Some(text.to_owned());
}

/// Format an 8-bit color channel as the 16-bit hex representation used in
/// X11 color replies. Each 8-bit value is scaled to 16 bits by repeating the
/// byte (e.g. 0xCC → 0xCCCC).
fn rgb_reply(
    r: u8,
    g: u8,
    b: u8,
) -> String {
    let r16 = (r as u16) << 8 | r as u16;
    let g16 = (g as u16) << 8 | g as u16;
    let b16 = (b as u16) << 8 | b as u16;
    format!("rgb:{r16:04x}/{g16:04x}/{b16:04x}")
}

/// OSC 10 / OSC 11 — foreground / background color query. If the payload is
/// `?` the terminal replies with the current default color in X11
/// `rgb:RR/GG/BB` format. Setting colors is not supported (silently ignored).
fn handle_osc_color_query(
    rest: &[u8],
    cmd: u8,
    current: palette::Srgb<u8>,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if rest != b"?" {
        return;
    }
    let reply = rgb_reply(current.red, current.green, current.blue);
    conformance::write_osc(pending_output, c1_mode, format_args!("{cmd};{reply}"));
}

/// OSC 12 — cursor color query. If the payload is `?` the terminal replies
/// with the cursor color in X11 `rgb:RRRR/GGGG/BBBB` format. When no explicit
/// cursor color is set, the default foreground is reported (matching xterm).
fn handle_osc_cursor_color_query(
    rest: &[u8],
    ctx: &mut OscContext<'_>,
) {
    if rest != b"?" {
        return;
    }
    let c = ctx.palette.cursor.unwrap_or(ctx.palette.fg);
    let reply = rgb_reply(c.red, c.green, c.blue);
    conformance::write_osc(ctx.pending_output, ctx.c1_mode, format_args!("12;{reply}"));
}

/// OSC 4;N;? — query the Nth entry of the 256-color palette. The response
/// mirrors the query format: `OSC 4;N;rgb:RR/GG/BB ST`. Only query (`?`) is
/// handled; set-palette payloads are silently ignored.
fn handle_osc_4(
    rest: &[u8],
    palette: &color::ColorPalette,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    // Payload format: N;? (index, semicolon, question mark).
    let (idx_bytes, query) = split_osc(rest);
    if query != b"?" {
        return;
    }
    let Ok(idx_str) = std::str::from_utf8(idx_bytes) else {
        return;
    };
    let Ok(idx) = idx_str.parse::<u8>() else {
        return;
    };
    let c = color::palette_color(palette, idx);
    let reply = rgb_reply(c.red, c.green, c.blue);
    conformance::write_osc(pending_output, c1_mode, format_args!("4;{idx_str};{reply}"));
}

/// Implements OSC 52 clipboard read/write as used by vim, tmux, etc.
///
/// Format: `OSC 52 ; Pc ; Pd ST` — Pc is one or more selector characters and
/// Pd is either base64-encoded text to copy, or `?` to query the clipboard
/// and have the terminal echo the result back over the PTY.
fn handle_osc_52(
    rest: &[u8],
    clipboard: &mut Clipboard,
    c1_mode: C1Mode,
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
        conformance::push_osc_prefix(pending_output, c1_mode);
        pending_output.extend_from_slice(b"52;");
        pending_output.extend_from_slice(pc_resp);
        pending_output.push(b';');
        pending_output.extend_from_slice(encoded.as_bytes());
        conformance::push_st(pending_output, c1_mode);
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

/// OSC 7 — current working directory reporting. Shells emit
/// `OSC 7 ; file://hostname/percent-encoded/path ST` after each `cd` so the
/// terminal can offer "open new window in this directory" or surface the
/// path in the title bar without parsing the prompt.
///
/// The hostname segment is informational (most terminals honour the path
/// regardless of host); we accept and ignore it. Empty payloads clear the
/// stored cwd, matching the behaviour shells use to indicate "I no longer
/// know where I am" (e.g. after a remote SSH session ends).
fn handle_osc_7(
    rest: &[u8],
    current_directory: &mut Option<PathBuf>,
) {
    if rest.is_empty() {
        *current_directory = None;
        return;
    }

    let Ok(uri) = std::str::from_utf8(rest) else {
        return;
    };

    // Strip the scheme. We only honour file://; ignoring other schemes keeps
    // remote shells (where the path is not meaningful locally) from poisoning
    // local features like "open new window here".
    let Some(rest) = uri.strip_prefix("file://") else {
        return;
    };

    // Drop the hostname between `file://` and the first `/`.
    let path_start = rest.find('/').unwrap_or(rest.len());
    let encoded_path = &rest[path_start..];
    if encoded_path.is_empty() {
        return;
    }

    let decoded = percent_decode(encoded_path.as_bytes()).collect::<Vec<u8>>();
    let Ok(path) = std::str::from_utf8(&decoded) else {
        return;
    };

    *current_directory = Some(PathBuf::from(path));
}

/// OSC 8 — hyperlinks. `OSC 8 ; params ; URI ST` opens a hyperlink span;
/// subsequent printed cells carry the link until a closing
/// `OSC 8 ; ; ST` (empty params + empty URI) ends it.
///
/// Params is a colon-separated `key=value` list — `id=…` is the only widely
/// used one, distinguishing adjacent links to the same URI. We honour `id`
/// when present so two distinct buttons pointing at the same URL still
/// register as two links.
fn handle_osc_8(
    rest: &[u8],
    registry: &mut HyperlinkRegistry,
    current: &mut Option<HyperlinkId>,
) {
    let (params, uri) = split_osc(rest);

    if uri.is_empty() {
        *current = None;
        return;
    }

    let Ok(uri_str) = std::str::from_utf8(uri) else {
        *current = None;
        return;
    };

    let id_param = params.split(|&b| b == b':').find_map(|kv| {
        let mut it = kv.splitn(2, |&b| b == b'=');
        let key = it.next()?;
        let value = it.next()?;
        if key == b"id" {
            std::str::from_utf8(value).ok()
        } else {
            None
        }
    });

    *current = Some(registry.intern(id_param, uri_str));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyperlink::HyperlinkRegistry;

    struct Bag {
        clipboard: Clipboard,
        pending: Vec<u8>,
        cwd: Option<PathBuf>,
        registry: HyperlinkRegistry,
        screen: Screen,
        viewport: Viewport,
        title: Option<String>,
        prompt_row: Option<u64>,
        command_metas: HashMap<u64, CommandMeta>,
        palette: color::ColorPalette,
    }

    impl Bag {
        fn new() -> Self {
            Self::with_screen(4, 2)
        }

        fn with_screen(
            cols: u32,
            rows: u32,
        ) -> Self {
            Self {
                clipboard: Clipboard::in_memory(),
                pending: Vec::new(),
                cwd: None,
                registry: HyperlinkRegistry::new(),
                screen: Screen::new(cols, rows, 100, color::default_fg(), color::default_bg()),
                viewport: Viewport { rows, cols, top: 0 },
                title: None,
                prompt_row: None,
                command_metas: HashMap::new(),
                palette: color::ColorPalette::default(),
            }
        }

        fn current_link(&self) -> Option<HyperlinkId> {
            self.screen.current_hyperlink
        }

        fn dispatch(
            &mut self,
            payload: &[u8],
        ) {
            let mut ctx = OscContext {
                clipboard: &mut self.clipboard,
                pending_output: &mut self.pending,
                c1_mode: C1Mode::SevenBit,
                current_directory: &mut self.cwd,
                hyperlinks: &mut self.registry,
                active_screen: &mut self.screen,
                viewport: &self.viewport,
                current_title: &mut self.title,
                current_prompt_row: &mut self.prompt_row,
                command_metas: &mut self.command_metas,
                palette: &self.palette,
                cell_width: 8,
                cell_height: 16,
            };
            handle_osc(payload, &mut ctx);
        }
    }

    #[test]
    fn osc_52_writes_clipboard_with_c_selector() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_52_writes_primary_with_p_selector() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;p;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Primary).as_deref(),
            Some("hello")
        );
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
    }

    #[test]
    fn osc_52_empty_selector_defaults_to_clipboard() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn osc_52_multi_selector_sets_both() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;cp;aGVsbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Primary).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn osc_52_tolerates_embedded_whitespace_in_base64() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;aGVs\nbG8=");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn osc_52_rejects_invalid_base64() {
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;!!not-base64!!");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
    }

    #[test]
    fn osc_52_query_emits_base64_response() {
        let mut bag = Bag::new();
        bag.clipboard.set(ClipboardKind::Clipboard, "hi");
        bag.dispatch(b"52;c;?");
        assert_eq!(bag.pending, b"\x1b]52;c;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_query_echoes_original_selector() {
        let mut bag = Bag::new();
        bag.clipboard.set(ClipboardKind::Primary, "hi");
        bag.dispatch(b"52;p;?");
        assert_eq!(bag.pending, b"\x1b]52;p;aGk=\x1b\\");
    }

    #[test]
    fn osc_52_ignored_for_unknown_command() {
        let mut bag = Bag::new();
        bag.dispatch(b"99;nothing");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_52_ignored_when_non_utf8() {
        // \xFF\xFE is valid base64 of 0xF5 0xFD 0xBF which is invalid UTF-8.
        let mut bag = Bag::new();
        bag.dispatch(b"52;c;//2/");
        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
    }

    // ---- OSC 7 ----

    #[test]
    fn osc_7_decodes_simple_path() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;file://localhost/home/jessica");
        assert_eq!(bag.cwd, Some(PathBuf::from("/home/jessica")));
    }

    #[test]
    fn osc_7_percent_decodes_path() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;file:///home/has%20space/proj");
        assert_eq!(bag.cwd, Some(PathBuf::from("/home/has space/proj")));
    }

    #[test]
    fn osc_7_empty_clears() {
        let mut bag = Bag::new();
        bag.cwd = Some(PathBuf::from("/old"));
        bag.dispatch(b"7;");
        assert_eq!(bag.cwd, None);
    }

    #[test]
    fn osc_7_ignores_non_file_scheme() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;ftp://server/some/path");
        assert_eq!(bag.cwd, None);
    }

    #[test]
    fn osc_7_ignores_invalid_utf8() {
        let mut bag = Bag::new();
        bag.dispatch(b"7;file:///\xFF\xFE");
        assert_eq!(bag.cwd, None);
    }

    // ---- OSC 8 ----

    #[test]
    fn osc_8_sets_current_link_with_uri() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;;https://example.com");
        let id = bag.current_link().expect("link set");
        assert_eq!(bag.registry.get(id), Some("https://example.com"));
    }

    #[test]
    fn osc_8_empty_uri_clears_current_link() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;;https://example.com");
        bag.dispatch(b"8;;");
        assert!(bag.current_link().is_none());
    }

    #[test]
    fn osc_8_distinct_id_keys_separate_link_ids() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;id=a;https://example.com");
        let id_a = bag.current_link().unwrap();
        bag.dispatch(b"8;id=b;https://example.com");
        let id_b = bag.current_link().unwrap();
        assert_ne!(id_a, id_b);
    }

    // ---- OSC 0 / OSC 2 ----

    #[test]
    fn osc_0_sets_window_title() {
        let mut bag = Bag::new();
        bag.dispatch(b"0;hello");
        assert_eq!(bag.title.as_deref(), Some("hello"));
    }

    #[test]
    fn osc_2_sets_window_title() {
        let mut bag = Bag::new();
        bag.dispatch(b"2;build done");
        assert_eq!(bag.title.as_deref(), Some("build done"));
    }

    #[test]
    fn osc_2_empty_clears_title() {
        let mut bag = Bag::new();
        bag.title = Some("stale".into());
        bag.dispatch(b"2;");
        assert!(bag.title.is_none());
    }

    #[test]
    fn osc_2_drops_invalid_utf8() {
        let mut bag = Bag::new();
        bag.title = Some("kept".into());
        bag.dispatch(b"2;\xff\xfe");
        // Invalid UTF-8 leaves the previous title untouched rather than
        // wiping it; that's safer than displaying garbage.
        assert_eq!(bag.title.as_deref(), Some("kept"));
    }

    #[test]
    fn osc_1_is_ignored() {
        let mut bag = Bag::new();
        bag.dispatch(b"1;icon-name-only");
        assert!(bag.title.is_none());
    }

    // ---- OSC 10 / OSC 11 / OSC 4 — color queries ----

    #[test]
    fn osc_10_query_returns_default_fg() {
        let mut bag = Bag::new();
        bag.dispatch(b"10;?");
        // default_fg() = (204,204,204) → 0xCCCC/0xCCCC/0xCCCC
        assert_eq!(bag.pending, b"\x1b]10;rgb:cccc/cccc/cccc\x1b\\");
    }

    #[test]
    fn osc_11_query_returns_default_bg() {
        let mut bag = Bag::new();
        bag.dispatch(b"11;?");
        // default_bg() = (0,0,0) → 0x0000/0x0000/0x0000
        assert_eq!(bag.pending, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
    }

    #[test]
    fn osc_10_non_query_is_ignored() {
        let mut bag = Bag::new();
        bag.dispatch(b"10;rgb:ffff/ffff/ffff");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_4_query_returns_palette_color() {
        let mut bag = Bag::new();
        // Palette color 1 = (205, 0, 0) → cd00/0000/0000
        bag.dispatch(b"4;1;?");
        assert_eq!(bag.pending, b"\x1b]4;1;rgb:cdcd/0000/0000\x1b\\");
    }

    #[test]
    fn osc_4_query_high_index() {
        let mut bag = Bag::new();
        // Palette color 15 = (255,255,255) → ffff/ffff/ffff
        bag.dispatch(b"4;15;?");
        assert_eq!(bag.pending, b"\x1b]4;15;rgb:ffff/ffff/ffff\x1b\\");
    }

    #[test]
    fn osc_4_non_query_is_ignored() {
        let mut bag = Bag::new();
        bag.dispatch(b"4;1;rgb:ffff/0000/0000");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_4_invalid_index_is_ignored() {
        let mut bag = Bag::new();
        bag.dispatch(b"4;999;?");
        assert!(bag.pending.is_empty());
    }

    // ---- OSC 12 — cursor color query ----

    #[test]
    fn osc_12_query_returns_fg_when_no_cursor_color() {
        let mut bag = Bag::new();
        bag.dispatch(b"12;?");
        // No cursor color set → falls back to fg (204,204,204).
        assert_eq!(bag.pending, b"\x1b]12;rgb:cccc/cccc/cccc\x1b\\");
    }

    #[test]
    fn osc_12_query_returns_explicit_cursor_color() {
        let mut bag = Bag::new();
        bag.palette.cursor = Some(palette::Srgb::new(255, 128, 0));
        bag.dispatch(b"12;?");
        assert_eq!(bag.pending, b"\x1b]12;rgb:ffff/8080/0000\x1b\\");
    }

    #[test]
    fn osc_12_non_query_is_ignored() {
        let mut bag = Bag::new();
        bag.dispatch(b"12;rgb:ffff/0000/0000");
        assert!(bag.pending.is_empty());
    }

    // ---- OSC 104/110/111/112 — color reset no-ops ----

    #[test]
    fn osc_104_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"104");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_104_with_index_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"104;1");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_110_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"110");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_111_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"111");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_112_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"112");
        assert!(bag.pending.is_empty());
    }

    // ---- OSC 1337 — iTerm2 non-image commands ----

    #[test]
    fn osc_1337_non_image_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"1337;SetMark");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_1337_set_user_var_accepted_silently() {
        let mut bag = Bag::new();
        bag.dispatch(b"1337;SetUserVar=foo=bar");
        assert!(bag.pending.is_empty());
    }

    #[test]
    fn osc_8_same_id_reuses_link_id() {
        let mut bag = Bag::new();
        bag.dispatch(b"8;id=foo;https://example.com");
        let id_first = bag.current_link().unwrap();
        bag.dispatch(b"8;;"); // close
        bag.dispatch(b"8;id=foo;https://example.com");
        let id_again = bag.current_link().unwrap();
        assert_eq!(id_first, id_again);
    }

    // ---- OSC 133 — shell integration ----

    impl Bag {
        /// Move the test screen's cursor. The active row index is derived
        /// from `cursor.row` + viewport, so OSC 133 landing points are
        /// selected by moving the cursor before dispatching.
        fn move_cursor(
            &mut self,
            col: u32,
            row: u32,
        ) {
            self.screen.cursor.col = col;
            self.screen.cursor.row = row;
        }

        fn row_at(
            &self,
            screen_row: u32,
        ) -> &crate::row::Row {
            let first_visible = self.screen.grid.rows.len() - self.viewport.rows as usize;
            &self.screen.grid.rows[first_visible + screen_row as usize]
        }
    }

    #[test]
    fn osc_133_a_marks_prompt_row_and_records_prompt_pointer() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 2);
        bag.dispatch(b"133;A");
        assert!(bag.row_at(2).prompt_start);
        assert_eq!(bag.prompt_row, Some(2));
    }

    #[test]
    fn osc_133_b_is_parsed_without_storing() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 1);
        bag.dispatch(b"133;B");
        // B is deliberately a no-op at the storage layer — it shouldn't
        // mark prompt/output rows or record a prompt pointer.
        assert!(!bag.row_at(1).prompt_start);
        assert!(!bag.row_at(1).output_start);
        assert_eq!(bag.prompt_row, None);
    }

    #[test]
    fn osc_133_c_marks_output_row() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 3);
        bag.dispatch(b"133;C");
        assert!(bag.row_at(3).output_start);
    }

    #[test]
    fn osc_133_d_stamps_exit_status_onto_prompt_row() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 1);
        bag.dispatch(b"133;A");
        // Cursor moves with output; D arrives on a later row but the exit
        // status must land on the prompt's row.
        bag.move_cursor(5, 3);
        bag.dispatch(b"133;D;42");
        assert_eq!(bag.row_at(1).exit_status, Some(42));
        assert_eq!(bag.row_at(3).exit_status, None);
    }

    #[test]
    fn osc_133_d_defaults_exit_to_zero_when_missing() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 0);
        bag.dispatch(b"133;A");
        bag.dispatch(b"133;D");
        assert_eq!(bag.row_at(0).exit_status, Some(0));
    }

    #[test]
    fn osc_133_d_ignores_non_numeric_exit() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 0);
        bag.dispatch(b"133;A");
        // A shell that omits the numeric status (e.g. emits D;aid=xyz)
        // still marks "command finished" — we pick success by default
        // rather than painting every prompt red.
        bag.dispatch(b"133;D;not-a-number");
        assert_eq!(bag.row_at(0).exit_status, Some(0));
    }

    #[test]
    fn osc_133_d_without_prior_a_is_silent() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(5, 2);
        bag.dispatch(b"133;D;1");
        // No A preceded → no row to stamp. Must not accidentally blow up
        // or mark the current-cursor row.
        for screen_row in 0..bag.viewport.rows {
            assert_eq!(bag.row_at(screen_row).exit_status, None);
        }
    }

    #[test]
    fn osc_133_a_overwrites_previous_pending_prompt() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 0);
        bag.dispatch(b"133;A");
        bag.move_cursor(0, 2);
        bag.dispatch(b"133;A");
        // A-without-D sequences are common when shell integration is
        // mid-transition: the second A should take over as the target of
        // the next D, and the first row keeps its mark but no exit code.
        bag.dispatch(b"133;D;7");
        assert_eq!(bag.row_at(0).exit_status, None);
        assert_eq!(bag.row_at(2).exit_status, Some(7));
        assert!(bag.row_at(0).prompt_start);
        assert!(bag.row_at(2).prompt_start);
    }

    #[test]
    fn osc_133_ignores_extra_key_value_args() {
        // iTerm2-style payloads include `aid=…`, `cl=…`, etc. We ignore
        // them rather than reject, matching how other terminals behave.
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 1);
        bag.dispatch(b"133;A;aid=abc;cl=m");
        assert!(bag.row_at(1).prompt_start);
        assert_eq!(bag.prompt_row, Some(1));
    }

    #[test]
    fn osc_133_unknown_kind_is_silent() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 1);
        bag.dispatch(b"133;Z");
        assert!(!bag.row_at(1).prompt_start);
        assert!(!bag.row_at(1).output_start);
    }

    #[test]
    fn osc_133_a_clears_stale_exit_status_on_recycled_row() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 0);
        bag.dispatch(b"133;A");
        bag.dispatch(b"133;D;5");
        // Same row later becomes a fresh prompt (e.g. in-place redraw).
        bag.move_cursor(0, 0);
        bag.dispatch(b"133;A");
        assert_eq!(bag.row_at(0).exit_status, None);
    }
}

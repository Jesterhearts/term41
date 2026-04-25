use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clip41::Clipboard;
use clip41::ClipboardKind;
use config41::ClipboardPermissions;
use config41::ColorPalette;
use config41::PermissionPolicy;
use percent_encoding::percent_decode;

use crate::C1Mode;
use crate::CommandMeta;
use crate::Row;
use crate::ShellIntegrationPhase;
use crate::color;
use crate::conformance;
use crate::io::clipboard::ClipboardRequest;
use crate::screen::Screen;
use crate::screen::grid::Viewport;
use crate::screen::hyperlink::HyperlinkId;
use crate::screen::hyperlink::HyperlinkRegistry;

// -- OSC command numbers ------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum OscCommand {
    SetIconAndTitle = 0,
    SetTitle = 2,
    PaletteColor = 4,
    SetDirectory = 7,
    Hyperlink = 8,
    FgColor = 10,
    BgColor = 11,
    CursorColor = 12,
    Clipboard = 52,
    ResetPalette = 104,
    ResetFg = 110,
    ResetBg = 111,
    ResetCursorColor = 112,
    ShellIntegration = 133,
    VscodeShellIntegration = 633,
    Iterm2 = 1337,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedOscAction<'a> {
    Unsupported,
    AcceptedNoop,
    SetTitle(Option<&'a str>),
    SetDirectory(DirectoryAction),
    SetHyperlink(HyperlinkAction<'a>),
    PaletteColorQuery { index: u8, index_text: &'a str },
    ForegroundColorQuery,
    BackgroundColorQuery,
    CursorColorQuery,
    Clipboard(ClipboardAction),
    ShellIntegration(ShellIntegrationAction),
    VscodeShellIntegration(VscodeShellIntegrationAction),
    ReportItermCellSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectoryAction {
    Clear,
    Set(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HyperlinkAction<'a> {
    Clear,
    Open { id: Option<&'a str>, uri: &'a str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardAction {
    Read {
        kind: ClipboardKind,
        response_selector: Vec<u8>,
    },
    Write {
        kinds: Vec<ClipboardKind>,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellIntegrationAction {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandFinished { exit: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VscodeShellIntegrationAction {
    Lifecycle(ShellIntegrationAction),
    SetDirectory(DirectoryAction),
    SetCommandLine(String),
}

impl TryFrom<u16> for OscCommand {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::SetIconAndTitle),
            2 => Ok(Self::SetTitle),
            4 => Ok(Self::PaletteColor),
            7 => Ok(Self::SetDirectory),
            8 => Ok(Self::Hyperlink),
            10 => Ok(Self::FgColor),
            11 => Ok(Self::BgColor),
            12 => Ok(Self::CursorColor),
            52 => Ok(Self::Clipboard),
            104 => Ok(Self::ResetPalette),
            110 => Ok(Self::ResetFg),
            111 => Ok(Self::ResetBg),
            112 => Ok(Self::ResetCursorColor),
            133 => Ok(Self::ShellIntegration),
            633 => Ok(Self::VscodeShellIntegration),
            1337 => Ok(Self::Iterm2),
            _ => Err(()),
        }
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
#[bon::builder]
pub(super) fn handle_osc(
    payload: &[u8],
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
    clipboard_requests: &mut Vec<ClipboardRequest>,
    clipboard_permissions: &ClipboardPermissions,
    c1_mode: C1Mode,
    current_directory: &mut Option<PathBuf>,
    hyperlinks: &mut HyperlinkRegistry,
    active_screen: &mut Screen,
    viewport: &Viewport,
    current_title: &mut Option<String>,
    /// Absolute row index of the most recent OSC 133 `A` (prompt start).
    /// An `OSC 133 D` stamps its exit code onto this row's exit_status so
    /// the mark sits next to the prompt, not the end-of-output. `None`
    /// before the first prompt and after the prompt row scrolls off the
    /// front of scrollback.
    current_prompt_row: &mut Option<u64>,
    /// Current OSC 133 phase, used as a compatibility hint.
    shell_integration_phase: &mut ShellIntegrationPhase,
    /// Per-prompt metadata: command column (from B), output row (from C),
    /// and timestamps for duration calculation.
    command_metas: &mut HashMap<u64, CommandMeta>,
    palette: &ColorPalette,
    cell_width: u32,
    cell_height: u32,
) {
    let action = parse_osc(payload);
    apply_parsed_osc()
        .action(action)
        .clipboard(clipboard)
        .pending_output(pending_output)
        .clipboard_requests(clipboard_requests)
        .clipboard_permissions(clipboard_permissions)
        .c1_mode(c1_mode)
        .current_directory(current_directory)
        .hyperlinks(hyperlinks)
        .active_screen(active_screen)
        .viewport(viewport)
        .current_title(current_title)
        .current_prompt_row(current_prompt_row)
        .shell_integration_phase(shell_integration_phase)
        .command_metas(command_metas)
        .palette(palette)
        .cell_width(cell_width)
        .cell_height(cell_height)
        .call();
}

fn parse_osc(payload: &[u8]) -> ParsedOscAction<'_> {
    let (cmd_bytes, rest) = split_osc(payload);
    let Some(cmd): Option<u16> = std::str::from_utf8(cmd_bytes)
        .ok()
        .and_then(|s| s.parse().ok())
    else {
        return ParsedOscAction::Unsupported;
    };

    let Ok(cmd) = OscCommand::try_from(cmd) else {
        return ParsedOscAction::Unsupported;
    };

    match cmd {
        OscCommand::SetIconAndTitle | OscCommand::SetTitle => parse_osc_title(rest),
        OscCommand::SetDirectory => parse_osc_directory(rest),
        OscCommand::Hyperlink => parse_osc_hyperlink(rest),
        OscCommand::PaletteColor => parse_osc_palette_query(rest),
        OscCommand::FgColor => parse_osc_color_query(rest, ParsedOscAction::ForegroundColorQuery),
        OscCommand::BgColor => parse_osc_color_query(rest, ParsedOscAction::BackgroundColorQuery),
        OscCommand::CursorColor => parse_osc_color_query(rest, ParsedOscAction::CursorColorQuery),
        OscCommand::Clipboard => parse_osc_clipboard(rest),
        OscCommand::ResetPalette
        | OscCommand::ResetFg
        | OscCommand::ResetBg
        | OscCommand::ResetCursorColor => ParsedOscAction::AcceptedNoop,
        OscCommand::ShellIntegration => parse_osc_133(rest),
        OscCommand::VscodeShellIntegration => parse_osc_633(rest),
        OscCommand::Iterm2 if rest.starts_with(b"ReportCellSize") => {
            ParsedOscAction::ReportItermCellSize
        }
        OscCommand::Iterm2 => ParsedOscAction::AcceptedNoop,
    }
}

fn parse_osc_title(rest: &[u8]) -> ParsedOscAction<'_> {
    if rest.is_empty() {
        return ParsedOscAction::SetTitle(None);
    }
    match std::str::from_utf8(rest) {
        Ok(title) => ParsedOscAction::SetTitle(Some(title)),
        Err(_) => ParsedOscAction::Unsupported,
    }
}

fn parse_osc_directory(rest: &[u8]) -> ParsedOscAction<'_> {
    parse_file_uri_directory(rest)
        .map(ParsedOscAction::SetDirectory)
        .unwrap_or(ParsedOscAction::Unsupported)
}

fn parse_file_uri_directory(value: &[u8]) -> Option<DirectoryAction> {
    if value.is_empty() {
        return Some(DirectoryAction::Clear);
    }

    let uri = std::str::from_utf8(value).ok()?;
    let rest = uri.strip_prefix("file://")?;
    let path_start = rest.find('/').unwrap_or(rest.len());
    let encoded_path = &rest[path_start..];
    if encoded_path.is_empty() {
        return None;
    }

    let decoded = percent_decode(encoded_path.as_bytes()).collect::<Vec<u8>>();
    let path = std::str::from_utf8(&decoded).ok()?;
    Some(DirectoryAction::Set(PathBuf::from(path)))
}

fn parse_absolute_or_file_directory(value: &[u8]) -> Option<DirectoryAction> {
    if value.is_empty() {
        return Some(DirectoryAction::Clear);
    }

    let path = std::str::from_utf8(value).ok()?;
    if path.starts_with("file://") {
        return parse_file_uri_directory(value);
    }
    if !Path::new(path).is_absolute() {
        return None;
    }
    Some(DirectoryAction::Set(PathBuf::from(path)))
}

fn parse_osc_hyperlink(rest: &[u8]) -> ParsedOscAction<'_> {
    let (params, uri) = split_osc(rest);

    if uri.is_empty() {
        return ParsedOscAction::SetHyperlink(HyperlinkAction::Clear);
    }

    let Ok(uri) = std::str::from_utf8(uri) else {
        return ParsedOscAction::SetHyperlink(HyperlinkAction::Clear);
    };

    let id = params.split(|&b| b == b':').find_map(|kv| {
        let (key, value) = split_key_value(kv)?;
        (key == b"id").then(|| std::str::from_utf8(value).ok())?
    });

    ParsedOscAction::SetHyperlink(HyperlinkAction::Open { id, uri })
}

fn parse_osc_palette_query(rest: &[u8]) -> ParsedOscAction<'_> {
    let (idx_bytes, query) = split_osc(rest);
    if query != b"?" {
        return ParsedOscAction::Unsupported;
    }
    let Ok(index_text) = std::str::from_utf8(idx_bytes) else {
        return ParsedOscAction::Unsupported;
    };
    let Ok(index) = index_text.parse::<u8>() else {
        return ParsedOscAction::Unsupported;
    };
    ParsedOscAction::PaletteColorQuery { index, index_text }
}

fn parse_osc_color_query<'a>(
    rest: &[u8],
    query_action: ParsedOscAction<'a>,
) -> ParsedOscAction<'a> {
    if rest == b"?" {
        query_action
    } else {
        ParsedOscAction::Unsupported
    }
}

fn parse_osc_clipboard(rest: &[u8]) -> ParsedOscAction<'_> {
    let (pc, pd) = split_osc(rest);
    let kinds = resolve_selectors(pc);

    if pd == b"?" {
        let Some(&kind) = kinds.first() else {
            return ParsedOscAction::Unsupported;
        };
        let response_selector = if pc.is_empty() {
            b"c".to_vec()
        } else {
            pc.to_vec()
        };
        return ParsedOscAction::Clipboard(ClipboardAction::Read {
            kind,
            response_selector,
        });
    }

    let Some(decoded) = decode_osc52(pd) else {
        return ParsedOscAction::Unsupported;
    };
    let Ok(text) = std::str::from_utf8(&decoded) else {
        return ParsedOscAction::Unsupported;
    };
    if kinds.is_empty() {
        return ParsedOscAction::Unsupported;
    }
    ParsedOscAction::Clipboard(ClipboardAction::Write {
        kinds,
        text: text.to_owned(),
    })
}

fn parse_osc_133(rest: &[u8]) -> ParsedOscAction<'_> {
    let (kind, args) = split_osc(rest);
    parse_shell_integration_lifecycle(kind, args)
        .map(ParsedOscAction::ShellIntegration)
        .unwrap_or(ParsedOscAction::Unsupported)
}

fn parse_osc_633(rest: &[u8]) -> ParsedOscAction<'_> {
    let (kind, args) = split_osc(rest);
    if let Some(action) = parse_shell_integration_lifecycle(kind, args) {
        return ParsedOscAction::VscodeShellIntegration(VscodeShellIntegrationAction::Lifecycle(
            action,
        ));
    }

    match kind {
        b"P" => parse_osc_633_property(args)
            .map(ParsedOscAction::VscodeShellIntegration)
            .unwrap_or(ParsedOscAction::Unsupported),
        b"E" => parse_osc_633_command_line(args)
            .map(|command_line| {
                ParsedOscAction::VscodeShellIntegration(
                    VscodeShellIntegrationAction::SetCommandLine(command_line),
                )
            })
            .unwrap_or(ParsedOscAction::Unsupported),
        _ => ParsedOscAction::Unsupported,
    }
}

fn parse_shell_integration_lifecycle(
    kind: &[u8],
    args: &[u8],
) -> Option<ShellIntegrationAction> {
    match kind {
        b"A" => Some(ShellIntegrationAction::PromptStart),
        b"B" => Some(ShellIntegrationAction::CommandStart),
        b"C" => Some(ShellIntegrationAction::OutputStart),
        b"D" => Some(ShellIntegrationAction::CommandFinished {
            exit: parse_shell_integration_exit(args),
        }),
        _ => None,
    }
}

fn parse_osc_633_property(args: &[u8]) -> Option<VscodeShellIntegrationAction> {
    let (key, value) = split_key_value(args)?;
    if key != b"Cwd" {
        return None;
    }
    parse_absolute_or_file_directory(value).map(VscodeShellIntegrationAction::SetDirectory)
}

fn parse_osc_633_command_line(args: &[u8]) -> Option<String> {
    let (command_line, _) = split_osc(args);
    decode_osc_633_command_line(command_line)
}

fn split_key_value(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = bytes.iter().position(|&b| b == b'=')?;
    Some((&bytes[..i], &bytes[i + 1..]))
}

#[bon::builder]
fn apply_parsed_osc(
    action: ParsedOscAction<'_>,
    clipboard: &mut Clipboard,
    pending_output: &mut Vec<u8>,
    clipboard_requests: &mut Vec<ClipboardRequest>,
    clipboard_permissions: &ClipboardPermissions,
    c1_mode: C1Mode,
    current_directory: &mut Option<PathBuf>,
    hyperlinks: &mut HyperlinkRegistry,
    active_screen: &mut Screen,
    viewport: &Viewport,
    current_title: &mut Option<String>,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    command_metas: &mut HashMap<u64, CommandMeta>,
    palette: &ColorPalette,
    cell_width: u32,
    cell_height: u32,
) {
    match action {
        ParsedOscAction::Unsupported | ParsedOscAction::AcceptedNoop => {}
        ParsedOscAction::SetTitle(title) => apply_osc_title(title, current_title),
        ParsedOscAction::SetDirectory(action) => apply_directory(action, current_directory),
        ParsedOscAction::SetHyperlink(action) => {
            apply_hyperlink(action, hyperlinks, &mut active_screen.current_hyperlink);
        }
        ParsedOscAction::PaletteColorQuery { index, index_text } => {
            let c = color::palette_color(palette, index);
            let reply = rgb_reply(c.red, c.green, c.blue);
            conformance::write_osc(
                pending_output,
                c1_mode,
                format_args!("4;{index_text};{reply}"),
            );
        }
        ParsedOscAction::ForegroundColorQuery => {
            write_color_query_reply(
                pending_output,
                c1_mode,
                OscCommand::FgColor as u8,
                palette.fg,
            );
        }
        ParsedOscAction::BackgroundColorQuery => {
            write_color_query_reply(
                pending_output,
                c1_mode,
                OscCommand::BgColor as u8,
                palette.bg,
            );
        }
        ParsedOscAction::CursorColorQuery => {
            let c = palette.cursor.unwrap_or(palette.fg);
            write_color_query_reply(pending_output, c1_mode, OscCommand::CursorColor as u8, c);
        }
        ParsedOscAction::Clipboard(action) => apply_clipboard_action(
            action,
            clipboard,
            c1_mode,
            pending_output,
            clipboard_requests,
            clipboard_permissions,
        ),
        ParsedOscAction::ShellIntegration(action) => apply_shell_integration_action(
            action,
            active_screen,
            viewport,
            current_prompt_row,
            shell_integration_phase,
            command_metas,
        ),
        ParsedOscAction::VscodeShellIntegration(action) => apply_vscode_shell_integration_action(
            action,
            current_directory,
            active_screen,
            viewport,
            current_prompt_row,
            shell_integration_phase,
            command_metas,
        ),
        ParsedOscAction::ReportItermCellSize => {
            conformance::write_osc(
                pending_output,
                c1_mode,
                format_args!("1337;ReportCellSize={};{}", cell_height, cell_width),
            );
        }
    }
}

fn apply_osc_title(
    title: Option<&str>,
    current_title: &mut Option<String>,
) {
    *current_title = title.map(ToOwned::to_owned);
}

fn apply_directory(
    action: DirectoryAction,
    current_directory: &mut Option<PathBuf>,
) {
    match action {
        DirectoryAction::Clear => *current_directory = None,
        DirectoryAction::Set(path) => *current_directory = Some(path),
    }
}

fn apply_hyperlink(
    action: HyperlinkAction<'_>,
    registry: &mut HyperlinkRegistry,
    current: &mut Option<HyperlinkId>,
) {
    match action {
        HyperlinkAction::Clear => *current = None,
        HyperlinkAction::Open { id, uri } => *current = Some(registry.intern(id, uri)),
    }
}

fn write_color_query_reply(
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    cmd: u8,
    current: palette::Srgb<u8>,
) {
    let reply = rgb_reply(current.red, current.green, current.blue);
    conformance::write_osc(pending_output, c1_mode, format_args!("{cmd};{reply}"));
}

fn apply_clipboard_action(
    action: ClipboardAction,
    clipboard: &mut Clipboard,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
    clipboard_requests: &mut Vec<ClipboardRequest>,
    clipboard_permissions: &ClipboardPermissions,
) {
    match action {
        ClipboardAction::Read {
            kind,
            response_selector,
        } => match clipboard_permissions.read {
            PermissionPolicy::Allow => {
                pending_output.extend(crate::io::clipboard::osc52_read_response(
                    clipboard,
                    kind,
                    &response_selector,
                    c1_mode,
                ));
            }
            PermissionPolicy::Ask => clipboard_requests.push(ClipboardRequest::Read {
                kind,
                response_selector,
                c1_mode,
            }),
            PermissionPolicy::Deny => {}
        },
        ClipboardAction::Write { kinds, text } => match clipboard_permissions.write {
            PermissionPolicy::Allow => {
                for kind in kinds {
                    clipboard.set(kind, &text);
                }
            }
            PermissionPolicy::Ask => {
                clipboard_requests.push(ClipboardRequest::Write { kinds, text })
            }
            PermissionPolicy::Deny => {}
        },
    }
}

fn apply_vscode_shell_integration_action(
    action: VscodeShellIntegrationAction,
    current_directory: &mut Option<PathBuf>,
    screen: &mut Screen,
    viewport: &Viewport,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    match action {
        VscodeShellIntegrationAction::Lifecycle(action) => apply_shell_integration_action(
            action,
            screen,
            viewport,
            current_prompt_row,
            shell_integration_phase,
            command_metas,
        ),
        VscodeShellIntegrationAction::SetDirectory(action) => {
            apply_directory(action, current_directory);
        }
        VscodeShellIntegrationAction::SetCommandLine(command_line) => {
            apply_command_line_metadata(command_line, *current_prompt_row, command_metas);
        }
    }
}

fn apply_command_line_metadata(
    command_line: String,
    current_prompt_row: Option<u64>,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    let Some(prompt_abs) = current_prompt_row else {
        return;
    };
    let Some(meta) = command_metas.get_mut(&prompt_abs) else {
        return;
    };
    meta.untrusted_command_line = Some(command_line);
}

fn apply_shell_integration_action(
    action: ShellIntegrationAction,
    screen: &mut Screen,
    viewport: &Viewport,
    current_prompt_row: &mut Option<u64>,
    shell_integration_phase: &mut ShellIntegrationPhase,
    command_metas: &mut HashMap<u64, CommandMeta>,
) {
    match action {
        ShellIntegrationAction::PromptStart => {
            let abs = mark_current_row(screen, viewport, |row| {
                row.prompt_start = true;
                // A fresh prompt invalidates any lingering exit_status from
                // a prior occupant of this row (e.g. a recycled scrollback
                // slot). The shell hasn't even shown the prompt yet.
                row.exit_status = None;
            });
            *current_prompt_row = Some(abs);
            *shell_integration_phase = ShellIntegrationPhase::Prompt;
            // Seed the metadata entry so B/C/D/E can fill it in.
            command_metas.insert(abs, CommandMeta::new());
        }
        ShellIntegrationAction::CommandStart => {
            *shell_integration_phase = ShellIntegrationPhase::Command;
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
        ShellIntegrationAction::OutputStart => {
            *shell_integration_phase = ShellIntegrationPhase::Output;
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
        ShellIntegrationAction::CommandFinished { exit } => {
            *shell_integration_phase = ShellIntegrationPhase::Finished;
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
    }
}

/// Run `apply` on the row the cursor currently occupies and return that
/// row's absolute index (stable under scrollback trimming). Factored out
/// because every OSC 133 kind that stores a mark does the same lookup.
fn mark_current_row(
    screen: &mut Screen,
    viewport: &Viewport,
    apply: impl FnOnce(&mut Row),
) -> u64 {
    let local = crate::screen::active_row_index(screen, viewport);
    apply(&mut screen.grid.rows[local]);
    (screen.grid.total_popped + local) as u64
}

/// Return the absolute row index the cursor currently sits on, without
/// mutating the row. Used by OSC 133 B to record the command start row.
fn current_absolute_row(
    screen: &Screen,
    viewport: &Viewport,
) -> u64 {
    let local = crate::screen::active_row_index(screen, viewport);
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
fn parse_shell_integration_exit(args: &[u8]) -> i32 {
    let (first, _) = split_osc(args);
    std::str::from_utf8(first)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0)
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

fn decode_osc_633_command_line(command_line: &[u8]) -> Option<String> {
    let mut decoded = Vec::with_capacity(command_line.len());
    let mut i = 0;
    while i < command_line.len() {
        if command_line[i] != b'\\' {
            decoded.push(command_line[i]);
            i += 1;
            continue;
        }

        let escaped = *command_line.get(i + 1)?;
        match escaped {
            b'\\' => {
                decoded.push(b'\\');
                i += 2;
            }
            b'x' | b'X' => {
                let hi = *command_line.get(i + 2)?;
                let lo = *command_line.get(i + 3)?;
                decoded.push((hex_nibble(hi)? << 4) | hex_nibble(lo)?);
                i += 4;
            }
            _ => return None,
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use config41::default_bg;
    use config41::default_fg;

    use super::*;

    struct Bag {
        clipboard: Clipboard,
        pending: Vec<u8>,
        cwd: Option<PathBuf>,
        registry: HyperlinkRegistry,
        screen: Screen,
        viewport: Viewport,
        title: Option<String>,
        prompt_row: Option<u64>,
        shell_integration_phase: ShellIntegrationPhase,
        command_metas: HashMap<u64, CommandMeta>,
        palette: ColorPalette,
        clipboard_requests: Vec<ClipboardRequest>,
        clipboard_permissions: ClipboardPermissions,
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
                screen: Screen::new(
                    cols,
                    rows,
                    100,
                    default_fg(),
                    default_bg(),
                    default_fg(),
                    default_bg(),
                ),
                viewport: Viewport { rows, cols, top: 0 },
                title: None,
                prompt_row: None,
                shell_integration_phase: ShellIntegrationPhase::None,
                command_metas: HashMap::new(),
                palette: ColorPalette::default(),
                clipboard_requests: Vec::new(),
                clipboard_permissions: ClipboardPermissions {
                    read: PermissionPolicy::Allow,
                    write: PermissionPolicy::Allow,
                },
            }
        }

        fn with_clipboard_permissions(
            mut self,
            clipboard_permissions: ClipboardPermissions,
        ) -> Self {
            self.clipboard_permissions = clipboard_permissions;
            self
        }

        fn current_link(&self) -> Option<HyperlinkId> {
            self.screen.current_hyperlink
        }

        fn dispatch(
            &mut self,
            payload: &[u8],
        ) {
            handle_osc()
                .payload(payload)
                .clipboard(&mut self.clipboard)
                .pending_output(&mut self.pending)
                .clipboard_requests(&mut self.clipboard_requests)
                .clipboard_permissions(&self.clipboard_permissions)
                .c1_mode(C1Mode::SevenBit)
                .current_directory(&mut self.cwd)
                .hyperlinks(&mut self.registry)
                .active_screen(&mut self.screen)
                .viewport(&self.viewport)
                .current_title(&mut self.title)
                .current_prompt_row(&mut self.prompt_row)
                .shell_integration_phase(&mut self.shell_integration_phase)
                .command_metas(&mut self.command_metas)
                .palette(&self.palette)
                .cell_width(8)
                .cell_height(16)
                .call();
        }
    }

    #[test]
    fn osc_parse_maps_title_semantically() {
        assert!(matches!(
            parse_osc(b"2;term41"),
            ParsedOscAction::SetTitle(Some("term41"))
        ));
    }

    #[test]
    fn osc_parse_maps_clipboard_query_semantically() {
        assert!(matches!(
            parse_osc(b"52;p;?"),
            ParsedOscAction::Clipboard(ClipboardAction::Read {
                kind: ClipboardKind::Primary,
                response_selector,
            }) if response_selector == b"p"
        ));
    }

    #[test]
    fn osc_parse_maps_hyperlink_semantically() {
        assert!(matches!(
            parse_osc(b"8;id=docs;https://example.test"),
            ParsedOscAction::SetHyperlink(HyperlinkAction::Open {
                id: Some("docs"),
                uri: "https://example.test",
            })
        ));
    }

    #[test]
    fn osc_parse_maps_shell_lifecycle_semantically() {
        assert!(matches!(
            parse_osc(b"133;D;42"),
            ParsedOscAction::ShellIntegration(ShellIntegrationAction::CommandFinished { exit: 42 })
        ));
    }

    #[test]
    fn osc_parse_maps_vscode_cwd_semantically() {
        assert!(matches!(
            parse_osc(b"633;P;Cwd=/tmp"),
            ParsedOscAction::VscodeShellIntegration(VscodeShellIntegrationAction::SetDirectory(
                DirectoryAction::Set(path),
            )) if path.as_path() == Path::new("/tmp")
        ));
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
    fn osc_52_ask_write_defers_clipboard_mutation() {
        let mut bag = Bag::new().with_clipboard_permissions(ClipboardPermissions {
            read: PermissionPolicy::Allow,
            write: PermissionPolicy::Ask,
        });
        bag.dispatch(b"52;c;aGVsbG8=");

        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("")
        );
        assert_eq!(
            bag.clipboard_requests,
            vec![ClipboardRequest::Write {
                kinds: vec![ClipboardKind::Clipboard],
                text: "hello".to_string(),
            }]
        );
    }

    #[test]
    fn osc_52_ask_read_defers_clipboard_query() {
        let mut bag = Bag::new().with_clipboard_permissions(ClipboardPermissions {
            read: PermissionPolicy::Ask,
            write: PermissionPolicy::Allow,
        });
        bag.clipboard.set(ClipboardKind::Clipboard, "hi");
        bag.dispatch(b"52;;?");

        assert!(bag.pending.is_empty());
        assert_eq!(
            bag.clipboard_requests,
            vec![ClipboardRequest::Read {
                kind: ClipboardKind::Clipboard,
                response_selector: b"c".to_vec(),
                c1_mode: C1Mode::SevenBit,
            }]
        );
    }

    #[test]
    fn osc_52_deny_blocks_clipboard_access() {
        let mut bag = Bag::new().with_clipboard_permissions(ClipboardPermissions {
            read: PermissionPolicy::Deny,
            write: PermissionPolicy::Deny,
        });
        bag.clipboard.set(ClipboardKind::Clipboard, "old");
        bag.dispatch(b"52;c;aGVsbG8=");
        bag.dispatch(b"52;c;?");

        assert_eq!(
            bag.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("old")
        );
        assert!(bag.pending.is_empty());
        assert!(bag.clipboard_requests.is_empty());
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
        ) -> &Row {
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

    // ---- OSC 633 — VS Code shell integration ----

    #[test]
    fn osc_633_a_b_c_d_alias_osc_133_lifecycle() {
        let mut bag = Bag::with_screen(10, 4);
        bag.move_cursor(0, 1);
        bag.dispatch(b"633;A");
        bag.move_cursor(2, 1);
        bag.dispatch(b"633;B");
        bag.move_cursor(0, 2);
        bag.dispatch(b"633;C");
        bag.dispatch(b"633;D;12");

        assert!(bag.row_at(1).prompt_start);
        assert!(bag.row_at(2).output_start);
        assert_eq!(bag.row_at(1).exit_status, Some(12));
        let meta = bag.command_metas.get(&1).expect("prompt metadata");
        assert_eq!(meta.command_col, Some(2));
        assert_eq!(meta.command_row, Some(1));
        assert_eq!(meta.output_row, Some(2));
        assert!(meta.started_at.is_some());
        assert!(meta.finished_at.is_some());
    }

    #[test]
    fn osc_633_cwd_property_accepts_absolute_local_path() {
        let mut bag = Bag::new();
        bag.dispatch(b"633;P;Cwd=/tmp/project");
        assert_eq!(bag.cwd.as_deref(), Some(Path::new("/tmp/project")));
    }

    #[test]
    fn osc_633_cwd_property_accepts_file_uri_like_osc_7() {
        let mut bag = Bag::new();
        bag.dispatch(b"633;P;Cwd=file://localhost/tmp/project%20space");
        assert_eq!(bag.cwd.as_deref(), Some(Path::new("/tmp/project space")));
    }

    #[test]
    fn osc_633_cwd_property_rejects_relative_path() {
        let mut bag = Bag::new();
        bag.dispatch(b"633;P;Cwd=relative/project");
        assert_eq!(bag.cwd, None);
    }

    #[test]
    fn osc_633_command_line_is_recorded_as_untrusted_metadata() {
        let mut bag = Bag::with_screen(10, 4);
        bag.dispatch(b"633;A");
        bag.dispatch(b"633;E;cargo\\x20test;nonce-123");

        let meta = bag.command_metas.get(&0).expect("prompt metadata");
        assert_eq!(meta.untrusted_command_line.as_deref(), Some("cargo test"));
    }

    #[test]
    fn osc_633_command_line_decodes_escaped_ascii_and_backslash() {
        let mut bag = Bag::with_screen(10, 4);
        bag.dispatch(b"633;A");
        bag.dispatch(b"633;E;printf\\x20foo\\x3Bbar\\\\baz\\x0Aline2");

        let meta = bag.command_metas.get(&0).expect("prompt metadata");
        assert_eq!(
            meta.untrusted_command_line.as_deref(),
            Some("printf foo;bar\\baz\nline2")
        );
    }

    #[test]
    fn osc_633_command_line_invalid_escape_is_ignored() {
        let mut bag = Bag::with_screen(10, 4);
        bag.dispatch(b"633;A");
        bag.dispatch(b"633;E;cargo\\qtest");

        let meta = bag.command_metas.get(&0).expect("prompt metadata");
        assert_eq!(meta.untrusted_command_line, None);
    }

    #[test]
    fn osc_633_command_line_without_prompt_is_silent() {
        let mut bag = Bag::new();
        bag.dispatch(b"633;E;cargo test;nonce-123");
        assert!(bag.command_metas.is_empty());
    }
}

#[cfg(test)]
mod process_tests {
    use crate::test_support::TestTerm;

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
    fn osc_7_updates_terminal_cwd() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]7;file://localhost/tmp/work\x1b\\");
        assert_eq!(
            term.metadata.current_directory.as_deref(),
            Some(std::path::Path::new("/tmp/work"))
        );
    }

    #[test]
    fn osc_8_attaches_link_to_subsequent_cells() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\after");
        assert_eq!(term.hyperlink_at(0, 0), Some("https://example.com"));
        assert_eq!(term.hyperlink_at(0, 3), Some("https://example.com"));
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

    #[test]
    fn osc_2_updates_terminal_title() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]2;build ok\x1b\\");
        assert_eq!(term.metadata.current_title.as_deref(), Some("build ok"));
    }

    #[test]
    fn osc_0_updates_terminal_title() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]0;hi\x1b\\");
        assert_eq!(term.metadata.current_title.as_deref(), Some("hi"));
    }

    #[test]
    fn osc_133_stamps_exit_status_onto_prompt_row_through_process() {
        let mut term = TestTerm::new(10, 6, 100, 16, 8);
        emit_prompt(&mut term, "$ ls", 1, 0);
        let prompt_row = &term.active.grid.rows[0];
        assert!(prompt_row.prompt_start);
        assert_eq!(prompt_row.exit_status, Some(0));
    }

    #[test]
    fn osc_633_marks_prompt_lifecycle_through_process() {
        let mut term = TestTerm::new(10, 6, 100, 16, 8);
        term.process(b"\x1b]633;A\x1b\\");
        term.process(b"$ ls");
        term.process(b"\x1b]633;B\x1b\\");
        term.process(b"\n\x1b]633;C\x1b\\");
        term.process(b"out\n");
        term.process(b"\x1b]633;D;7\x1b\\");

        let prompt_row = &term.active.grid.rows[0];
        assert!(prompt_row.prompt_start);
        assert_eq!(prompt_row.exit_status, Some(7));
        assert!(term.active.grid.rows[1].output_start);
    }

    #[test]
    fn osc_633_cwd_property_updates_terminal_cwd() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        term.process(b"\x1b]633;P;Cwd=/tmp/work\x1b\\");
        assert_eq!(
            term.metadata.current_directory.as_deref(),
            Some(std::path::Path::new("/tmp/work"))
        );
    }

    #[test]
    fn osc_133_exit_status_survives_scrollback_pop() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        emit_prompt(&mut term, "$ first", 2, 0);
        emit_prompt(&mut term, "$ second", 2, 1);
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
    fn prompt_marks_ride_reflow_shrink_then_grow() {
        let mut term = TestTerm::new(20, 6, 100, 16, 8);
        term.process(b"\x1b]133;A\x1b\\");
        term.process(b"$ this is a long prompt line");
        term.process(b"\x1b]133;B\x1b\\\n");
        term.process(b"\x1b]133;D;0\x1b\\");

        term.resize(8, 6);
        term.resize(20, 6);

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
        assert_eq!(prompt_rows[0].1.exit_status, Some(0));
    }

    #[test]
    fn prompt_marks_do_not_duplicate_on_continuation_rows() {
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
